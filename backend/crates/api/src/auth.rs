use crate::{ApiError, AppState, ErrorBody};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use cookie::{Cookie, SameSite, time::Duration};
use serde::{Deserialize, Serialize};
use specspace_infrastructure::{
    login_flows::LoginFlowStore,
    sessions::{SessionError, SessionIdentity, SessionStore},
};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Clone)]
pub struct CookiePolicy {
    secure: bool,
    origin: Option<String>,
}

impl CookiePolicy {
    pub fn unconfigured() -> Self {
        Self {
            secure: true,
            origin: None,
        }
    }

    pub fn new(public_url: &str, local_development: bool) -> Result<Self, &'static str> {
        let url = url::Url::parse(public_url).map_err(|_| "INVALID_PUBLIC_URL")?;
        let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !(url.scheme() == "https" || (local_development && local && url.scheme() == "http"))
        {
            return Err("INVALID_PUBLIC_URL");
        }
        Ok(Self {
            secure: url.scheme() == "https",
            origin: Some(url.origin().ascii_serialization()),
        })
    }

    fn session_name(&self) -> &'static str {
        if self.secure {
            "__Host-specspace-session"
        } else {
            "specspace-session"
        }
    }
    fn login_name(&self) -> &'static str {
        if self.secure {
            "__Host-specspace-login"
        } else {
            "specspace-login"
        }
    }
    fn cookie(
        &self,
        name: &'static str,
        value: &str,
        http_only: bool,
        seconds: i64,
    ) -> Cookie<'static> {
        Cookie::build((name, value.to_owned()))
            .path("/")
            .secure(self.secure)
            .http_only(http_only)
            .same_site(SameSite::Lax)
            .max_age(Duration::seconds(seconds))
            .build()
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/login", get(login))
        .route("/api/v1/auth/status", get(availability))
        .route("/api/v1/auth/callback", get(callback))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/me", get(me))
}

pub(crate) fn error(status: StatusCode, code: &'static str, message_he: &'static str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ApiError {
                code,
                message_he,
                trace_id: Uuid::new_v4(),
                retryable: status == StatusCode::SERVICE_UNAVAILABLE,
                field_errors: serde_json::json!({}),
            },
        }),
    )
        .into_response()
}

fn database_error() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "AUTH_UNAVAILABLE",
        "שירות ההתחברות אינו זמין כרגע.",
    )
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut values = vec![];
    for header in headers.get_all(header::COOKIE) {
        for cookie in Cookie::split_parse(header.to_str().ok()?) {
            let cookie = cookie.ok()?;
            if cookie.name() == name {
                values.push(cookie.value().to_owned());
            }
        }
    }
    if values.len() == 1 {
        values.pop()
    } else {
        None
    }
}

fn set_cookie(response: &mut Response, cookie: Cookie<'_>) {
    // Cookies contain only server-generated hex secrets and constant attributes.
    if let Ok(mut value) = HeaderValue::from_str(&cookie.to_string()) {
        value.set_sensitive(true);
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

pub(crate) enum AuthFailure {
    Required,
    Expired,
    Unavailable,
    Csrf,
}

impl IntoResponse for AuthFailure {
    fn into_response(self) -> Response {
        match self {
            Self::Required => error(
                StatusCode::UNAUTHORIZED,
                "AUTH_REQUIRED",
                "יש להתחבר כדי להמשיך.",
            ),
            Self::Expired => error(
                StatusCode::UNAUTHORIZED,
                "SESSION_EXPIRED",
                "ההתחברות פגה. יש להתחבר מחדש.",
            ),
            Self::Unavailable => database_error(),
            Self::Csrf => error(
                StatusCode::FORBIDDEN,
                "CSRF_FAILED",
                "אימות הבקשה נכשל. יש לרענן את העמוד.",
            ),
        }
    }
}

pub(crate) async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<SessionIdentity, AuthFailure> {
    let token = cookie_value(headers, state.cookies.session_name()).ok_or(AuthFailure::Required)?;
    match SessionStore::new(state.database.clone())
        .authenticate(&token)
        .await
    {
        Ok(Some(identity)) => Ok(identity),
        Ok(None) => Err(AuthFailure::Expired),
        Err(_) => Err(AuthFailure::Unavailable),
    }
}

pub(crate) fn verify_write(
    state: &AppState,
    headers: &HeaderMap,
    identity: &SessionIdentity,
) -> Result<(), AuthFailure> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let csrf = headers
        .get("X-XSRF-TOKEN")
        .and_then(|value| value.to_str().ok());
    if state.cookies.origin.as_deref().is_none()
        || origin != state.cookies.origin.as_deref()
        || !csrf.is_some_and(|value| identity.verify_csrf(value))
    {
        return Err(AuthFailure::Csrf);
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/v1/auth/login", responses((status = 307, description = "Redirect to the configured identity provider"), (status = 503, body = ErrorBody)))]
pub(crate) async fn login(State(state): State<AppState>) -> Response {
    let Some(oidc) = &state.oidc else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OIDC_NOT_CONFIGURED",
            "התחברות ארגונית עדיין לא הוגדרה.",
        );
    };
    let start = match oidc.begin().await {
        Ok(start) => start,
        Err(_) => return database_error(),
    };
    let binding = match LoginFlowStore::new(state.database.clone())
        .begin(&start.state, &start.nonce, &start.pkce_verifier)
        .await
    {
        Ok(binding) => binding,
        Err(_) => return database_error(),
    };
    let mut response = Redirect::temporary(&start.redirect_url).into_response();
    set_cookie(
        &mut response,
        state
            .cookies
            .cookie(state.cookies.login_name(), &binding, true, 300),
    );
    response
}

#[derive(Deserialize)]
pub(crate) struct Callback {
    state: String,
    code: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/auth/callback",
    params(("state" = String, Query), ("code" = Option<String>, Query)),
    responses((status = 303, description = "Session cookies issued; redirect to specification"), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<Callback>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Some(oidc) = &state.oidc else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OIDC_NOT_CONFIGURED",
            "התחברות ארגונית עדיין לא הוגדרה.",
        );
    };
    let Ok(Query(query)) = query else {
        return error(
            StatusCode::BAD_REQUEST,
            "INVALID_CALLBACK",
            "קישור ההתחברות אינו תקין.",
        );
    };
    let Some(binding) = cookie_value(&headers, state.cookies.login_name()) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "LOGIN_STATE_INVALID",
            "יש להתחיל את ההתחברות מחדש באותו דפדפן.",
        );
    };
    let flow = match LoginFlowStore::new(state.database.clone())
        .consume(&query.state, &binding)
        .await
    {
        Ok(Some(flow)) => flow,
        Ok(None) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "LOGIN_STATE_INVALID",
                "ההתחברות פגה או כבר הושלמה. יש להתחיל מחדש.",
            );
        }
        Err(_) => return database_error(),
    };
    let Some(code) = query.code else {
        return error(
            StatusCode::UNAUTHORIZED,
            "LOGIN_CANCELLED",
            "ההתחברות לא הושלמה.",
        );
    };
    let identity = match oidc
        .verify_code(&code, flow.nonce, flow.pkce_verifier)
        .await
    {
        Ok(identity) => identity,
        Err(_) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "OIDC_VERIFICATION_FAILED",
                "לא ניתן לאמת את ההתחברות. יש להתחיל מחדש.",
            );
        }
    };
    let user = sqlx::query_scalar::<_, Uuid>("SELECT u.id FROM users u WHERE issuer = $1 AND external_subject = $2 AND status = 'ACTIVE' AND EXISTS (SELECT 1 FROM memberships m WHERE m.user_id = u.id)")
        .bind(identity.issuer).bind(identity.subject).fetch_optional(&state.database).await;
    let user_id = match user {
        Ok(Some(user)) => user,
        Ok(None) => {
            return error(
                StatusCode::FORBIDDEN,
                "ACCOUNT_NOT_REGISTERED",
                "החשבון טרם הוקצה לסביבת העבודה. יש לפנות למנהל המערכת.",
            );
        }
        Err(_) => return database_error(),
    };
    let sessions = SessionStore::new(state.database.clone());
    let credentials = match sessions.mint(user_id, 28800).await {
        Ok(credentials) => credentials,
        Err(SessionError::Unauthorized) => {
            return error(
                StatusCode::FORBIDDEN,
                "ACCOUNT_DISABLED",
                "הגישה לחשבון אינה פעילה.",
            );
        }
        Err(_) => return database_error(),
    };
    if let Some(old) = cookie_value(&headers, state.cookies.session_name())
        && sessions.revoke(&old).await.is_err()
    {
        return database_error();
    }
    let mut response = Redirect::to("/specification").into_response();
    set_cookie(
        &mut response,
        state
            .cookies
            .cookie(state.cookies.login_name(), "", true, 0),
    );
    set_cookie(
        &mut response,
        state.cookies.cookie(
            state.cookies.session_name(),
            credentials.session_cookie_value(),
            true,
            28800,
        ),
    );
    set_cookie(
        &mut response,
        state
            .cookies
            .cookie("XSRF-TOKEN", credentials.csrf_cookie_value(), false, 28800),
    );
    response
}

#[utoipa::path(post, path = "/api/v1/auth/logout", security(("sessionCookie" = [])),
    params(("Origin" = String, Header), ("X-XSRF-TOKEN" = String, Header)),
    responses((status = 204, description = "Current session revoked"), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let identity = match authenticate(&state, &headers).await {
        Ok(identity) => identity,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = verify_write(&state, &headers, &identity) {
        return error.into_response();
    }
    if let Some(token) = cookie_value(&headers, state.cookies.session_name())
        && SessionStore::new(state.database.clone())
            .revoke(&token)
            .await
            .is_err()
    {
        return database_error();
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    set_cookie(
        &mut response,
        state
            .cookies
            .cookie(state.cookies.session_name(), "", true, 0),
    );
    set_cookie(
        &mut response,
        state.cookies.cookie("XSRF-TOKEN", "", false, 0),
    );
    response
}

#[derive(Serialize, FromRow, utoipa::ToSchema)]
pub struct Membership {
    organization_id: Uuid,
    role: String,
    review_domains: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProfileCapabilities {
    read_specification: bool,
    create_proposal: bool,
    review: bool,
    merge: bool,
    manage_policy: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Profile {
    user_id: Uuid,
    display_name: String,
    memberships: Vec<Membership>,
    capabilities: ProfileCapabilities,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AuthAvailability {
    login_available: bool,
}

#[utoipa::path(get, path = "/api/v1/auth/status", responses((status = 200, body = AuthAvailability)))]
pub(crate) async fn availability(State(state): State<AppState>) -> Json<AuthAvailability> {
    Json(AuthAvailability {
        login_available: state.oidc.is_some(),
    })
}

#[utoipa::path(get, path = "/api/v1/me", security(("sessionCookie" = [])), responses((status = 200, body = Profile), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let identity = match authenticate(&state, &headers).await {
        Ok(identity) => identity,
        Err(error) => return error.into_response(),
    };
    let name = sqlx::query_scalar::<_, String>(
        "SELECT display_name FROM users WHERE id = $1 AND status = 'ACTIVE'",
    )
    .bind(identity.user_id)
    .fetch_optional(&state.database)
    .await;
    let name = match name {
        Ok(Some(name)) => name,
        Ok(None) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "ACCOUNT_DISABLED",
                "הגישה לחשבון אינה פעילה.",
            );
        }
        Err(_) => return database_error(),
    };
    let memberships = match sqlx::query_as::<_, Membership>("SELECT organization_id, role, review_domains FROM memberships WHERE user_id = $1 ORDER BY organization_id").bind(identity.user_id).fetch_all(&state.database).await {
        Ok(memberships) => memberships, Err(_) => return database_error(),
    };
    if memberships.is_empty() {
        return AuthFailure::Expired.into_response();
    }
    let capabilities = ProfileCapabilities {
        read_specification: true,
        create_proposal: memberships
            .iter()
            .any(|membership| membership.role != "READER"),
        review: memberships.iter().any(|membership| {
            matches!(
                membership.role.as_str(),
                "REVIEWER" | "MAINTAINER" | "ADMIN"
            ) && !membership.review_domains.is_empty()
        }),
        merge: memberships
            .iter()
            .any(|membership| matches!(membership.role.as_str(), "MAINTAINER" | "ADMIN")),
        manage_policy: memberships
            .iter()
            .any(|membership| membership.role == "ADMIN"),
    };
    Json(Profile {
        user_id: identity.user_id,
        display_name: name,
        memberships,
        capabilities,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_cookies_have_host_scope_secure_http_only_and_same_site() {
        let policy = CookiePolicy::new("https://spec.example", false).unwrap();
        let cookie = policy.cookie(policy.session_name(), "synthetic-secret", true, 28800);
        assert_eq!(cookie.name(), "__Host-specspace-session");
        assert_eq!(cookie.path(), Some("/"));
        assert!(cookie.domain().is_none());
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert!(CookiePolicy::new("http://spec.example", true).is_err());
        assert!(CookiePolicy::new("http://127.0.0.1", false).is_err());
    }

    #[test]
    fn ambiguous_session_cookies_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("specspace-session=first; specspace-session=second"),
        );
        assert!(cookie_value(&headers, "specspace-session").is_none());
    }
}
