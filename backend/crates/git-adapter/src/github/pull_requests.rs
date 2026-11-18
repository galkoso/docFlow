use super::*;
use crate::pull_requests::{
    MAX_PR_BODY_BYTES, MAX_PR_PAGES, PullRequest, PullRequestContent, PullRequestError,
    PullRequestListing, PullRequestNumber, PullRequestPage, PullRequestProvider, PullRequestScope,
    PullRequestState,
};

impl GitHub {
    fn pr_scope(&self, scope: &PullRequestScope) -> Result<(), PullRequestError> {
        if self.repository_id != Some(scope.repository_id()) {
            return Err(PullRequestError::InvalidInput);
        }
        Ok(())
    }
    fn pr_value(
        &self,
        scope: &PullRequestScope,
        value: &Value,
    ) -> Result<PullRequest, PullRequestError> {
        let invalid = PullRequestError::InvalidResponse;
        let positive = |value: &Value| {
            value
                .as_u64()
                .filter(|number| *number > 0)
                .ok_or(PullRequestError::InvalidResponse)
        };
        let number = PullRequestNumber::new(positive(&value["number"])?)
            .map_err(|_| PullRequestError::InvalidResponse)?;
        let id = positive(&value["id"])?;
        let node_id = value["node_id"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 255
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-=+/".contains(&b))
            })
            .ok_or(PullRequestError::InvalidResponse)?;
        for side in ["head", "base"] {
            if positive(&value[side]["repo"]["id"])? != scope.repository_id()
                || !value[side]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&self.repository))
            {
                return Err(PullRequestError::InvalidResponse);
            }
        }
        if value["head"]["ref"].as_str() != Some(scope.branch().as_str())
            || value["base"]["ref"].as_str() != Some("main")
        {
            return Err(invalid);
        }
        let sha = |value: &Value| {
            value
                .as_str()
                .ok_or(PullRequestError::InvalidResponse)
                .and_then(|sha| {
                    CommitSha::parse(sha).map_err(|_| PullRequestError::InvalidResponse)
                })
        };
        let head = sha(&value["head"]["sha"])?;
        let target = sha(&value["base"]["sha"])?;
        let merged = match value.get("merged_at") {
            Some(Value::Null) => false,
            Some(Value::String(time)) if chrono::DateTime::parse_from_rfc3339(time).is_ok() => true,
            _ => return Err(PullRequestError::InvalidResponse),
        };
        let state = match (value["state"].as_str(), merged) {
            (Some("open"), false) => PullRequestState::Open,
            (Some("closed"), false) => PullRequestState::Closed,
            (Some("closed"), true) => PullRequestState::Merged,
            _ => return Err(PullRequestError::InvalidResponse),
        };
        if value
            .get("merged")
            .is_some_and(|v| v.as_bool() != Some(merged))
        {
            return Err(PullRequestError::InvalidResponse);
        }
        let draft = value["draft"]
            .as_bool()
            .ok_or(PullRequestError::InvalidResponse)?;
        let title = value["title"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
            .ok_or(PullRequestError::InvalidResponse)?;
        let body = match value.get("body") {
            Some(Value::Null) => "",
            Some(Value::String(body))
                if body.len() <= MAX_PR_BODY_BYTES && !body.contains('\0') =>
            {
                body
            }
            _ => return Err(PullRequestError::InvalidResponse),
        };
        let url = format!(
            "https://github.com/{}/pull/{}",
            self.repository,
            number.get()
        );
        if !value["html_url"]
            .as_str()
            .is_some_and(|provided| provided.eq_ignore_ascii_case(&url))
        {
            return Err(PullRequestError::InvalidResponse);
        }
        Ok(PullRequest {
            id,
            node_id: node_id.to_owned(),
            number,
            repository_id: scope.repository_id(),
            branch: scope.branch().clone(),
            head,
            target,
            state,
            draft,
            title: title.to_owned(),
            body: body.to_owned(),
            url,
        })
    }
    async fn pr_response(response: Response, mutation: bool) -> Result<Value, PullRequestError> {
        if response.status() == StatusCode::NOT_FOUND {
            return Err(PullRequestError::NotFound);
        }
        if !mutation && response.status().is_success() && response.status() != StatusCode::OK {
            return Err(PullRequestError::InvalidResponse);
        }
        if mutation && response.status().is_redirection() {
            return Err(PullRequestError::Provider(GitError::ResultUnknown));
        }
        Self::response(response, mutation)
            .await
            .map_err(PullRequestError::Provider)
    }
    fn pr_written(
        &self,
        scope: &PullRequestScope,
        value: &Value,
        content: &PullRequestContent,
        expected: Option<&PullRequest>,
    ) -> Result<PullRequest, PullRequestError> {
        let uncertain = || PullRequestError::Provider(GitError::ResultUnknown);
        let pr = self.pr_value(scope, value).map_err(|_| uncertain())?;
        if pr.state != PullRequestState::Open
            || pr.title != content.title()
            || pr.body != content.body()
            || expected.is_none() && !pr.draft
            || expected.is_some_and(|old| {
                old.id != pr.id
                    || old.number != pr.number
                    || old.node_id != pr.node_id
                    || old.draft != pr.draft
            })
        {
            return Err(uncertain());
        }
        Ok(pr)
    }
}
#[async_trait]
impl PullRequestProvider for GitHub {
    async fn pull_request(
        &self,
        scope: &PullRequestScope,
        number: PullRequestNumber,
    ) -> Result<PullRequest, PullRequestError> {
        self.pr_scope(scope)?;
        let response = self
            .client
            .get(format!(
                "{}/repos/{}/pulls/{}",
                self.rest_base,
                self.repository,
                number.get()
            ))
            .send()
            .await
            .map_err(|_| PullRequestError::Provider(GitError::ProviderUnavailable))?;
        let value = Self::pr_response(response, false).await?;
        let pr = self.pr_value(scope, &value)?;
        if pr.number != number {
            return Err(PullRequestError::InvalidResponse);
        }
        Ok(pr)
    }
    async fn pull_requests(
        &self,
        scope: &PullRequestScope,
        page: PullRequestPage,
    ) -> Result<PullRequestListing, PullRequestError> {
        self.pr_scope(scope)?;
        let response = self
            .client
            .get(format!(
                "{}/repos/{}/pulls",
                self.rest_base, self.repository
            ))
            .query(&[
                ("state", "all".to_owned()),
                (
                    "head",
                    format!("{}:{}", self.owner, scope.branch().as_str()),
                ),
                ("sort", "created".to_owned()),
                ("direction", "asc".to_owned()),
                ("per_page", page.limit().to_string()),
                ("page", page.page().to_string()),
            ])
            .send()
            .await
            .map_err(|_| PullRequestError::Provider(GitError::ProviderUnavailable))?;
        let value = Self::pr_response(response, false).await?;
        let values = value
            .as_array()
            .filter(|items| items.len() <= page.limit() as usize)
            .ok_or(PullRequestError::InvalidResponse)?;
        let mut numbers = HashSet::new();
        let mut ids = HashSet::new();
        let mut items = vec![];
        for value in values {
            let pr = self.pr_value(scope, value)?;
            if !numbers.insert(pr.number.get()) || !ids.insert(pr.id) {
                return Err(PullRequestError::InvalidResponse);
            }
            items.push(pr);
        }
        let next = if items.len() == page.limit() as usize {
            if page.page() == MAX_PR_PAGES {
                return Err(PullRequestError::Provider(GitError::ResponseTooLarge));
            }
            Some(PullRequestPage::new(page.page() + 1, page.limit())?)
        } else {
            None
        };
        Ok(PullRequestListing { items, next })
    }
    async fn create_pull_request(
        &self,
        scope: &PullRequestScope,
        content: &PullRequestContent,
    ) -> Result<PullRequest, PullRequestError> {
        self.pr_scope(scope)?;
        let response = self.client.post(format!("{}/repos/{}/pulls", self.rest_base, self.repository))
            .json(&json!({"title": content.title(), "body": content.body(), "head": scope.branch().as_str(), "base": "main", "draft": true, "maintainer_can_modify": false}))
            .send().await.map_err(|_| PullRequestError::Provider(GitError::ResultUnknown))?;
        if response.status().is_success() && response.status() != StatusCode::CREATED {
            return Err(PullRequestError::Provider(GitError::ResultUnknown));
        }
        let value = Self::pr_response(response, true).await?;
        self.pr_written(scope, &value, content, None)
    }
    async fn update_pull_request(
        &self,
        scope: &PullRequestScope,
        number: PullRequestNumber,
        content: &PullRequestContent,
    ) -> Result<PullRequest, PullRequestError> {
        let old = self.pull_request(scope, number).await?;
        if old.state != PullRequestState::Open {
            return Err(PullRequestError::NotOpen);
        }
        let response = self
            .client
            .patch(format!(
                "{}/repos/{}/pulls/{}",
                self.rest_base,
                self.repository,
                number.get()
            ))
            .json(&json!({"title": content.title(), "body": content.body()}))
            .send()
            .await
            .map_err(|_| PullRequestError::Provider(GitError::ResultUnknown))?;
        if response.status().is_success() && response.status() != StatusCode::OK {
            return Err(PullRequestError::Provider(GitError::ResultUnknown));
        }
        let value = Self::pr_response(response, true).await?;
        self.pr_written(scope, &value, content, Some(&old))
    }
}

#[cfg(test)]
mod tests;
