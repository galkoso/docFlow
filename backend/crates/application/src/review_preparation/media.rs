//! Sealed media evidence for one exact candidate. No originals or derivative
//! pixels survive verification; only their hashes and bounded dimensions do.
use super::*;
use crate::review::RequiredMedia;
use serde::Serialize;
use sha2::{Digest, Sha256};
use specspace_document_engine::media;
use specspace_domain::RepositoryPath;
use specspace_git_adapter::blob;
use std::io::{self, BufWriter, Write};

pub const MEDIA_VALIDATION_VERSION: u32 = media::VALIDATION_VERSION;

#[derive(Serialize)]
pub struct VerifiedMedia {
    path: String,
    blob_oid: String,
    bytes: u64,
    source_sha256: String,
    display_sha256: String,
    display_width: u32,
    display_height: u32,
}
impl VerifiedMedia {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn blob_oid(&self) -> &str {
        &self.blob_oid
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub fn display_sha256(&self) -> &str {
        &self.display_sha256
    }
    pub fn display_dimensions(&self) -> (u32, u32) {
        (self.display_width, self.display_height)
    }
}
pub struct VerifiedReview {
    candidate: ReviewCandidate,
    media: Vec<VerifiedMedia>,
    fingerprint: String,
    evidence_digest: String,
}
impl VerifiedReview {
    pub fn candidate(&self) -> &ReviewCandidate {
        &self.candidate
    }
    pub fn media(&self) -> &[VerifiedMedia] {
        &self.media
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn evidence_digest(&self) -> &str {
        &self.evidence_digest
    }
    pub fn media_validation_version(&self) -> u32 {
        MEDIA_VALIDATION_VERSION
    }
}
impl ReviewPreparer {
    pub async fn verify_media(
        &self,
        candidate: ReviewCandidate,
    ) -> Result<VerifiedReview, ReviewPreparationError> {
        let permit = self
            .loads
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReviewPreparationError::Busy)?;
        tokio::time::timeout(self.timeout, self.verify_media_inner(candidate, permit))
            .await
            .map_err(|_| ReviewPreparationError::TimedOut)?
    }
    async fn verify_media_inner(
        &self,
        candidate: ReviewCandidate,
        mut permit: OwnedSemaphorePermit,
    ) -> Result<VerifiedReview, ReviewPreparationError> {
        let scope = &candidate.scope;
        let expectation = &candidate.expectation;
        self.check(scope, expectation).await?;
        let provider = self.checked_provider(scope, expectation).await?;
        let mut receipts = Vec::with_capacity(candidate.plan.required_media().len());
        for required in candidate.plan.required_media() {
            if !media::valid_path(&required.path) || required.bytes > media::MAX_ASSET_BYTES as u64
            {
                return Err(invalid(&required.path, "REVIEW_MEDIA_METADATA_INVALID"));
            }
            let path = RepositoryPath::parse(&required.path)
                .map_err(|_| invalid(&required.path, "REVIEW_MEDIA_METADATA_INVALID"))?;
            let result = provider
                .read_bytes(&expectation.head, &path, media::MAX_ASSET_BYTES)
                .await;
            self.check(scope, expectation).await?;
            let data = result.map_err(ReviewPreparationError::Provider)?;
            let required = required.clone();
            let (result, returned) =
                tokio::task::spawn_blocking(move || (verify_image(required, data), permit))
                    .await
                    .map_err(|_| ReviewPreparationError::ProcessingUnavailable)?;
            permit = returned;
            // Recheck before exposing either a receipt or a decoder diagnostic.
            self.check(scope, expectation).await?;
            receipts.push(result?);
        }
        let plan_fingerprint = candidate.plan.summary().fingerprint.clone();
        let (hashes, receipts, _permit) = tokio::task::spawn_blocking(move || {
            receipts.sort_by(|a, b| a.path.cmp(&b.path));
            let mut writer = BufWriter::new(HashWriter(Sha256::new()));
            let result = (|| {
                let identity = serde_json::to_vec(&(
                    "specspace-verified-review-content-v1",
                    &plan_fingerprint,
                    MEDIA_VALIDATION_VERSION,
                ))
                .map_err(|_| ReviewPreparationError::ProcessingUnavailable)?;
                let fingerprint = format!("{:x}", Sha256::digest(identity));
                serde_json::to_writer(
                    &mut writer,
                    &(
                        "specspace-verified-review-v1",
                        &plan_fingerprint,
                        MEDIA_VALIDATION_VERSION,
                        &receipts,
                    ),
                )
                .map_err(|_| ReviewPreparationError::ProcessingUnavailable)?;
                writer
                    .flush()
                    .map_err(|_| ReviewPreparationError::ProcessingUnavailable)?;
                let evidence_digest = format!(
                    "{:x}",
                    writer
                        .into_inner()
                        .map_err(|_| ReviewPreparationError::ProcessingUnavailable)?
                        .0
                        .finalize()
                );
                Ok((fingerprint, evidence_digest))
            })();
            (result, receipts, permit)
        })
        .await
        .map_err(|_| ReviewPreparationError::ProcessingUnavailable)?;
        // Fresh binding/ref checks follow the entire scan and final CPU work.
        self.refs(scope, expectation).await?;
        let (fingerprint, evidence_digest) = hashes?;
        Ok(VerifiedReview {
            candidate,
            media: receipts,
            fingerprint,
            evidence_digest,
        })
    }
}
fn verify_image(
    required: RequiredMedia,
    data: Vec<u8>,
) -> Result<VerifiedMedia, ReviewPreparationError> {
    if data.len() > media::MAX_ASSET_BYTES
        || data.len() as u64 != required.bytes
        || blob::oid(&data, required.blob_oid.len())
            .ok()
            .as_ref()
            .map(|oid| oid.as_str())
            != Some(required.blob_oid.as_str())
    {
        return Err(invalid(&required.path, "REVIEW_MEDIA_BLOB_MISMATCH"));
    }
    let source_sha256 = format!("{:x}", Sha256::digest(&data));
    let display =
        media::display(&required.path, &data).map_err(|code| invalid(&required.path, code))?;
    Ok(VerifiedMedia {
        path: required.path,
        blob_oid: required.blob_oid,
        bytes: required.bytes,
        source_sha256,
        display_sha256: format!("{:x}", Sha256::digest(&display.png)),
        display_width: display.width,
        display_height: display.height,
    })
}
fn invalid(path: &str, code: &'static str) -> ReviewPreparationError {
    ReviewPreparationError::InvalidMedia {
        path: path.to_owned(),
        code,
    }
}
struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
