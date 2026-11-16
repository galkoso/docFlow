//! Full source is explicit user input, including unfinished recovery content.
//! This module renders/analyses it without publishing a file, buffer or review.
use super::{
    EditorError, EditorFinding, EditorPreview, Version, access, current, invalid, problem,
};
use crate::{AppState, ErrorBody, auth, specification};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_application::{book::BookError, proposal_reader::ProposalBook};
use specspace_document_engine::{
    editing::{self, EditorDocument},
    markdown, render,
};
use specspace_domain::RepositoryPath;
use specspace_git_adapter::{CommitSha, GitError};
use std::collections::HashSet;
use uuid::Uuid;

const MAX_SOURCE: usize = 2 * 1024 * 1024;
const MAX_FINDINGS: usize = 1000;

#[derive(Serialize, utoipa::ToSchema)]
pub struct EditorSource {
    pub proposal_id: Uuid,
    pub repository_id: Uuid,
    pub number: i64,
    pub title: String,
    pub branch: String,
    pub target_branch: String,
    pub proposal_version: i64,
    pub recorded_head_sha: String,
    pub snapshot_sha: String,
    pub document_id: String,
    pub can_edit: bool,
    pub markdown: String,
    pub sha256: String,
    pub parse_error: Option<String>,
    pub model: Option<EditorDocument>,
    pub model_error: Option<String>,
    pub findings: Vec<EditorFinding>,
    pub findings_total: usize,
    pub findings_truncated: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePreviewRequest {
    pub commit: String,
    pub expected_proposal_version: i64,
    pub expected_document_sha256: String,
    pub markdown: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SourcePreview {
    pub preview: EditorPreview,
    pub model: Option<EditorDocument>,
    pub model_error: Option<String>,
    pub parse_error: Option<String>,
    pub base_parse_error: Option<String>,
    /// This preview can also compare recovery against a historical/closed base.
    /// Actual writes must independently enforce current Git/state/version gates.
    pub can_edit: bool,
}

struct Analysis {
    parsed: Option<markdown::Document>,
    model: Option<EditorDocument>,
    model_error: Option<String>,
    parse_error: Option<String>,
    findings: Vec<EditorFinding>,
    total: usize,
    sections: HashSet<String>,
}
fn analyse(source: &str, document: &str) -> Analysis {
    match markdown::parse(source) {
        Ok(parsed) => {
            let invalid = if parsed.metadata.document_id != document {
                Some("EDITOR_DOCUMENT_ID_MISMATCH")
            } else if source.contains('\0') {
                Some("INVALID_CONTENT")
            } else {
                None
            };
            let total = parsed.findings.len() + usize::from(invalid.is_some());
            let mut findings: Vec<_> = parsed
                .findings
                .iter()
                .take(MAX_FINDINGS)
                .map(|finding| EditorFinding {
                    code: finding.code.into(),
                    source_start_byte: finding.source_start,
                    source_end_byte: finding.source_end,
                })
                .collect();
            if let Some(code) = invalid
                && findings.len() < MAX_FINDINGS
            {
                findings.push(EditorFinding {
                    code: code.into(),
                    source_start_byte: 0,
                    source_end_byte: source.len(),
                });
            }
            let (model, model_error) = match invalid {
                Some(code) => (None, Some(code.into())),
                None => match editing::parse(source) {
                    Ok(model) => (Some(model), None),
                    Err(code) => (None, Some(code.into())),
                },
            };
            let sections = parsed
                .blocks
                .iter()
                .filter(|block| block.kind == "heading" && !block.section_id.is_empty())
                .map(|block| block.section_id.clone())
                .collect();
            Analysis {
                parsed: Some(parsed),
                model,
                model_error,
                parse_error: None,
                findings,
                total,
                sections,
            }
        }
        Err(code) => Analysis {
            parsed: None,
            model: None,
            model_error: Some(code.into()),
            parse_error: Some(code.into()),
            findings: vec![EditorFinding {
                code: code.into(),
                source_start_byte: 0,
                source_end_byte: source.len(),
            }],
            total: 1,
            sections: HashSet::new(),
        },
    }
}
fn hash(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}

async fn raw(
    state: &AppState,
    snapshot: &ProposalBook,
    document: &str,
) -> Result<String, EditorError> {
    let matches: Vec<_> = snapshot
        .book
        .documents
        .iter()
        .filter(|entry| entry.document_id == document)
        .collect();
    let entry = match matches.as_slice() {
        [] => return Err(EditorError::Book(BookError::NotFound)),
        [entry] => entry,
        _ => return Err(EditorError::Book(BookError::InvalidCatalog)),
    };
    // A malicious/broken catalog must not broaden this endpoint to arbitrary
    // repository files. Source repair still addresses catalogued docs/*.md only.
    if !entry.path.starts_with("docs/") || !entry.path.ends_with(".md") {
        return Err(EditorError::Book(BookError::NotFound));
    }
    let path = RepositoryPath::parse(&entry.path)
        .map_err(|_| EditorError::Book(BookError::InvalidCatalog))?;
    let source = match &entry.content {
        Some(content) => content.source.clone(),
        None => state
            .book_source
            .provider(snapshot.scope.repository)
            .await
            .map_err(EditorError::Book)?
            .read_text(&snapshot.book.commit, &path)
            .await
            .map_err(|error| match error {
                GitError::ResponseTooLarge => invalid("FILE_TOO_LARGE"),
                error => EditorError::Book(BookError::Provider(error)),
            })?,
    };
    if source.len() > MAX_SOURCE {
        return Err(invalid("FILE_TOO_LARGE"));
    }
    Ok(source)
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/documents/{document}/editor/source",
            get(read),
        )
        .route(
            "/api/v1/proposals/{proposal}/documents/{document}/editor/source-preview",
            post(preview).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
}

#[utoipa::path(get,path="/api/v1/proposals/{proposal}/documents/{document}/editor/source",operation_id="proposalEditorSource",security(("sessionCookie"=[])),params(("proposal"=Uuid,Path),("document"=String,Path),Version),
    responses((status=200,body=EditorSource),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=502,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<Version>, axum::extract::rejection::QueryRejection>,
) -> Response {
    async fn run(
        state: &AppState,
        actor: Uuid,
        proposal: Uuid,
        document: String,
        commit: String,
    ) -> Result<Response, EditorError> {
        let commit = CommitSha::parse(commit)
            .map_err(|_| EditorError::Book(BookError::InvalidQuery("INVALID_COMMIT")))?;
        let snapshot = state
            .proposal_books
            .at(actor, proposal, Some(commit))
            .await
            .map_err(EditorError::Book)?;
        let source = raw(state, &snapshot, &document).await?;
        let permit = state
            .render_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| EditorError::Book(BookError::ProcessingUnavailable))?;
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let analysis = analyse(&source, &document);
            (source, document, analysis)
        })
        .await
        .map_err(|_| EditorError::Book(BookError::ProcessingUnavailable))?;
        let scope = current(state, actor, proposal, &snapshot).await?;
        let (markdown, document_id, analysis) = result;
        let can_edit = scope.editable() && snapshot.book.commit == snapshot.scope.head;
        Ok(Json(EditorSource {
            proposal_id: proposal,
            repository_id: scope.repository_id,
            number: scope.number,
            title: scope.title,
            branch: scope.branch,
            target_branch: scope.target_branch,
            proposal_version: scope.version,
            recorded_head_sha: snapshot.scope.head.as_str().into(),
            snapshot_sha: snapshot.book.commit.as_str().into(),
            document_id,
            can_edit,
            sha256: hash(&markdown),
            markdown,
            parse_error: analysis.parse_error,
            model: analysis.model,
            model_error: analysis.model_error,
            findings: analysis.findings,
            findings_total: analysis.total,
            findings_truncated: analysis.total > MAX_FINDINGS,
        })
        .into_response())
    }
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, document))), Ok(Query(query))) = (path, query) else {
        return specification::failure(BookError::InvalidQuery("INVALID_QUERY"));
    };
    match run(&state, actor.user_id, proposal, document, query.commit).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(post,path="/api/v1/proposals/{proposal}/documents/{document}/editor/source-preview",operation_id="previewProposalSource",security(("sessionCookie"=[])),params(("proposal"=Uuid,Path),("document"=String,Path)),request_body=SourcePreviewRequest,
    responses((status=200,body=SourcePreview),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=502,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    body: Result<Json<SourcePreviewRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    async fn run(
        state: &AppState,
        actor: Uuid,
        proposal: Uuid,
        document: String,
        request: SourcePreviewRequest,
    ) -> Result<Response, EditorError> {
        if request.markdown.len() > MAX_SOURCE {
            return Err(invalid("FILE_TOO_LARGE"));
        }
        if request.expected_proposal_version <= 0 {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                "INVALID_EDITOR_VERSION",
                "גרסת ההצעה אינה תקינה.",
            ));
        }
        let commit = CommitSha::parse(request.commit)
            .map_err(|_| EditorError::Book(BookError::InvalidQuery("INVALID_COMMIT")))?;
        let scope = access(state, actor, proposal).await?;
        if !scope.assigned {
            return Err(problem(
                StatusCode::FORBIDDEN,
                "EDITOR_FORBIDDEN",
                "אין הרשאה להכין עריכת מקור להצעה זו.",
            ));
        }
        if scope.version != request.expected_proposal_version {
            return Err(problem(
                StatusCode::CONFLICT,
                "EDITOR_VERSION_CHANGED",
                "פרטי ההצעה השתנו. יש לבדוק אותם תוך שמירת העריכה המקומית.",
            ));
        }
        let snapshot = state
            .proposal_books
            .at(actor, proposal, Some(commit.clone()))
            .await
            .map_err(EditorError::Book)?;
        let base = raw(state, &snapshot, &document).await?;
        if hash(&base) != request.expected_document_sha256 {
            return Err(invalid("EDITOR_BASE_MISMATCH"));
        }
        let permit = state
            .render_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| EditorError::Book(BookError::ProcessingUnavailable))?;
        let version = request.expected_proposal_version;
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let old = markdown::parse(&base);
            let old_sections: HashSet<_> = old
                .as_ref()
                .map(|parsed| {
                    parsed
                        .blocks
                        .iter()
                        .filter(|block| block.kind == "heading" && !block.section_id.is_empty())
                        .map(|block| block.section_id.clone())
                        .collect()
                })
                .unwrap_or_default();
            let base_parse_error = old.as_ref().err().map(|code| (*code).to_owned());
            let analysis = analyse(&request.markdown, &document);
            let removed_section_ids = if analysis.parsed.is_some() && old.is_ok() {
                let mut ids: Vec<_> = old_sections
                    .difference(&analysis.sections)
                    .cloned()
                    .collect();
                ids.sort();
                Some(ids)
            } else {
                None
            };
            let (render_events, render_error) = match &analysis.parsed {
                Some(parsed) => match render::events(parsed) {
                    Ok(events) => (events, None),
                    Err(code) => (vec![], Some(code.into())),
                },
                None => (vec![], analysis.parse_error.clone()),
            };
            SourcePreview {
                preview: EditorPreview {
                    proposal_id: proposal,
                    proposal_version: version,
                    document_id: document,
                    base_snapshot_sha: commit.as_str().into(),
                    base_document_sha256: request.expected_document_sha256,
                    sha256: hash(&request.markdown),
                    changed: request.markdown != base,
                    markdown: request.markdown,
                    removed_section_ids,
                    findings: analysis.findings,
                    findings_total: analysis.total,
                    findings_truncated: analysis.total > MAX_FINDINGS,
                    render_events,
                    render_error,
                },
                model: analysis.model,
                model_error: analysis.model_error,
                parse_error: analysis.parse_error,
                base_parse_error,
                can_edit: false,
            }
        })
        .await
        .map_err(|_| EditorError::Book(BookError::ProcessingUnavailable))?;
        let scope = current(state, actor, proposal, &snapshot).await?;
        if !scope.assigned {
            return Err(problem(
                StatusCode::FORBIDDEN,
                "EDITOR_FORBIDDEN",
                "הרשאת עריכת המקור אינה זמינה עוד.",
            ));
        }
        if scope.version != version {
            return Err(problem(
                StatusCode::CONFLICT,
                "EDITOR_VERSION_CHANGED",
                "פרטי ההצעה השתנו בזמן הכנת התצוגה.",
            ));
        }
        let mut result = result;
        result.can_edit = scope.editable() && snapshot.book.commit == snapshot.scope.head;
        Ok(Json(result).into_response())
    }
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_write(&state, &headers, &actor) {
        return error.into_response();
    }
    let Ok(Path((proposal, document))) = path else {
        return specification::failure(BookError::InvalidQuery("INVALID_QUERY"));
    };
    let request = match body {
        Ok(Json(request)) => request,
        Err(error) => {
            return problem(
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "INVALID_EDITOR_REQUEST",
                "פרטי המקור אינם תקינים.",
            )
            .into_response();
        }
    };
    match run(&state, actor.user_id, proposal, document, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
