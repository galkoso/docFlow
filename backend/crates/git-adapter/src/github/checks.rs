use super::*;
use crate::checks::{
    CHECK_NAME, CheckError, CheckListing, CheckPage, CheckResult, CheckRun, CheckRunId, CheckScope,
    CheckState, MAX_CHECK_PAGES, ReviewCheckProvider, external_id,
};
use uuid::Uuid;

impl GitHub {
    fn check_scope(&self, scope: &CheckScope) -> Result<(), CheckError> {
        if self.repository_id != Some(scope.repository_id()) {
            return Err(CheckError::InvalidInput);
        }
        Ok(())
    }
    fn check_value(&self, scope: &CheckScope, v: &Value) -> Result<CheckRun, CheckError> {
        let invalid = || CheckError::InvalidResponse;
        let id = CheckRunId::new(v["id"].as_u64().ok_or_else(invalid)?).map_err(|_| invalid())?;
        if v["name"].as_str() != Some(CHECK_NAME)
            || v["head_sha"].as_str() != Some(scope.head().as_str())
            || v["app"]["id"].as_u64() != Some(scope.app_id())
        {
            return Err(CheckError::IdentityMismatch);
        }
        let raw = v["external_id"].as_str().ok_or_else(invalid)?;
        let operation = raw
            .strip_prefix("specspace-review-policy:")
            .and_then(|s| Uuid::parse_str(s).ok())
            .filter(|id| !id.is_nil())
            .ok_or_else(invalid)?;
        if raw != external_id(operation)? {
            return Err(invalid());
        }
        let state = match (v["status"].as_str(), v.get("conclusion")) {
            (Some("queued" | "in_progress"), Some(Value::Null)) => CheckState::Pending,
            (Some("completed"), Some(Value::String(value))) => match value.as_str() {
                "success" => CheckState::Success,
                "failure" => CheckState::Failure,
                "neutral" | "cancelled" | "skipped" | "timed_out" | "action_required" | "stale" => {
                    CheckState::Other
                }
                _ => return Err(invalid()),
            },
            _ => return Err(invalid()),
        };
        if state != CheckState::Pending
            && !v["completed_at"]
                .as_str()
                .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
        {
            return Err(invalid());
        }
        Ok(CheckRun {
            id,
            scope: scope.clone(),
            operation,
            state,
        })
    }
    async fn check_response(
        response: Response,
        mutation: bool,
        expected: StatusCode,
    ) -> Result<Value, CheckError> {
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(CheckError::NotFound);
        }
        if status.is_success() && status != expected || mutation && status.is_redirection() {
            return Err(if mutation {
                CheckError::Provider(GitError::ResultUnknown)
            } else {
                CheckError::InvalidResponse
            });
        }
        Self::response(response, mutation)
            .await
            .map_err(CheckError::Provider)
    }
    fn check_written(
        &self,
        scope: &CheckScope,
        value: &Value,
        id: Option<CheckRunId>,
        operation: Uuid,
        result: CheckResult,
    ) -> Result<CheckRun, CheckError> {
        let unknown = || CheckError::Provider(GitError::ResultUnknown);
        let run = self.check_value(scope, value).map_err(|_| unknown())?;
        if id.is_some_and(|id| run.id != id)
            || run.operation != operation
            || run.state != CheckState::from(result)
            || value["output"]["title"].as_str() != Some(result.title())
            || value["output"]["summary"].as_str() != Some(result.summary())
        {
            return Err(unknown());
        }
        Ok(run)
    }
}
#[async_trait]
impl ReviewCheckProvider for GitHub {
    async fn check_run(&self, scope: &CheckScope, id: CheckRunId) -> Result<CheckRun, CheckError> {
        self.check_scope(scope)?;
        let response = self
            .client
            .get(format!(
                "{}/repos/{}/check-runs/{}",
                self.rest_base,
                self.repository,
                id.get()
            ))
            .send()
            .await
            .map_err(|_| CheckError::Provider(GitError::ProviderUnavailable))?;
        let value = Self::check_response(response, false, StatusCode::OK).await?;
        let run = self.check_value(scope, &value)?;
        if run.id != id {
            return Err(CheckError::IdentityMismatch);
        }
        Ok(run)
    }
    async fn check_runs(
        &self,
        scope: &CheckScope,
        page: CheckPage,
    ) -> Result<CheckListing, CheckError> {
        self.check_scope(scope)?;
        let response = self
            .client
            .get(format!(
                "{}/repos/{}/commits/{}/check-runs",
                self.rest_base,
                self.repository,
                scope.head().as_str()
            ))
            .query(&[
                ("check_name", CHECK_NAME.to_owned()),
                ("filter", "all".into()),
                ("app_id", scope.app_id().to_string()),
                ("per_page", page.limit().to_string()),
                ("page", page.page().to_string()),
            ])
            .send()
            .await
            .map_err(|_| CheckError::Provider(GitError::ProviderUnavailable))?;
        let value = Self::check_response(response, false, StatusCode::OK).await?;
        let total_count = value["total_count"]
            .as_u64()
            .ok_or(CheckError::InvalidResponse)?;
        let rows = value["check_runs"]
            .as_array()
            .filter(|rows| rows.len() <= usize::from(page.limit()))
            .ok_or(CheckError::InvalidResponse)?;
        let end = u64::from(page.page() - 1) * u64::from(page.limit()) + rows.len() as u64;
        if end > total_count || end < total_count && rows.len() != usize::from(page.limit()) {
            return Err(CheckError::InvalidResponse);
        }
        let mut ids = HashSet::new();
        let mut items = Vec::new();
        for row in rows {
            let run = self.check_value(scope, row)?;
            if !ids.insert(run.id) {
                return Err(CheckError::InvalidResponse);
            }
            items.push(run);
        }
        let next = if end < total_count {
            if page.page() == MAX_CHECK_PAGES {
                return Err(CheckError::Provider(GitError::ResponseTooLarge));
            }
            Some(CheckPage::new(page.page() + 1, page.limit())?)
        } else {
            None
        };
        Ok(CheckListing {
            items,
            next,
            total_count,
        })
    }
    async fn create_check(
        &self,
        scope: &CheckScope,
        operation: Uuid,
    ) -> Result<CheckRun, CheckError> {
        self.check_scope(scope)?;
        let external = external_id(operation)?;
        let result = CheckResult::Pending;
        let response = self.client.post(format!("{}/repos/{}/check-runs", self.rest_base, self.repository))
            .json(&json!({"name":CHECK_NAME,"head_sha":scope.head().as_str(),"external_id":external,"status":"in_progress","output":{"title":result.title(),"summary":result.summary()}}))
            .send().await.map_err(|_| CheckError::Provider(GitError::ResultUnknown))?;
        let value = Self::check_response(response, true, StatusCode::CREATED).await?;
        self.check_written(scope, &value, None, operation, result)
    }
    async fn update_check(
        &self,
        scope: &CheckScope,
        id: CheckRunId,
        operation: Uuid,
        result: CheckResult,
    ) -> Result<CheckRun, CheckError> {
        external_id(operation)?;
        let old = self.check_run(scope, id).await?;
        if old.operation != operation {
            return Err(CheckError::IdentityMismatch);
        }
        if result == CheckResult::Pending && old.state != CheckState::Pending {
            return Err(CheckError::CannotRestart);
        }
        let mut body = json!({"status": if result == CheckResult::Pending {"in_progress"} else {"completed"},"output":{"title":result.title(),"summary":result.summary()}});
        if result != CheckResult::Pending {
            body["conclusion"] = json!(if result == CheckResult::Success {
                "success"
            } else {
                "failure"
            });
        }
        let response = self
            .client
            .patch(format!(
                "{}/repos/{}/check-runs/{}",
                self.rest_base,
                self.repository,
                id.get()
            ))
            .json(&body)
            .send()
            .await
            .map_err(|_| CheckError::Provider(GitError::ResultUnknown))?;
        let value = Self::check_response(response, true, StatusCode::OK).await?;
        self.check_written(scope, &value, Some(id), operation, result)
    }
}

#[cfg(test)]
mod tests;
