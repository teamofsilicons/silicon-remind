//! Public Remind wire types (contract 2). No backend or database dependency.
//!
//! Accounts are identified by their permanent Silicon Accounts `uuid` (a 128-bit UUIDv4
//! in canonical lowercase, hyphenated form) and shown by their current public id (`c:ada`,
//! `si:scout`), which can change.
use crate::Secret;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Reminder occurrence mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleKind {
    /// Fires at the first future match, then moves to the archive.
    OneTime,
    /// Fires at every match.
    Recurring,
}
/// Reminder lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleStatus {
    /// Firing.
    Active,
    /// Kept, not firing.
    Paused,
    /// A one-time reminder that fired.
    Completed,
}
/// Current or archived reminder view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleSection {
    /// Active and paused reminders.
    #[default]
    Current,
    /// Archived and fired one-time reminders, kept 45 days.
    Archived,
}
/// Forward-compatible delivery status (`pending`, `delivered`, `retrying`, `failed`).
pub type ExecutionStatus = String;
pub(crate) const TIMEZONE_REQUIRED_MESSAGE: &str = "Providing a timezone is mandatory. Set CreateScheduleRequest.timezone to an IANA timezone identifier, for example \"Asia/Kolkata\" (the `timezone` field when using JSON).";

/// Carbon (a person) or Silicon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// A Carbon (`c:` ids).
    Carbon,
    /// A Silicon (`si:` ids).
    Silicon,
}
impl AccountKind {
    /// `carbon` or `silicon`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Carbon => "carbon",
            Self::Silicon => "silicon",
        }
    }
    /// `Carbon` or `Silicon`.
    pub fn title(self) -> &'static str {
        match self {
            Self::Carbon => "Carbon",
            Self::Silicon => "Silicon",
        }
    }
}

/// An account as Remind shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRef {
    /// Permanent Silicon Accounts identifier.
    pub uuid: String,
    /// Current public id; empty when Remind does not know it yet.
    #[serde(default)]
    pub id: String,
    /// Carbon or Silicon, when known.
    #[serde(default)]
    pub kind: Option<AccountKind>,
}

/// A new reminder. The signed-in Silicon owns it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "CreateScheduleInput")]
pub struct CreateScheduleRequest {
    /// Reminder content, delivered when it fires.
    pub text: String,
    /// One-time or recurring.
    pub kind: ScheduleKind,
    /// Required IANA timezone identifier, for example `Asia/Kolkata` or `UTC`.
    pub timezone: String,
    /// Five-field Linux cron expression (`minute hour day-of-month month day-of-week`).
    pub cron: String,
}

#[derive(Deserialize)]
struct CreateScheduleInput {
    text: String,
    kind: ScheduleKind,
    timezone: Option<String>,
    cron: String,
}

impl TryFrom<CreateScheduleInput> for CreateScheduleRequest {
    type Error = &'static str;

    fn try_from(input: CreateScheduleInput) -> std::result::Result<Self, Self::Error> {
        let timezone = input
            .timezone
            .filter(|value| !value.trim().is_empty())
            .ok_or(TIMEZONE_REQUIRED_MESSAGE)?;
        Ok(Self {
            text: input.text,
            kind: input.kind,
            timezone,
            cron: input.cron,
        })
    }
}

/// A reminder.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleResponse {
    /// Reminder id.
    pub id: Uuid,
    /// The Silicon that owns it; `None` only for a reminder kept from before Remind signed in
    /// with Silicon Accounts whose owner is not matched to an account yet.
    #[serde(default)]
    pub owner: Option<AccountRef>,
    /// The owner's current `si:` id.
    pub silicon_id: String,
    /// Reminder content.
    pub text: String,
    /// IANA timezone identifier.
    pub timezone: String,
    /// `one_time` or `recurring`.
    pub kind: String,
    /// Five-field Linux cron expression.
    pub cron: String,
    /// `active`, `paused` or `completed`.
    pub status: String,
    /// Current or archived.
    pub section: ScheduleSection,
    /// Next due instant, if any.
    pub next_run_at: Option<DateTime<Utc>>,
    /// When it entered the archive.
    pub archived_at: Option<DateTime<Utc>>,
    /// When an archived reminder is deleted for good.
    pub purge_after: Option<DateTime<Utc>>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

/// One delivery occurrence of a reminder.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionResponse {
    /// Stable occurrence id (also the delivery's idempotency key).
    pub id: Uuid,
    /// Parent reminder.
    pub schedule_id: Uuid,
    /// Intended occurrence instant.
    pub scheduled_for: DateTime<Utc>,
    /// Most recent attempt.
    pub attempted_at: Option<DateTime<Utc>>,
    /// When a receiver accepted it (not downstream acknowledgment).
    pub delivered_at: Option<DateTime<Utc>>,
    /// Delivery state.
    pub status: ExecutionStatus,
    /// Receipt id from the receiver, when it sent one.
    pub hook_event_id: Option<Uuid>,
    /// Sanitized failure reason.
    pub failure_reason: Option<String>,
}

/// Pause or resume several reminders at once.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BulkScheduleStatusRequest {
    /// Reminder ids, in the order results come back.
    pub schedule_ids: Vec<Uuid>,
    /// `active` or `paused`.
    pub status: ScheduleStatus,
}

/// Partial change; omitted fields keep their current values.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PatchScheduleRequest {
    /// New text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// New cron expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// New IANA timezone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// New kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ScheduleKind>,
    /// `active` or `paused`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ScheduleStatus>,
}

/// One page; pass `next_cursor` unchanged into the next request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    /// The items.
    pub items: Vec<T>,
    /// Cursor for the next page, `None` at the end.
    pub next_cursor: Option<String>,
}

/// Filters applied before pagination.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ListSchedules {
    /// One Silicon, by `si:` id or uuid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silicon_id: Option<String>,
    /// Current (default) or archived.
    pub section: ScheduleSection,
    /// Only this status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ScheduleStatus>,
    /// The previous page's `next_cursor`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Page size, 1 to 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Page size and cursor.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Paging {
    /// The previous page's `next_cursor`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Page size, 1 to 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// The calling account as Remind sees it (`GET /auth/me`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    /// Permanent Silicon Accounts identifier.
    pub uuid: String,
    /// Carbon or Silicon.
    pub kind: AccountKind,
    /// Current public id.
    pub id: String,
    /// Display name (empty when unknown).
    #[serde(default)]
    pub display_name: String,
    /// Profile photo URL (empty when none).
    #[serde(default)]
    pub pfp_url: String,
    /// The Carbon who looks after the calling Silicon; `None` for a Carbon.
    #[serde(default)]
    pub custodian: Option<AccountRef>,
    /// True for a Silicon signed in with an access token: it may create and change its own
    /// reminders. Carbons read.
    pub can_manage_reminders: bool,
    /// `access_token` or `proof`.
    pub credential: String,
    /// The app that holds the verification proof; `None` for an access token.
    #[serde(default)]
    pub issuing_app: Option<String>,
    /// How many Silicons' reminders the caller can read.
    pub visible_silicons: i64,
}

/// Result of [`crate::Client::login_status`]. Identity fields appear only when verified.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LoginStatus {
    /// Whether Remind accepted the credential.
    pub authenticated: bool,
    /// Who it is.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
}

/// A Silicon whose reminders the caller can read (`GET /silicons`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VisibleSilicon {
    /// Permanent Silicon Accounts identifier (also the page cursor).
    pub uuid: String,
    /// Current `si:` id; empty when Remind does not know it yet.
    #[serde(default)]
    pub silicon_id: String,
    /// Display name.
    #[serde(default)]
    pub display_name: String,
    /// Profile photo URL.
    #[serde(default)]
    pub pfp_url: String,
    /// `self`, `custodian` (a Silicon you look after), `sibling` (another Silicon of your
    /// custodian) or `shared` (it shared its reminders with you).
    pub relation: String,
    /// Retained reminders.
    pub reminder_count: i64,
}

/// Names another account, and for a Carbon which of its Silicons the request is about.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountTarget {
    /// The other account: `c:handle`, `si:handle` or uuid.
    pub id: String,
    /// Which Silicon this is about. Required for a Carbon (one it looks after); a Silicon
    /// may omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silicon_id: Option<String>,
}

/// An account allowed to read a Silicon's reminders.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewerGrant {
    /// Grant id.
    pub id: Uuid,
    /// The Silicon whose reminders are shared.
    pub owner: AccountRef,
    /// Who may read them.
    pub viewer: AccountRef,
    /// The uuid of the account that granted it (the Silicon or its custodian).
    pub granted_by: String,
    /// When.
    pub created_at: DateTime<Utc>,
}

/// Grants you gave (on your or your Silicons' reminders) and grants you received.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViewerGrants {
    /// Grants on reminders you own or look after.
    pub granted: Vec<ViewerGrant>,
    /// Grants that let you read someone else's reminders.
    pub received: Vec<ViewerGrant>,
}

/// An account from outside a Silicon's circle allowed to share reminders with it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Allowance {
    /// Entry id.
    pub id: Uuid,
    /// The Silicon.
    pub silicon: AccountRef,
    /// The allowed account.
    pub allowed: AccountRef,
    /// The uuid of the account that added it.
    pub created_by: String,
    /// When.
    pub created_at: DateTime<Utc>,
}

/// A Silicon's allow-list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Allowances {
    /// Active entries.
    pub items: Vec<Allowance>,
}

/// A delivery endpoint for the signed-in Silicon's reminders.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Destination {
    /// Absolute https URL (plain http only for this machine outside production).
    pub endpoint_url: String,
    /// Optional HMAC signing secret; never returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_secret: Option<Secret>,
}

/// A created subscription; the signing secret is never returned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationReceipt {
    /// Subscription id.
    pub id: Uuid,
    /// The Silicon's current `si:` id.
    pub silicon_id: String,
    /// The Silicon's uuid.
    #[serde(default)]
    pub silicon_uuid: String,
    /// Revision.
    pub version: i64,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

/// The first active subscription (`GET /webhook`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationInfo {
    /// Subscription id.
    pub id: Uuid,
    /// The Silicon's current `si:` id.
    pub silicon_id: String,
    /// Where deliveries go.
    pub endpoint_url: String,
    /// Revision.
    pub version: i64,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

/// One delivery subscription (`GET /webhooks`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookSubscription {
    /// Subscription id.
    pub id: Uuid,
    /// The Silicon's current `si:` id.
    pub silicon_id: String,
    /// The Silicon that owns it.
    #[serde(default)]
    pub owner: Option<AccountRef>,
    /// Where deliveries go.
    pub endpoint_url: String,
    /// Revision.
    pub version: i64,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

/// Active subscriptions, possibly none.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookSubscriptions {
    /// The subscriptions.
    pub items: Vec<WebhookSubscription>,
}

/// Atomic batch result, in request order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusBatch {
    /// One result per requested reminder.
    pub items: Vec<StatusChange>,
}
/// One reminder's new status.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusChange {
    /// Reminder id.
    pub id: Uuid,
    /// `active` or `paused`.
    pub status: String,
    /// Next due instant, if any.
    pub next_run_at: Option<DateTime<Utc>>,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

/// A new test environment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateEnvironment {
    /// Unique among your active environments.
    pub name: String,
    /// Optional purpose.
    pub description: Option<String>,
}

/// Test environment metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestEnvironment {
    /// Public selector; never a credential.
    pub id: Uuid,
    /// The owner's uuid (`None` for an environment kept from before Silicon Accounts whose
    /// creator is not matched yet).
    #[serde(default)]
    pub owner_uuid: Option<String>,
    /// The owner.
    #[serde(default)]
    pub owner: Option<AccountRef>,
    /// Name.
    pub name: String,
    /// Optional purpose.
    pub description: Option<String>,
    /// Metadata revision.
    pub version: i64,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last activity; 15 days without any retires it.
    pub last_activity_at: DateTime<Utc>,
    /// When it was retired.
    pub deleted_at: Option<DateTime<Utc>>,
    /// When a retired environment is deleted for good.
    pub purge_after: Option<DateTime<Utc>>,
}

/// A created environment and its key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvironmentCreated {
    /// Metadata.
    pub environment: TestEnvironment,
    /// The 32-character key.
    pub key: Secret,
}
/// An environment's active key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvironmentKey {
    /// Environment id.
    pub environment_id: Uuid,
    /// The 32-character key.
    pub key: Secret,
}
/// Liveness or readiness.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    /// Service name.
    pub service: String,
    /// `ok` or a failure state.
    pub status: String,
    /// Service version.
    pub version: String,
}

/// A bug report. Never include credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BugReportRequest {
    /// Steps, expected result, actual result.
    pub message: String,
    /// Optional link to a Silicon Remind pull request that fixes it.
    pub pr: Option<String>,
}
/// A report's receipt and delivery state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BugReportResponse {
    /// Report id.
    pub id: Uuid,
    /// `queued`, `sending`, `sent`, `failed` or `simulated`.
    pub status: String,
    /// Why delivery failed.
    pub failure_reason: Option<String>,
}

/// Bounded operational fields; bodies, credentials and arbitrary context never appear.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// `rust_client`, `cli` or `web`.
    pub source: String,
    /// What happened, e.g. `command_completed`.
    pub event: String,
    /// `response`, `command` or `browser`.
    pub step: String,
    /// Whether it worked.
    pub success: bool,
    /// How long it took.
    pub duration_ms: u64,
    /// HTTP status, when relevant.
    pub status_code: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::{CreateScheduleRequest, Identity, PatchScheduleRequest, ScheduleResponse};
    use super::{TIMEZONE_REQUIRED_MESSAGE, VisibleSilicon};

    #[test]
    fn create_requires_an_explicit_timezone() {
        let base = serde_json::json!({"text": "Check the build", "kind": "recurring", "cron": "0 9 * * *"});
        for timezone in [
            None,
            Some(serde_json::Value::Null),
            Some("".into()),
            Some("  ".into()),
        ] {
            let mut input = base.clone();
            if let Some(timezone) = timezone {
                input["timezone"] = timezone;
            }
            let error = serde_json::from_value::<CreateScheduleRequest>(input)
                .expect_err("creation without a timezone must fail");
            assert!(error.to_string().contains(TIMEZONE_REQUIRED_MESSAGE));
        }
        for timezone in ["Asia/Kolkata", "UTC"] {
            let mut input = base.clone();
            input["timezone"] = timezone.into();
            let request = serde_json::from_value::<CreateScheduleRequest>(input)
                .expect("explicit timezone must be preserved");
            assert_eq!(request.timezone, timezone);
        }
    }

    #[test]
    fn editing_text_does_not_replace_timezone() {
        let request: PatchScheduleRequest =
            serde_json::from_value(serde_json::json!({"text": "Updated reminder"}))
                .expect("text-only edits remain valid");
        assert!(request.timezone.is_none());
        let encoded = serde_json::to_value(request).expect("patch serializes");
        assert!(encoded.get("timezone").is_none());
    }

    #[test]
    fn contract_two_responses_decode() {
        let schedule: ScheduleResponse = serde_json::from_value(serde_json::json!({
            "id": "01992000-0000-7000-8000-000000000001",
            "owner": {"uuid": "zQo", "id": "si:scout", "kind": "silicon"},
            "silicon_id": "si:scout", "text": "Standup", "timezone": "UTC", "kind": "recurring",
            "cron": "0 9 * * *", "status": "active", "section": "current", "next_run_at": null,
            "archived_at": null, "purge_after": null,
            "created_at": "2026-10-10T00:00:00Z", "updated_at": "2026-10-10T00:00:00Z"
        }))
        .expect("schedule decodes");
        assert_eq!(schedule.owner.expect("owner").uuid, "zQo");
        let identity: Identity = serde_json::from_value(serde_json::json!({
            "uuid": "aB3", "kind": "carbon", "id": "c:ada", "display_name": "Ada", "pfp_url": "",
            "custodian": null, "can_manage_reminders": false, "credential": "access_token",
            "issuing_app": null, "visible_silicons": 2
        }))
        .expect("identity decodes");
        assert_eq!(identity.visible_silicons, 2);
        let silicon: VisibleSilicon = serde_json::from_value(serde_json::json!({
            "uuid": "zQo", "silicon_id": "si:scout", "display_name": "", "pfp_url": "",
            "relation": "custodian", "reminder_count": 3
        }))
        .expect("visible silicon decodes");
        assert_eq!(silicon.relation, "custodian");
    }
}
