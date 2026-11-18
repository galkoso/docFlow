//! Immutable PR projection intent. Construction is not authorization: the DB
//! journal must resolve current actor scope and the exact active sealed snapshot.
//! Retrying a stored intent reuses its exact metadata, never a newly rendered body.
use crate::{
    comparison_reader::RepositoryBinding,
    proposal::{LinkKind, ProposalLink, UiImpact},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use specspace_domain::BranchName;
use specspace_git_adapter::{
    CommitSha,
    pull_requests::{
        PullRequest, PullRequestContent, PullRequestNumber, PullRequestScope, PullRequestState,
    },
};
use url::{Host, Url};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq)]
pub struct PrSyncSource {
    pub organization: Uuid,
    pub proposal: Uuid,
    pub actor: Uuid,
    pub operation: Uuid,
    pub snapshot: Uuid,
    pub repository: RepositoryBinding,
    pub branch: BranchName,
    pub head: CommitSha,
    pub target: CommitSha,
    pub merge_base: CommitSha,
    pub proposal_version: i64,
    pub configuration_version: u64,
    pub policy_version: u64,
    pub fingerprint: String,
    pub evidence_digest: String,
    pub authority_digest: String,
}
/// Current projection data must be captured by the journal under the same
/// source/version locks. Missing question coverage must not become "no questions".
pub struct PrProjection {
    pub number: i64,
    pub title: String,
    pub description: String,
    pub ui_impact: UiImpact,
    pub ui_impact_reason: Option<String>,
    pub open_questions: Option<Vec<String>>,
    pub links: Vec<ProposalLink>,
}
#[derive(Clone)]
pub struct PrSiteOrigin(Url);
impl PrSiteOrigin {
    pub fn parse(value: &str, development: bool) -> Result<Self, PrSyncError> {
        let url = Url::parse(value).map_err(|_| PrSyncError::InvalidSite)?;
        let local = match url.host() {
            Some(Host::Domain("localhost")) => true,
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if !(url.scheme() == "https" || development && url.scheme() == "http" && local)
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || url.port() == Some(0)
        {
            return Err(PrSyncError::InvalidSite);
        }
        Ok(Self(url))
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum PrSyncError {
    InvalidSource,
    InvalidSite,
    InvalidProjection,
    TooLarge,
    InvalidIntent,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PrBinding {
    pub number: PullRequestNumber,
    pub provider_id: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrWritePhase {
    NotStarted,
    CreateStarted,
    UpdateStarted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrSyncDecision {
    CreateDraft,
    UpdateMetadata(PullRequestNumber),
    Synchronized(PullRequestNumber),
    ObserveAgain,
    Conflict,
    Stale,
}
#[derive(Clone)]
pub struct PrSyncIntent {
    source: PrSyncSource,
    content: PullRequestContent,
    request_hash: String,
}
impl PrSyncIntent {
    pub fn prepare(
        source: PrSyncSource,
        projection: PrProjection,
        site: &PrSiteOrigin,
    ) -> Result<Self, PrSyncError> {
        validate_source(&source)?;
        if projection.number < 1
            || !(3..=120).contains(&projection.title.chars().count())
            || projection.title.chars().any(char::is_control)
            || !(10..=10000).contains(&projection.description.chars().count())
            || projection.description.contains('\0')
            || projection
                .ui_impact_reason
                .as_ref()
                .is_some_and(|reason| reason.chars().count() > 10000 || reason.contains('\0'))
            || projection.ui_impact == UiImpact::No
                && projection
                    .ui_impact_reason
                    .as_ref()
                    .is_none_or(|reason| reason.trim().is_empty())
            || projection.open_questions.as_ref().is_some_and(|questions| {
                questions.len() > 100
                    || questions.iter().any(|q| {
                        q.trim().is_empty() || q.chars().count() > 1000 || q.contains('\0')
                    })
            })
            || projection.links.len() > 20
        {
            return Err(PrSyncError::InvalidProjection);
        }
        let mut page = site
            .0
            .join(&format!("changes/{}/review/snapshots", source.proposal))
            .map_err(|_| PrSyncError::InvalidSite)?;
        page.query_pairs_mut()
            .append_pair("snapshot", &source.snapshot.to_string());
        let mut body = format!(
            "{}\n{}\n{}\n\n# SpecSpace — הצעה #{}\n\n[גרסת הסקירה באתר]({page})\n\n## תיאור\n{}\n\n## השפעת ממשק\n{}\n",
            proposal_marker(&source),
            operation_marker(&source),
            snapshot_marker(&source),
            projection.number,
            quote(&projection.description),
            match projection.ui_impact {
                UiImpact::Yes => "כן",
                UiImpact::No => "לא",
                UiImpact::Undecided => "טרם הוכרעה",
            }
        );
        if let Some(reason) = &projection.ui_impact_reason {
            body.push_str(&format!("\n{}\n", quote(reason)));
        }
        body.push_str("\n## שאלות מוצר פתוחות\n");
        match projection.open_questions {
            None => body.push_str(
                "יש לבדוק באתר את שאלות המוצר לפני אישור. רשימה מלאה לא נכללה בהקשר זה.\n",
            ),
            Some(questions) if questions.is_empty() => {
                body.push_str("אין שאלות פתוחות רשומות בהקשר שנבדק.\n")
            }
            Some(questions) => {
                for (index, question) in questions.iter().enumerate() {
                    body.push_str(&format!("\n{}.\n{}\n", index + 1, quote(question)));
                }
            }
        }
        body.push_str("\n## קישורים\n");
        for link in projection.links {
            let url = Url::parse(&link.url).map_err(|_| PrSyncError::InvalidProjection)?;
            if url.scheme() != "https"
                || url.host().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || link.url.len() > 2048
                || link
                    .url
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '<' | '>' | '`' | '(' | ')'))
            {
                return Err(PrSyncError::InvalidProjection);
            }
            let label = match link.kind {
                LinkKind::DevelopmentPr => "PR פיתוח",
                LinkKind::Ticket => "משימת מעקב",
                LinkKind::Figma => "עיצוב",
            };
            body.push_str(&format!("- [{label}]({})\n", url.as_str()));
        }
        body.push_str(&format!("\n## זהות הגרסה\n- head: `{}`\n- target: `{}`\n- merge base: `{}`\n- policy/configuration: {}/{}\n\nגרסת סקירה זו אינה אישור ואינה פרסום באפיון המאושר.\n\n<!-- /specspace:projection -->",
            source.head.as_str(), source.target.as_str(), source.merge_base.as_str(), source.policy_version, source.configuration_version));
        let content =
            PullRequestContent::new(projection.title, body).map_err(|_| PrSyncError::TooLarge)?;
        let request_hash = hash(&source, &content);
        Ok(Self {
            source,
            content,
            request_hash,
        })
    }
    /// Restore already captured metadata without rerendering a newer template.
    pub fn restore(
        source: PrSyncSource,
        title: String,
        body: String,
        request_hash: &str,
    ) -> Result<Self, PrSyncError> {
        validate_source(&source)?;
        let content =
            PullRequestContent::new(title, body).map_err(|_| PrSyncError::InvalidIntent)?;
        if !digest(request_hash)
            || hash(&source, &content) != request_hash
            || !owned_body(content.body(), &source)
            || !content.body().starts_with(&format!(
                "{}\n{}\n{}\n",
                proposal_marker(&source),
                operation_marker(&source),
                snapshot_marker(&source)
            ))
        {
            return Err(PrSyncError::InvalidIntent);
        }
        Ok(Self {
            source,
            content,
            request_hash: request_hash.to_owned(),
        })
    }
    pub fn source(&self) -> &PrSyncSource {
        &self.source
    }
    pub fn content(&self) -> &PullRequestContent {
        &self.content
    }
    pub fn request_hash(&self) -> &str {
        &self.request_hash
    }
    pub fn scope(&self) -> PullRequestScope {
        PullRequestScope::new(self.source.repository.remote_id, self.source.branch.clone())
            .expect("validated source repository")
    }
    /// The caller must supply a complete bounded discovery result and recheck
    /// current authority. An unknown creation is never retried on apparent absence.
    pub fn decide(
        &self,
        observed: &[PullRequest],
        bound: Option<PrBinding>,
        phase: PrWritePhase,
    ) -> PrSyncDecision {
        if observed.is_empty() {
            return if bound.is_none() && phase == PrWritePhase::NotStarted {
                PrSyncDecision::CreateDraft
            } else {
                PrSyncDecision::ObserveAgain
            };
        }
        if observed.len() != 1 {
            return PrSyncDecision::Conflict;
        }
        let pr = &observed[0];
        if pr.repository_id != self.source.repository.remote_id
            || pr.branch != self.source.branch
            || !owned_body(&pr.body, &self.source)
            || pr.state != PullRequestState::Open
            || !pr.draft
            || pr.id == 0
            || bound
                .is_some_and(|binding| binding.number != pr.number || binding.provider_id != pr.id)
            || bound.is_none()
                && !pr.body.starts_with(&format!(
                    "{}\n{}\n",
                    proposal_marker(&self.source),
                    operation_marker(&self.source)
                ))
        {
            return PrSyncDecision::Conflict;
        }
        if pr.head != self.source.head || pr.target != self.source.target {
            return PrSyncDecision::Stale;
        }
        if pr.title == self.content.title() && pr.body == self.content.body() {
            return PrSyncDecision::Synchronized(pr.number);
        }
        if phase != PrWritePhase::NotStarted {
            return PrSyncDecision::ObserveAgain;
        }
        if bound.is_none() {
            return PrSyncDecision::Conflict;
        }
        PrSyncDecision::UpdateMetadata(pr.number)
    }
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_source(source: &PrSyncSource) -> Result<(), PrSyncError> {
    if [
        source.organization,
        source.proposal,
        source.actor,
        source.operation,
        source.snapshot,
        source.repository.repository,
    ]
    .iter()
    .any(Uuid::is_nil)
        || source.repository.remote_id == 0
        || source.repository.installation_id == 0
        || source.proposal_version < 1
        || source.configuration_version == 0
        || source.policy_version == 0
        || !digest(&source.fingerprint)
        || !digest(&source.evidence_digest)
        || !digest(&source.authority_digest)
    {
        return Err(PrSyncError::InvalidSource);
    }
    Ok(())
}
fn proposal_marker(source: &PrSyncSource) -> String {
    format!(
        "<!-- specspace:proposal:{}:{} -->",
        source.organization, source.proposal
    )
}
fn operation_marker(source: &PrSyncSource) -> String {
    format!("<!-- specspace:operation:{} -->", source.operation)
}
fn snapshot_marker(source: &PrSyncSource) -> String {
    format!(
        "<!-- specspace:snapshot:{}:{} -->",
        source.snapshot, source.fingerprint
    )
}
fn owned_body(body: &str, source: &PrSyncSource) -> bool {
    let mut lines = body.lines();
    if lines.next() != Some(proposal_marker(source).as_str()) {
        return false;
    }
    let operation = lines
        .next()
        .and_then(|line| line.strip_prefix("<!-- specspace:operation:"))
        .and_then(|line| line.strip_suffix(" -->"));
    if !operation.is_some_and(|value| {
        Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
    }) {
        return false;
    }
    let snapshot = lines
        .next()
        .and_then(|line| line.strip_prefix("<!-- specspace:snapshot:"))
        .and_then(|line| line.strip_suffix(" -->"))
        .and_then(|line| line.split_once(':'));
    if !snapshot.is_some_and(|(value, hash)| {
        digest(hash)
            && Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
    }) {
        return false;
    }
    body.ends_with("\n<!-- /specspace:projection -->")
        && body
            .lines()
            .filter(|line| *line == "<!-- /specspace:projection -->")
            .count()
            == 1
        && body
            .lines()
            .filter(|line| line.starts_with("<!-- specspace:proposal:"))
            .count()
            == 1
        && body
            .lines()
            .filter(|line| line.starts_with("<!-- specspace:operation:"))
            .count()
            == 1
        && body
            .lines()
            .filter(|line| line.starts_with("<!-- specspace:snapshot:"))
            .count()
            == 1
}

fn hash(source: &PrSyncSource, content: &PullRequestContent) -> String {
    let value = json!([
        "specspace-pr-sync-v1",
        [
            source.organization,
            source.proposal,
            source.actor,
            source.operation,
            source.snapshot
        ],
        [
            source.repository.repository.to_string(),
            source.repository.remote_id.to_string(),
            source.repository.installation_id.to_string(),
            source.branch.as_str().to_owned()
        ],
        [
            source.head.as_str(),
            source.target.as_str(),
            source.merge_base.as_str()
        ],
        [
            source.proposal_version.to_string(),
            source.configuration_version.to_string(),
            source.policy_version.to_string()
        ],
        [
            &source.fingerprint,
            &source.evidence_digest,
            &source.authority_digest
        ],
        [content.title(), content.body()]
    ]);
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).expect("fixed serializable metadata"))
    )
}
fn quote(text: &str) -> String {
    let mut value = String::from("> ");
    for character in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
        match character {
            '&' => value.push_str("&amp;"),
            '<' => value.push_str("&lt;"),
            '>' => value.push_str("&gt;"),
            '@' => value.push_str("&#64;"),
            '\n' => value.push_str("\n> "),
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '!' | '#' | '|' | '~' => {
                value.push('\\');
                value.push(character);
            }
            _ => value.push(character),
        }
    }
    value
}

#[cfg(test)]
mod tests;
