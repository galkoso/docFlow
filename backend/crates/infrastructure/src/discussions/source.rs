use super::*;
use crate::provider_backoff::{self, ConnectionKey};
use crate::review_files::PgReviewBookAuthorization;
use specspace_application::{
    book::BookError,
    comparison_file::{ComparisonFileReader, FileReadError, discussion::AnchorReadError},
    comparison_reader::ComparisonReadError,
};
use specspace_git_adapter::GitError;
use std::sync::Arc;

struct SourceStop {
    error: DiscussionStoreError,
    code: &'static str,
    delay: Option<u32>,
}

impl SourceStop {
    fn from_error(error: AnchorReadError) -> Self {
        use FileReadError::*;
        match error {
            AnchorReadError::Selection(_) => Self {
                error: DiscussionStoreError::Invalid,
                code: "INVALID_SELECTION",
                delay: None,
            },
            AnchorReadError::Source(error) => match error {
                Provider(GitError::RateLimited {
                    retry_after_seconds,
                })
                | Access(ComparisonReadError::Provider(GitError::RateLimited {
                    retry_after_seconds,
                }))
                | Access(ComparisonReadError::Source(BookError::Provider(
                    GitError::RateLimited {
                        retry_after_seconds,
                    },
                ))) => {
                    let delay = retry_after_seconds.unwrap_or(60).clamp(1, u32::MAX as u64) as u32;
                    Self {
                        error: DiscussionStoreError::ProviderRateLimited(delay),
                        code: "PROVIDER_RATE_LIMIT",
                        delay: Some(delay),
                    }
                }
                NotFound | Access(ComparisonReadError::NotFound) => Self {
                    error: DiscussionStoreError::NotFound,
                    code: "ACCESS_LOST",
                    delay: None,
                },
                Access(ComparisonReadError::ContextChanged) => Self {
                    error: DiscussionStoreError::Stale,
                    code: "CONTEXT_CHANGED",
                    delay: None,
                },
                Busy | Access(ComparisonReadError::Busy) => Self {
                    error: DiscussionStoreError::Capacity,
                    code: "CAPACITY",
                    delay: None,
                },
                TimedOut | Access(ComparisonReadError::TimedOut) => Self {
                    error: DiscussionStoreError::SourceUnavailable,
                    code: "TIMEOUT",
                    delay: None,
                },
                UnsupportedKind => Self {
                    error: DiscussionStoreError::Invalid,
                    code: "UNSUPPORTED_SELECTION",
                    delay: None,
                },
                NotReady => Self {
                    error: DiscussionStoreError::NotReady,
                    code: "SOURCE_NOT_READY",
                    delay: None,
                },
                InvalidEvidence | TooLarge | InvalidImage(_) => Self {
                    error: DiscussionStoreError::InvalidSource,
                    code: "INVALID_EVIDENCE",
                    delay: None,
                },
                _ => Self {
                    error: DiscussionStoreError::SourceUnavailable,
                    code: "SOURCE_UNAVAILABLE",
                    delay: None,
                },
            },
        }
    }
}
fn key(scope: &Scope) -> ConnectionKey<'_> {
    ConnectionKey {
        organization: scope.organization_id,
        repository: scope.repository_id,
        remote_id: &scope.remote_id,
        installation_id: &scope.installation_id,
    }
}

impl PgDiscussions {
    /// Admission commits before source IO; neither source work nor the final
    /// permission/context check can be supplied as browser evidence.
    pub async fn open_thread(
        &self,
        actor: Uuid,
        proposal: Uuid,
        request: OpenThreadRequest,
        reader: &ComparisonFileReader,
    ) -> Result<DiscussionRecorded, DiscussionStoreError> {
        let admission = match self.admit_open(actor, proposal, request).await? {
            OpenAdmissionResult::Recorded(receipt) => return Ok(receipt),
            OpenAdmissionResult::Ready(admission) => admission,
        };
        let (snapshot_id, anchor) = match &admission.request.origin {
            ThreadOrigin::Snapshot {
                snapshot_id,
                anchor,
                ..
            } if !matches!(anchor, SnapshotAnchor::Proposal {}) => (snapshot_id, anchor),
            _ => return self.complete_open(&admission, None).await,
        };
        let mut tx = self.transaction().await?;
        let current = scope(&mut tx, actor, proposal).await?;
        if let Some(receipt) = replay(&mut tx, &admission.identity).await? {
            tx.commit().await?;
            return Ok(receipt);
        }
        if current != admission.scope {
            return Err(DiscussionStoreError::Stale);
        }
        let delay = provider_backoff::remaining(&mut tx, &key(&current)).await?;
        tx.commit().await?;
        if let Some(delay) = delay {
            let stop = SourceStop {
                error: DiscussionStoreError::ProviderRateLimited(delay),
                code: "PROVIDER_BACKOFF",
                delay: None,
            };
            return self.stopped_result(&admission, stop).await;
        }
        enum Loaded {
            File(Box<VerifiedFileAnchor>),
            BookDocument(Box<VerifiedBookAnchor>),
        }
        let loaded = match anchor {
            SnapshotAnchor::File {
                file_id,
                side,
                selection,
            } => reader
                .load_discussion_anchor(
                    actor,
                    proposal,
                    *snapshot_id,
                    file_id,
                    *side,
                    selection.clone(),
                )
                .await
                .map(Box::new)
                .map(Loaded::File),
            SnapshotAnchor::BookDocument {
                entry_id,
                selection,
            } => reader
                .with_book_authorization(Arc::new(PgReviewBookAuthorization::new(
                    self.pool.clone(),
                )))
                .load_anchor(actor, proposal, *snapshot_id, entry_id, selection.clone())
                .await
                .map(Box::new)
                .map(Loaded::BookDocument),
            SnapshotAnchor::Proposal {} => {
                unreachable!("general anchors completed before source IO")
            }
        };
        let loaded = match loaded {
            Ok(proof) => proof,
            Err(error) => {
                let stop = SourceStop::from_error(error);
                return self.stopped_result(&admission, stop).await;
            }
        };
        let proof = match &loaded {
            Loaded::File(proof) => DiscussionSourceProof::File(proof),
            Loaded::BookDocument(proof) => DiscussionSourceProof::BookDocument(proof),
        };
        match self.complete_open(&admission, Some(proof)).await {
            Err(
                error @ (DiscussionStoreError::Forbidden
                | DiscussionStoreError::NotFound
                | DiscussionStoreError::Stale),
            ) => {
                let stop = SourceStop {
                    error,
                    code: "FINAL_AUTHORITY_CHANGED",
                    delay: None,
                };
                self.stopped_result(&admission, stop).await
            }
            result => result,
        }
    }

    async fn stopped_result(
        &self,
        admission: &OpenAdmission,
        stop: SourceStop,
    ) -> Result<DiscussionRecorded, DiscussionStoreError> {
        self.source_stopped(admission, &stop).await?;
        // An overlapping identical request may have completed while this read
        // failed. Prefer its exact receipt, with current write access checked.
        let mut tx = self.transaction().await?;
        let current = scope(
            &mut tx,
            admission.identity.actor,
            admission.identity.proposal,
        )
        .await?;
        if let Some(receipt) = replay(&mut tx, &admission.identity).await? {
            tx.commit().await?;
            return Ok(receipt);
        }
        if current != admission.scope {
            return Err(DiscussionStoreError::Stale);
        }
        tx.commit().await?;
        Err(stop.error)
    }

    async fn source_stopped(
        &self,
        admission: &OpenAdmission,
        stop: &SourceStop,
    ) -> Result<(), DiscussionStoreError> {
        let identity = &admission.identity;
        let mut tx = self.transaction().await?;
        // The durable, already-authorized admission proves the original scope.
        // Record access loss too, without requiring the revoked actor to retain
        // write permission and without exposing any previously read source.
        let inserted=sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,after_ref,result) VALUES($1,$2,$3,'DISCUSSION_SOURCE_STOPPED',$4,$5,$6,$7) ON CONFLICT(id) DO NOTHING")
            .bind(identity.attempt).bind(identity.organization).bind(identity.actor).bind(identity.proposal).bind(admission.snapshot.as_ref().map(|s|s.id.to_string())).bind(identity.request.to_string()).bind(stop.code).execute(&mut *tx).await?.rows_affected();
        if inserted == 1 {
            if let Some(delay) = stop.delay {
                provider_backoff::defer(&mut tx, &key(&admission.scope), delay).await?;
            }
        } else {
            let matches:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM audit_events WHERE id=$1 AND organization_id=$2 AND actor_id=$3 AND action='DISCUSSION_SOURCE_STOPPED' AND entity_id=$4 AND after_ref=$5 AND result=$6 AND before_ref IS NOT DISTINCT FROM $7)")
                .bind(identity.attempt).bind(identity.organization).bind(identity.actor).bind(identity.proposal).bind(identity.request.to_string()).bind(stop.code).bind(admission.snapshot.as_ref().map(|s|s.id.to_string())).fetch_one(&mut *tx).await?;
            if !matches {
                return Err(DiscussionStoreError::Unavailable);
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_rate_limits_keep_defaults_and_bounded_delays() {
        for (seconds, expected) in [(None, 60), (Some(0), 1), (Some(u64::MAX), u32::MAX)] {
            let stop = SourceStop::from_error(AnchorReadError::Source(FileReadError::Access(
                ComparisonReadError::Source(BookError::Provider(GitError::RateLimited {
                    retry_after_seconds: seconds,
                })),
            )));
            assert_eq!(
                stop.error,
                DiscussionStoreError::ProviderRateLimited(expected)
            );
            assert_eq!(stop.delay, Some(expected));
            assert_eq!(stop.code, "PROVIDER_RATE_LIMIT");
        }
    }
}
