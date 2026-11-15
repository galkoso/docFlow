use crate::{ApiError, AppState, ErrorBody, auth, specification};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use specspace_application::{book::BookError, proposal_reader::ProposalBook};
use specspace_document_engine::{
    editing::{self, EditorDocument, EditorEdit},
    markdown, render,
};
use specspace_git_adapter::CommitSha;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in=Query)]
pub(crate) struct Version {
    commit: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EditorPreviewRequest {
    pub commit: String,
    pub expected_proposal_version: i64,
    pub edit: EditorEdit,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct EditorResponse {
    pub proposal_id: Uuid,
    pub repository_id: Uuid,
    pub number: i64,
    pub title: String,
    pub branch: String,
    pub target_branch: String,
    pub proposal_version: i64,
    pub recorded_head_sha: String,
    pub snapshot_sha: String,
    pub can_edit: bool,
    pub model: EditorDocument,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct EditorFinding {
    pub code: String,
    pub source_start_byte: usize,
    pub source_end_byte: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct EditorPreview {
    pub proposal_id: Uuid,
    pub proposal_version: i64,
    pub document_id: String,
    pub base_snapshot_sha: String,
    pub base_document_sha256: String,
    pub markdown: String,
    pub sha256: String,
    pub changed: bool,
    pub removed_section_ids: Vec<String>,
    pub findings: Vec<EditorFinding>,
    pub render_events: Vec<render::RenderEvent>,
    pub render_error: Option<String>,
}

#[derive(FromRow)]
struct Access {
    repository_id: Uuid,
    number: i64,
    title: String,
    branch: String,
    target_branch: String,
    state: String,
    head_sha: Option<String>,
    version: i64,
    assigned: bool,
}
impl Access {
    fn editable(&self) -> bool {
        self.assigned
            && matches!(
                self.state.as_str(),
                "DRAFT" | "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
            )
    }
    fn assert_edit(&self, commit: &CommitSha, version: i64) -> Result<(), EditorError> {
        if !self.assigned {
            return Err(problem(
                StatusCode::FORBIDDEN,
                "EDITOR_FORBIDDEN",
                "אין הרשאה לערוך את ההצעה הזו.",
            ));
        }
        if !self.editable() {
            return Err(problem(
                StatusCode::CONFLICT,
                "PROPOSAL_NOT_EDITABLE",
                "ההצעה אינה זמינה לעריכה. יש לשמור עותק התאוששות ולבדוק את מצבה.",
            ));
        }
        if self.head_sha.as_deref() != Some(commit.as_str()) {
            return Err(problem(
                StatusCode::CONFLICT,
                "EDITOR_HEAD_CHANGED",
                "גרסת ההצעה השתנתה. יש להשוות לגרסה הרשומה לפני המשך העריכה.",
            ));
        }
        if self.version != version {
            return Err(problem(
                StatusCode::CONFLICT,
                "EDITOR_VERSION_CHANGED",
                "פרטי ההצעה השתנו. יש לטעון אותם מחדש תוך שמירת העריכה המקומית.",
            ));
        }
        Ok(())
    }
}

enum EditorError {
    Auth(auth::AuthFailure),
    Book(BookError),
    Problem {
        status: StatusCode,
        code: &'static str,
        message: &'static str,
    },
}
impl IntoResponse for EditorError {
    fn into_response(self) -> Response {
        match self {
            Self::Auth(error) => error.into_response(),
            Self::Book(error) => specification::failure(error),
            Self::Problem {
                status,
                code,
                message,
            } => (
                status,
                Json(ErrorBody {
                    error: ApiError {
                        code,
                        message_he: message,
                        trace_id: Uuid::new_v4(),
                        retryable: false,
                        field_errors: serde_json::json!({}),
                        details: None,
                    },
                }),
            )
                .into_response(),
        }
    }
}
fn problem(status: StatusCode, code: &'static str, message: &'static str) -> EditorError {
    EditorError::Problem {
        status,
        code,
        message,
    }
}
fn invalid(code: &'static str) -> EditorError {
    problem(
        match code {
            "EDITOR_BASE_MISMATCH" => StatusCode::CONFLICT,
            "FILE_TOO_LARGE" => StatusCode::PAYLOAD_TOO_LARGE,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        },
        code,
        "לא ניתן להכין את עריכת המסמך. יש לבדוק את הגרסה ואת מקור התוכן.",
    )
}
async fn access(state: &AppState, actor: Uuid, proposal: Uuid) -> Result<Access, EditorError> {
    sqlx::query_as("SELECT p.repository_id,p.number,p.title,p.branch,r.default_branch AS target_branch,p.state,p.head_sha,p.version,(m.role<>'READER' AND (p.owner_id=$1 OR m.role='ADMIN' OR EXISTS(SELECT 1 FROM proposal_collaborators c WHERE c.organization_id=p.organization_id AND c.proposal_id=p.id AND c.user_id=$1 AND c.access='EDIT'))) AS assigned FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$2 AND u.id=$1 AND u.status='ACTIVE'")
        .bind(actor).bind(proposal).fetch_optional(&state.database).await.map_err(|_|EditorError::Book(BookError::AuthorizationUnavailable))?
        .ok_or(EditorError::Book(BookError::NotFound))
}
fn source<'a>(snapshot: &'a ProposalBook, document: &str) -> Result<&'a str, EditorError> {
    let matching: Vec<_> = snapshot
        .book
        .documents
        .iter()
        .filter(|entry| entry.document_id == document)
        .collect();
    match matching.as_slice() {
        [entry] => {
            let doc = entry
                .content
                .as_ref()
                .ok_or_else(|| invalid("EDITOR_SOURCE_INVALID"))?;
            if doc.metadata.document_id != document {
                return Err(invalid("EDITOR_DOCUMENT_ID_MISMATCH"));
            }
            Ok(doc.source.as_str())
        }
        [] => Err(EditorError::Book(BookError::NotFound)),
        _ => Err(EditorError::Book(BookError::InvalidCatalog)),
    }
}
async fn current(
    state: &AppState,
    actor: Uuid,
    proposal: Uuid,
    snapshot: &ProposalBook,
) -> Result<Access, EditorError> {
    state
        .proposal_books
        .recheck(actor, proposal, &snapshot.scope)
        .await
        .map_err(EditorError::Book)?;
    let access = access(state, actor, proposal).await?;
    if access.repository_id != snapshot.scope.repository
        || access.branch != snapshot.scope.branch.as_str()
        || access.head_sha.as_deref() != Some(snapshot.scope.head.as_str())
    {
        return Err(problem(
            StatusCode::CONFLICT,
            "EDITOR_CONTEXT_CHANGED",
            "הגרסה השתנתה בזמן הטעינה. יש לבדוק את ההצעה מחדש.",
        ));
    }
    Ok(access)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/documents/{document}/editor",
            get(model),
        )
        .route(
            "/api/v1/proposals/{proposal}/documents/{document}/editor/preview",
            post(preview).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
}

#[utoipa::path(get,path="/api/v1/proposals/{proposal}/documents/{document}/editor",operation_id="proposalEditorModel",security(("sessionCookie"=[])),params(("proposal"=Uuid,Path),("document"=String,Path),Version),
    responses((status=200,body=EditorResponse),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=502,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn model(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<Version>, axum::extract::rejection::QueryRejection>,
) -> Response {
    async fn run(
        state: &AppState,
        headers: &HeaderMap,
        proposal: Uuid,
        document: String,
        commit: String,
    ) -> Result<Response, EditorError> {
        let actor = auth::authenticate(state, headers)
            .await
            .map_err(EditorError::Auth)?;
        let commit = CommitSha::parse(commit)
            .map_err(|_| EditorError::Book(BookError::InvalidQuery("INVALID_COMMIT")))?;
        let snapshot = state
            .proposal_books
            .at(actor.user_id, proposal, Some(commit))
            .await
            .map_err(EditorError::Book)?;
        source(&snapshot, &document)?;
        let permit = state
            .render_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| EditorError::Book(BookError::ProcessingUnavailable))?;
        let book = snapshot.book.clone();
        let view_scope = snapshot.scope.clone();
        let parsed = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let view = ProposalBook {
                scope: view_scope,
                book,
            };
            editing::parse(source(&view, &document)?).map_err(invalid)
        })
        .await
        .map_err(|_| EditorError::Book(BookError::ProcessingUnavailable))?;
        let scope = current(state, actor.user_id, proposal, &snapshot).await?;
        let model = parsed?;
        let snapshot_sha = snapshot.book.commit.as_str().to_owned();
        let can_edit = scope.editable() && snapshot_sha == snapshot.scope.head.as_str();
        let recorded_head_sha = scope
            .head_sha
            .clone()
            .ok_or(EditorError::Book(BookError::ProposalNotReady))?;
        Ok(Json(EditorResponse {
            proposal_id: proposal,
            repository_id: scope.repository_id,
            number: scope.number,
            title: scope.title,
            branch: scope.branch,
            target_branch: scope.target_branch,
            proposal_version: scope.version,
            recorded_head_sha,
            can_edit,
            snapshot_sha,
            model,
        })
        .into_response())
    }
    let (Ok(Path((proposal, document))), Ok(Query(query))) = (path, query) else {
        return specification::failure(BookError::InvalidQuery("INVALID_QUERY"));
    };
    match run(&state, &headers, proposal, document, query.commit).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(post,path="/api/v1/proposals/{proposal}/documents/{document}/editor/preview",operation_id="previewProposalEditor",security(("sessionCookie"=[])),params(("proposal"=Uuid,Path),("document"=String,Path)),request_body=EditorPreviewRequest,
    responses((status=200,body=EditorPreview),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=502,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    body: Result<Json<EditorPreviewRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    async fn run(
        state: &AppState,
        actor: Uuid,
        proposal: Uuid,
        document: String,
        request: EditorPreviewRequest,
    ) -> Result<Response, EditorError> {
        if request.expected_proposal_version <= 0 {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                "INVALID_EDITOR_VERSION",
                "גרסת ההצעה אינה תקינה.",
            ));
        }
        let commit = CommitSha::parse(request.commit)
            .map_err(|_| EditorError::Book(BookError::InvalidQuery("INVALID_COMMIT")))?;
        access(state, actor, proposal)
            .await?
            .assert_edit(&commit, request.expected_proposal_version)?;
        let snapshot = state
            .proposal_books
            .at(actor, proposal, Some(commit.clone()))
            .await
            .map_err(EditorError::Book)?;
        source(&snapshot, &document)?;
        let permit = state
            .render_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| EditorError::Book(BookError::ProcessingUnavailable))?;
        let book = snapshot.book.clone();
        let view_scope = snapshot.scope.clone();
        let version = request.expected_proposal_version;
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let view = ProposalBook {
                scope: view_scope,
                book,
            };
            let model = editing::parse(source(&view, &document)?).map_err(invalid)?;
            let edited = model.assemble(&request.edit).map_err(invalid)?;
            let parsed = markdown::parse(&edited.source).map_err(invalid)?;
            let (render_events, render_error) = match render::events(&parsed) {
                Ok(events) => (events, None),
                Err(code) => (vec![], Some(code.to_owned())),
            };
            Ok::<_, EditorError>(EditorPreview {
                proposal_id: proposal,
                proposal_version: version,
                document_id: document,
                base_snapshot_sha: commit.as_str().into(),
                base_document_sha256: model.sha256,
                markdown: edited.source,
                sha256: edited.sha256,
                changed: edited.changed,
                removed_section_ids: edited.removed_section_ids,
                findings: parsed
                    .findings
                    .into_iter()
                    .map(|finding| EditorFinding {
                        code: finding.code.into(),
                        source_start_byte: finding.source_start,
                        source_end_byte: finding.source_end,
                    })
                    .collect(),
                render_events,
                render_error,
            })
        })
        .await
        .map_err(|_| EditorError::Book(BookError::ProcessingUnavailable))?;
        current(state, actor, proposal, &snapshot)
            .await?
            .assert_edit(&snapshot.book.commit, version)?;
        Ok(Json(result?).into_response())
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
                "פרטי העריכה אינם תקינים.",
            )
            .into_response();
        }
    };
    match run(&state, actor.user_id, proposal, document, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
