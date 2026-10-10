//! One method per Remind API operation (contract 2). Paths are relative to `/api/v2`.
use crate::{Client, Error, Mutation, Result, models};
use reqwest::Method;
use std::time::Duration;
use uuid::Uuid;

fn segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn silicon_query(silicon: Option<&str>) -> Vec<(&'static str, String)> {
    silicon
        .map(|s| vec![("silicon_id", s.trim().to_owned())])
        .unwrap_or_default()
}

impl Client {
    /// Sends one bounded, best-effort operational event (Space Station, through Remind).
    /// Does nothing without a signed-in session or after opting out; never fails the caller.
    pub async fn track(&self, event: &models::TelemetryEvent) {
        if !self.telemetry_enabled() || !self.has_credential() {
            return;
        }
        if let Ok(request) = self.request(Method::POST, "/telemetry/events") {
            let _ = request
                .timeout(Duration::from_millis(250))
                .json(event)
                .send()
                .await;
        }
    }

    /// `GET /api/versions`: the contracts this Remind serves and their deprecation state.
    pub async fn versions(&self) -> Result<serde_json::Value> {
        self.json(self.request(Method::GET, "/api/versions")?).await
    }

    /// `GET /health/live` or `/health/ready`.
    pub async fn health(&self, ready: bool) -> Result<models::Health> {
        let path = if ready {
            "/health/ready"
        } else {
            "/health/live"
        };
        self.json(self.request(Method::GET, path)?).await
    }

    /// `GET /auth/me`: the signed-in account as Remind sees it.
    pub async fn me(&self) -> Result<models::Identity> {
        self.json(self.request(Method::GET, "/auth/me")?).await
    }

    /// Checks the attached credential with Remind. Only an HTTP 401 becomes
    /// `authenticated: false`; every other failure stays an error.
    pub async fn login_status(&self) -> Result<models::LoginStatus> {
        match self.me().await {
            Ok(identity) => Ok(models::LoginStatus {
                authenticated: true,
                identity: Some(identity),
            }),
            Err(Error::Api { status: 401, .. }) => Ok(models::LoginStatus::default()),
            Err(error) => Err(error),
        }
    }

    /// `POST /reports`: queue a bug report email (simulated inside a test environment).
    pub async fn report(
        &self,
        input: &models::BugReportRequest,
        mutation: &Mutation,
    ) -> Result<models::BugReportResponse> {
        self.json(
            self.mutation(Method::POST, "/reports", mutation)?
                .json(input),
        )
        .await
    }

    /// `GET /reports/{id}`: delivery state of a report you sent.
    pub async fn report_status(&self, id: Uuid) -> Result<models::BugReportResponse> {
        self.json(self.request(Method::GET, &format!("/reports/{id}"))?)
            .await
    }

    /// `POST /schedules`: create a reminder for the signed-in Silicon. The timezone is
    /// mandatory and checked before anything is sent.
    pub async fn create_reminder(
        &self,
        input: &models::CreateScheduleRequest,
        mutation: &Mutation,
    ) -> Result<models::ScheduleResponse> {
        if input.timezone.trim().is_empty() {
            return Err(Error::Invalid(models::TIMEZONE_REQUIRED_MESSAGE.into()));
        }
        self.json(
            self.mutation(Method::POST, "/schedules", mutation)?
                .json(input),
        )
        .await
    }

    /// `GET /schedules`: the reminders the caller can read, filtered before pagination.
    pub async fn reminders(
        &self,
        query: &models::ListSchedules,
    ) -> Result<models::Page<models::ScheduleResponse>> {
        self.json(self.request(Method::GET, "/schedules")?.query(query))
            .await
    }

    /// `GET /schedules/{id}`.
    pub async fn reminder(&self, id: Uuid) -> Result<models::ScheduleResponse> {
        self.json(self.request(Method::GET, &format!("/schedules/{id}"))?)
            .await
    }

    /// `PATCH /schedules/{id}`: change text, cron, timezone, kind or status of your reminder.
    pub async fn update_reminder(
        &self,
        id: Uuid,
        input: &models::PatchScheduleRequest,
        mutation: &Mutation,
    ) -> Result<models::ScheduleResponse> {
        self.json(
            self.mutation(Method::PATCH, &format!("/schedules/{id}"), mutation)?
                .json(input),
        )
        .await
    }

    /// `PATCH /schedules`: pause or resume 1 to 100 of your reminders, all or nothing.
    pub async fn set_status(
        &self,
        ids: Vec<Uuid>,
        status: models::ScheduleStatus,
        mutation: &Mutation,
    ) -> Result<models::StatusBatch> {
        let body = models::BulkScheduleStatusRequest {
            schedule_ids: ids,
            status,
        };
        self.json(
            self.mutation(Method::PATCH, "/schedules", mutation)?
                .json(&body),
        )
        .await
    }

    /// `DELETE /schedules/{id}`: archive your reminder (readable for 45 days).
    pub async fn archive_reminder(&self, id: Uuid) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/schedules/{id}"))?)
            .await
    }

    /// `GET /schedules/{id}/executions`: delivery history of a reminder you can read.
    pub async fn executions(
        &self,
        id: Uuid,
        paging: &models::Paging,
    ) -> Result<models::Page<models::ExecutionResponse>> {
        self.json(
            self.request(Method::GET, &format!("/schedules/{id}/executions"))?
                .query(paging),
        )
        .await
    }

    /// `POST /webhooks`: add a delivery subscription for the signed-in Silicon.
    pub async fn subscribe_webhook(
        &self,
        input: &models::Destination,
    ) -> Result<models::DestinationReceipt> {
        self.json(self.request(Method::POST, "/webhooks")?.json(input))
            .await
    }

    /// `PUT /webhook`: the older single-endpoint form of [`Client::subscribe_webhook`].
    pub async fn configure_webhook(
        &self,
        input: &models::Destination,
    ) -> Result<models::DestinationReceipt> {
        self.json(self.request(Method::PUT, "/webhook")?.json(input))
            .await
    }

    /// `GET /webhooks`: a Silicon's own subscriptions, or (for a Carbon, read-only) those of
    /// the Silicons it looks after; `silicon` narrows that to one of them.
    pub async fn webhooks(&self, silicon: Option<&str>) -> Result<models::WebhookSubscriptions> {
        self.json(
            self.request(Method::GET, "/webhooks")?
                .query(&silicon_query(silicon)),
        )
        .await
    }

    /// `DELETE /webhooks/{id}`: end one subscription of the signed-in Silicon.
    pub async fn unsubscribe_webhook(&self, id: Uuid) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/webhooks/{id}"))?)
            .await
    }

    /// `GET /webhook`: the first active subscription (older single-endpoint form).
    pub async fn webhook(&self) -> Result<models::DestinationInfo> {
        self.json(self.request(Method::GET, "/webhook")?).await
    }

    /// `DELETE /webhook`: end every subscription of the signed-in Silicon.
    pub async fn disable_webhook(&self) -> Result<()> {
        self.empty(self.request(Method::DELETE, "/webhook")?).await
    }

    /// `GET /silicons`: the Silicons whose reminders the caller can read, by uuid.
    pub async fn silicons(
        &self,
        after: Option<&str>,
        limit: u32,
    ) -> Result<models::Page<models::VisibleSilicon>> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(after) = after {
            query.push(("after", after.to_owned()));
        }
        self.json(self.request(Method::GET, "/silicons")?.query(&query))
            .await
    }

    /// `GET /viewers`: grants on your (or your Silicons') reminders, and grants you received.
    pub async fn viewers(&self) -> Result<models::ViewerGrants> {
        self.json(self.request(Method::GET, "/viewers")?).await
    }

    /// `POST /viewers`: let an account (by `c:`/`si:` id or uuid) read a Silicon's reminders.
    pub async fn grant_viewer(
        &self,
        target: &models::AccountTarget,
    ) -> Result<models::ViewerGrant> {
        self.json(self.request(Method::POST, "/viewers")?.json(target))
            .await
    }

    /// `DELETE /viewers/{viewer}`: stop sharing a Silicon's reminders with an account.
    pub async fn revoke_viewer(&self, viewer: &str, silicon: Option<&str>) -> Result<()> {
        self.empty(
            self.request(
                Method::DELETE,
                &format!("/viewers/{}", segment(viewer.trim())),
            )?
            .query(&silicon_query(silicon)),
        )
        .await
    }

    /// `GET /allowed-accounts`: accounts from outside a Silicon's circle allowed to share
    /// reminders with it.
    pub async fn allowed_accounts(&self, silicon: Option<&str>) -> Result<models::Allowances> {
        self.json(
            self.request(Method::GET, "/allowed-accounts")?
                .query(&silicon_query(silicon)),
        )
        .await
    }

    /// `POST /allowed-accounts`: allow an account to share reminders with a Silicon.
    pub async fn allow_account(&self, target: &models::AccountTarget) -> Result<models::Allowance> {
        self.json(
            self.request(Method::POST, "/allowed-accounts")?
                .json(target),
        )
        .await
    }

    /// `DELETE /allowed-accounts/{account}`: remove an account from a Silicon's allow-list.
    pub async fn disallow_account(&self, account: &str, silicon: Option<&str>) -> Result<()> {
        self.empty(
            self.request(
                Method::DELETE,
                &format!("/allowed-accounts/{}", segment(account.trim())),
            )?
            .query(&silicon_query(silicon)),
        )
        .await
    }
}
