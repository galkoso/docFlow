use crate::{CommitRequest, CommitSha, FileChange, GitError, GitProvider};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{
    Client, Response, StatusCode,
    header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT},
};
use serde_json::{Value, json};
use specspace_domain::{BranchName, RepositoryPath};
use std::{collections::HashSet, time::Duration};

pub struct GitHub {
    client: Client,
    repository: String,
    owner: String,
    name: String,
    rest_base: String,
    graphql_url: String,
}

impl GitHub {
    /// Supply a short-lived GitHub App installation token, issued for this repo.
    /// A new adapter is constructed when the installation token is refreshed.
    pub fn new(repository: &str, installation_token: &str) -> Result<Self, GitError> {
        Self::configured(
            repository,
            installation_token,
            "https://api.github.com",
            "https://api.github.com/graphql",
        )
    }

    fn configured(
        repository: &str,
        token: &str,
        rest_base: &str,
        graphql_url: &str,
    ) -> Result<Self, GitError> {
        let parts: Vec<_> = repository.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || *part == "."
                    || *part == ".."
                    || part.len() > 100
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
        {
            return Err(GitError::InvalidInput);
        }
        let mut headers = HeaderMap::new();
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| GitError::InvalidInput)?;
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("SpecSpace/0.1"));
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2026-03-10"),
        );
        let client = Client::builder()
            .retry(reqwest::retry::never())
            .default_headers(headers)
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| GitError::ProviderUnavailable)?;
        Ok(Self {
            client,
            repository: repository.into(),
            owner: parts[0].into(),
            name: parts[1].into(),
            rest_base: rest_base.into(),
            graphql_url: graphql_url.into(),
        })
    }

    pub(crate) async fn response(
        mut response: Response,
        mutation: bool,
    ) -> Result<Value, GitError> {
        let status = response.status();
        let retry_after_seconds = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        if status == StatusCode::TOO_MANY_REQUESTS
            || (status == StatusCode::FORBIDDEN
                && (retry_after_seconds.is_some()
                    || response
                        .headers()
                        .get("x-ratelimit-remaining")
                        .is_some_and(|v| v == "0")))
        {
            return Err(GitError::RateLimited {
                retry_after_seconds,
            });
        }
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(GitError::Unauthorized);
        }
        if status == StatusCode::NOT_FOUND {
            return Err(GitError::BranchMissing);
        }
        if status.is_server_error() {
            return Err(if mutation {
                GitError::ResultUnknown
            } else {
                GitError::ProviderUnavailable
            });
        }
        if !status.is_success() {
            return Err(GitError::ProviderRejected);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            if mutation {
                GitError::ResultUnknown
            } else {
                GitError::ProviderUnavailable
            }
        })? {
            if body.len().saturating_add(chunk.len()) > 16 * 1024 * 1024 {
                return Err(if mutation {
                    GitError::ResultUnknown
                } else {
                    GitError::ResponseTooLarge
                });
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| {
            if mutation {
                GitError::ResultUnknown
            } else {
                GitError::InvalidResponse
            }
        })
    }

    async fn graphql(
        &self,
        query: &str,
        variables: Value,
        mutation: bool,
    ) -> Result<Value, GitError> {
        let response = self
            .client
            .post(&self.graphql_url)
            .json(&json!({"query": query, "variables": variables}))
            .send()
            .await
            .map_err(|_| {
                if mutation {
                    GitError::ResultUnknown
                } else {
                    GitError::ProviderUnavailable
                }
            })?;
        let body = Self::response(response, mutation).await?;
        if body
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            // Partial GraphQL mutation responses may contain an applied commit.
            return Err(if mutation {
                GitError::ResultUnknown
            } else {
                GitError::ProviderRejected
            });
        }
        Ok(body)
    }

    async fn head(&self, branch: &str) -> Result<CommitSha, GitError> {
        let data = self.graphql(
            "query($owner: String!, $name: String!, $ref: String!) { repository(owner: $owner, name: $name) { ref(qualifiedName: $ref) { target { oid } } } }",
            json!({"owner": self.owner, "name": self.name, "ref": format!("refs/heads/{branch}")}), false,
        ).await?;
        let sha = data
            .pointer("/data/repository/ref/target/oid")
            .and_then(Value::as_str)
            .ok_or(GitError::BranchMissing)?;
        CommitSha::parse(sha)
    }

    fn commit_input(&self, request: &CommitRequest<'_>) -> Result<Value, GitError> {
        if request.changes.is_empty()
            || request.changes.len() > 50
            || request.message.trim().is_empty()
            || request.message.chars().count() > 10000
        {
            return Err(GitError::InvalidInput);
        }
        let mut additions = vec![];
        let mut deletions = vec![];
        let mut paths = HashSet::new();
        let mut total_bytes = 0usize;
        for change in request.changes {
            let path = match change {
                FileChange::Put { path, .. } | FileChange::Delete { path } => path,
            };
            if !paths.insert(path.as_str().to_lowercase()) {
                return Err(GitError::InvalidInput);
            }
            match change {
                FileChange::Put { path, content } => {
                    total_bytes = total_bytes.saturating_add(content.len());
                    if total_bytes > 10 * 1024 * 1024 {
                        return Err(GitError::InvalidInput);
                    }
                    additions
                        .push(json!({"path": path.as_str(), "contents": STANDARD.encode(content)}));
                }
                FileChange::Delete { path } => deletions.push(json!({"path": path.as_str()})),
            }
        }
        Ok(json!({
            "branch": {"repositoryNameWithOwner": self.repository, "branchName": request.branch.as_str()},
            "expectedHeadOid": request.expected_head.as_str(),
            "clientMutationId": request.operation_id.to_string(),
            "message": {"headline": request.message.lines().next().unwrap_or("SpecSpace update"),
                "body": format!("{}\n\nSpecSpace-Operation: {}\nSpecSpace-Actor: {}", request.message, request.operation_id, request.actor_id)},
            "fileChanges": {"additions": additions, "deletions": deletions}
        }))
    }
}

#[async_trait]
impl GitProvider for GitHub {
    async fn main_head(&self) -> Result<CommitSha, GitError> {
        self.head("main").await
    }
    async fn branch_head(&self, branch: &BranchName) -> Result<CommitSha, GitError> {
        self.head(branch.as_str()).await
    }

    async fn create_branch(
        &self,
        branch: &BranchName,
        source: &CommitSha,
    ) -> Result<CommitSha, GitError> {
        let response = self
            .client
            .post(format!(
                "{}/repos/{}/git/refs",
                self.rest_base, self.repository
            ))
            .json(
                &json!({"ref": format!("refs/heads/{}", branch.as_str()), "sha": source.as_str()}),
            )
            .send()
            .await
            .map_err(|_| GitError::ResultUnknown)?;
        if response.status() == StatusCode::UNPROCESSABLE_ENTITY
            && self.branch_head(branch).await.is_ok()
        {
            return Err(GitError::BranchExists);
        }
        let data = Self::response(response, true).await?;
        let returned = data
            .pointer("/object/sha")
            .and_then(Value::as_str)
            .ok_or(GitError::ResultUnknown)?;
        let actual = self.branch_head(branch).await?;
        if returned != source.as_str() || actual != *source {
            return Err(GitError::ResultUnknown);
        }
        Ok(actual)
    }

    async fn commit(&self, request: CommitRequest<'_>) -> Result<CommitSha, GitError> {
        let input = self.commit_input(&request)?;
        if self.branch_head(request.branch).await? != *request.expected_head {
            return Err(GitError::HeadMismatch);
        }
        let result = self.graphql(
            "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
            json!({"input": input}), true,
        ).await;
        let data = match result {
            Ok(data) => data,
            Err(GitError::ResultUnknown) => {
                // A changed head alone does not prove who wrote it. The operation
                // journal must inspect commit trailers before retrying/settling.
                return Err(GitError::ResultUnknown);
            }
            Err(error) => return Err(error),
        };
        let oid = data
            .pointer("/data/createCommitOnBranch/commit/oid")
            .and_then(Value::as_str)
            .ok_or(GitError::ResultUnknown)?;
        let committed = CommitSha::parse(oid).map_err(|_| GitError::ResultUnknown)?;
        let actual = self
            .branch_head(request.branch)
            .await
            .map_err(|_| GitError::ResultUnknown)?;
        if actual != committed {
            return Err(GitError::ResultUnknown);
        }
        Ok(committed)
    }

    async fn read_text(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
    ) -> Result<String, GitError> {
        let data = self.graphql(
            "query($owner: String!, $name: String!, $expression: String!) { repository(owner: $owner, name: $name) { object(expression: $expression) { ... on Blob { byteSize isBinary isTruncated text } } } }",
            json!({"owner": self.owner, "name": self.name, "expression": format!("{}:{}", commit.as_str(), path.as_str())}), false,
        ).await?;
        let blob = data
            .pointer("/data/repository/object")
            .ok_or(GitError::InvalidResponse)?;
        if blob["isBinary"] != false
            || blob["isTruncated"] != false
            || blob["byteSize"].as_u64().ok_or(GitError::InvalidResponse)? > 2 * 1024 * 1024
        {
            return Err(GitError::InvalidResponse);
        }
        blob["text"]
            .as_str()
            .filter(|text| {
                text.len() <= 2 * 1024 * 1024
                    && Some(text.len() as u64) == blob["byteSize"].as_u64()
            })
            .map(str::to_owned)
            .ok_or(GitError::InvalidResponse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::HeaderMap,
        response::{IntoResponse, Response},
        routing::post,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    };
    use uuid::Uuid;

    const OLD: &str = "1111111111111111111111111111111111111111";
    const NEW: &str = "2222222222222222222222222222222222222222";

    struct ProviderState {
        head: Mutex<String>,
        mutations: Mutex<Vec<Value>>,
        mode: AtomicU8,
    }

    struct Fixture {
        adapter: GitHub,
        state: Arc<ProviderState>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn provider(
        State(state): State<Arc<ProviderState>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        assert_eq!(headers["authorization"], "Bearer fixture-token");
        if state.mode.load(Ordering::SeqCst) == 2 {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "17")],
                "rate limited",
            )
                .into_response();
        }
        let query = body["query"].as_str().unwrap();
        if query.starts_with("mutation") {
            let input = &body["variables"]["input"];
            state.mutations.lock().unwrap().push(input.clone());
            assert_eq!(input["branch"]["repositoryNameWithOwner"], "synthetic/book");
            assert!(
                input["branch"]["branchName"]
                    .as_str()
                    .unwrap()
                    .starts_with("change/")
            );
            if state.mode.load(Ordering::SeqCst) == 1 {
                return Json(
                    json!({"errors": [{"type": "STALE_DATA", "message": "Head changed"}]}),
                )
                .into_response();
            }
            assert_eq!(
                input["expectedHeadOid"].as_str().unwrap(),
                state.head.lock().unwrap().as_str()
            );
            *state.head.lock().unwrap() = NEW.into();
            return Json(json!({"data": {"createCommitOnBranch": {"commit": {"oid": NEW}}}}))
                .into_response();
        }
        if body["variables"].get("expression").is_some() {
            assert_eq!(body["variables"]["expression"], format!("{OLD}:docs/a.md"));
            return Json(json!({"data": {"repository": {"object": {
                "byteSize": 5, "text": "hello", "isBinary": false,
                "isTruncated": state.mode.load(Ordering::SeqCst) == 3
            }}}}))
            .into_response();
        }
        Json(json!({"data": {"repository": {"ref": {"target": {"oid": *state.head.lock().unwrap()}}}}})).into_response()
    }

    async fn create_ref(
        State(state): State<Arc<ProviderState>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        assert_eq!(body["ref"], "refs/heads/change/1-feature");
        assert_eq!(body["sha"], OLD);
        *state.head.lock().unwrap() = OLD.into();
        Json(json!({"object": {"sha": OLD}}))
    }

    async fn fixture() -> Fixture {
        let state = Arc::new(ProviderState {
            head: Mutex::new(OLD.into()),
            mutations: Mutex::new(vec![]),
            mode: AtomicU8::new(0),
        });
        let app = Router::new()
            .route("/graphql", post(provider))
            .route("/repos/synthetic/book/git/refs", post(create_ref))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let adapter = GitHub::configured(
            "synthetic/book",
            "fixture-token",
            &origin,
            &format!("{origin}/graphql"),
        )
        .unwrap();
        Fixture {
            adapter,
            state,
            server,
        }
    }

    #[tokio::test]
    async fn multiple_files_publish_as_one_expected_head_mutation() {
        let fixture = fixture().await;
        let branch = BranchName::parse("change/1-feature").unwrap();
        let head = CommitSha::parse(OLD).unwrap();
        assert_eq!(
            fixture.adapter.create_branch(&branch, &head).await.unwrap(),
            head
        );
        let changes = [
            FileChange::Put {
                path: RepositoryPath::parse("docs/a.md").unwrap(),
                content: "תוכן חדש".as_bytes().to_vec(),
            },
            FileChange::Put {
                path: RepositoryPath::parse("docs/b.md").unwrap(),
                content: b"second".to_vec(),
            },
            FileChange::Delete {
                path: RepositoryPath::parse("docs/removed.md").unwrap(),
            },
        ];
        let operation = Uuid::new_v4();
        let committed = fixture
            .adapter
            .commit(CommitRequest {
                operation_id: operation,
                branch: &branch,
                expected_head: &head,
                message: "Test change",
                actor_id: Uuid::new_v4(),
                changes: &changes,
            })
            .await
            .unwrap();
        assert_eq!(committed.as_str(), NEW);
        let mutations = fixture.state.mutations.lock().unwrap();
        assert_eq!(mutations.len(), 1);
        assert_eq!(
            mutations[0]["fileChanges"]["additions"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            mutations[0]["fileChanges"]["deletions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let decoded = STANDARD
            .decode(
                mutations[0]["fileChanges"]["additions"][0]["contents"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(decoded, "תוכן חדש".as_bytes());
        assert_eq!(mutations[0]["clientMutationId"], operation.to_string());
    }

    #[tokio::test]
    async fn stale_head_is_rejected_without_writing_and_unknown_results_are_not_retried() {
        let fixture = fixture().await;
        let branch = BranchName::parse("change/1-feature").unwrap();
        let head = CommitSha::parse(OLD).unwrap();
        let changes = [FileChange::Put {
            path: RepositoryPath::parse("docs/a.md").unwrap(),
            content: b"changed".to_vec(),
        }];
        let request = || CommitRequest {
            operation_id: Uuid::new_v4(),
            branch: &branch,
            expected_head: &head,
            message: "Test",
            actor_id: Uuid::new_v4(),
            changes: &changes,
        };
        *fixture.state.head.lock().unwrap() = NEW.into();
        assert_eq!(
            fixture.adapter.commit(request()).await.unwrap_err(),
            GitError::HeadMismatch
        );
        assert!(fixture.state.mutations.lock().unwrap().is_empty());
        *fixture.state.head.lock().unwrap() = OLD.into();
        fixture.state.mode.store(1, Ordering::SeqCst);
        assert_eq!(
            fixture.adapter.commit(request()).await.unwrap_err(),
            GitError::ResultUnknown
        );
        assert_eq!(fixture.state.mutations.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn read_is_commit_pinned_and_rejects_truncated_content() {
        let fixture = fixture().await;
        let head = CommitSha::parse(OLD).unwrap();
        let path = RepositoryPath::parse("docs/a.md").unwrap();
        assert_eq!(
            fixture.adapter.read_text(&head, &path).await.unwrap(),
            "hello"
        );
        fixture.state.mode.store(3, Ordering::SeqCst);
        assert_eq!(
            fixture.adapter.read_text(&head, &path).await.unwrap_err(),
            GitError::InvalidResponse
        );
    }

    #[tokio::test]
    async fn rate_limit_preserves_retry_after_without_automatic_retries() {
        let fixture = fixture().await;
        fixture.state.mode.store(2, Ordering::SeqCst);
        assert_eq!(
            fixture.adapter.main_head().await.unwrap_err(),
            GitError::RateLimited {
                retry_after_seconds: Some(17)
            }
        );
    }
}
