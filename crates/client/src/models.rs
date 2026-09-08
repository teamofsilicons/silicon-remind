//! Public Remind wire types. No backend or database dependency.
use crate::Secret;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Reminder occurrence mode.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleKind {
    OneTime,
    Recurring,
}
/// Reminder lifecycle state.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleStatus {
    Active,
    Paused,
    Completed,
}
/// Current or archived reminder view.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleSection {
    #[default]
    Current,
    Archived,
}
/// Forward-compatible delivery status.
pub type ExecutionStatus = String;
fn default_timezone() -> String {
    "UTC".to_owned()
}

/// Public CreateScheduleRequest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateScheduleRequest {
    /// Reminder content.
    pub text: String,
    /// One-time or recurring materialization behavior.
    pub kind: ScheduleKind,
    /// IANA timezone identifier; omitted values use UTC.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Five-field Linux cron expression.
    pub cron: String,
}

/// Public ScheduleResponse.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleResponse {
    /// Schedule UUID.
    pub id: Uuid,
    /// Organization scope.
    pub org_id: String,
    /// Owner Silicon.
    pub silicon_id: String,
    /// Reminder content.
    pub text: String,
    /// IANA timezone identifier.
    pub timezone: String,
    /// One-time or recurring materialization behavior.
    pub kind: String,
    /// Five-field Linux cron expression.
    pub cron: String,
    /// Public lifecycle status.
    pub status: String,
    /// Product-facing current or archived section.
    pub section: ScheduleSection,
    /// Next due instant or null.
    pub next_run_at: Option<DateTime<Utc>>,
    /// Time at which the reminder entered the archive, if applicable.
    pub archived_at: Option<DateTime<Utc>>,
    /// Permanent-deletion deadline for archived reminders.
    pub purge_after: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last mutation timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Public ExecutionResponse.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionResponse {
    /// Stable logical occurrence UUID.
    pub id: Uuid,
    /// Parent schedule UUID.
    pub schedule_id: Uuid,
    /// Intended occurrence instant.
    pub scheduled_for: DateTime<Utc>,
    /// Most recent attempt time.
    pub attempted_at: Option<DateTime<Utc>>,
    /// webhook ingress receipt time; not downstream acknowledgment.
    pub delivered_at: Option<DateTime<Utc>>,
    /// Delivery lifecycle status.
    pub status: ExecutionStatus,
    /// Historical name for the webhook ingress receipt UUID.
    pub hook_event_id: Option<Uuid>,
    /// Sanitized terminal/transient failure reason.
    pub failure_reason: Option<String>,
}

/// Public BulkScheduleStatusRequest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BulkScheduleStatusRequest {
    /// Reminder identifiers in the order results must be returned.
    pub schedule_ids: Vec<Uuid>,
    /// Desired status. Application policy rejects the terminal status.
    pub status: ScheduleStatus,
}

/// Partial reminder replacement; omitted fields retain their current values.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PatchScheduleRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ScheduleKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ScheduleStatus>,
}
/// Opaque pagination; pass next_cursor unchanged into the next request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
/// Filters used before server-side pagination.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ListSchedules {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silicon_id: Option<String>,
    pub section: ScheduleSection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ScheduleStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}
/// Page-size and opaque-cursor input.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Paging {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}
/// IAM application session, with redacted Debug output for credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub expires_in: i64,
    pub token_type: String,
    pub scope: String,
    pub actor: serde_json::Value,
    pub org_id: Option<String>,
}
/// Current organization authority obtained from IAM on this request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub principal_id: Uuid,
    pub actor_type: String,
    pub public_id: Option<String>,
    pub org_id: String,
    pub membership_id: Uuid,
    pub org_role: Option<String>,
    pub authorization_epoch: u64,
    pub can_manage_reminders: bool,
}
/// Outbound webhook destination, configured by its owner.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Destination {
    pub endpoint_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_secret: Option<Secret>,
}
/// Public destination receipt; signing credentials are never returned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationReceipt {
    pub id: Uuid,
    pub silicon_id: String,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}
/// Readable destination with its write-only credential omitted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationInfo {
    pub id: Uuid,
    pub silicon_id: String,
    pub endpoint_url: String,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}
/// Active webhook subscriptions for the authenticated Silicon.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebhookSubscriptions {
    pub items: Vec<DestinationInfo>,
}
/// One Silicon registered with Remind.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Silicon {
    pub principal_id: Uuid,
    pub silicon_id: String,
    pub reminder_count: i64,
}
/// Atomic batch result in input UUID order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusBatch {
    pub items: Vec<StatusChange>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusChange {
    pub id: Uuid,
    pub status: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}
/// New sandbox paired with an existing IAM test Application.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateEnvironment {
    pub name: String,
    pub description: Option<String>,
    pub iam_test_key: Secret,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iam_app_secret: Option<Secret>,
}

/// Organization-owned sandbox metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestEnvironment {
    /// Public environment selector, never an authentication credential.
    pub id: Uuid,
    /// Production organization that owns this test environment.
    pub org_id: String,
    /// Production principal that created it.
    pub creator_id: Uuid,
    /// Human-readable name.
    pub name: String,
    /// Optional purpose.
    pub description: Option<String>,
    /// IAM sandbox to which all authentication is bound.
    pub iam_environment_id: Uuid,
    /// Monotonic metadata revision.
    pub version: i64,
    /// Creation instant.
    pub created_at: DateTime<Utc>,
    /// Most recent successful user activity; worker ticks do not keep it alive.
    pub last_activity_at: DateTime<Utc>,
    /// Retirement instant, if inactive.
    pub deleted_at: Option<DateTime<Utc>>,
    /// Permanent deletion deadline.
    pub purge_after: Option<DateTime<Utc>>,
}

/// Newly created environment and its retrievable root key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvironmentCreated {
    pub environment: TestEnvironment,
    pub key: Secret,
}
/// Active root key response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvironmentKey {
    pub environment_id: Uuid,
    pub key: Secret,
}
/// Liveness/readiness response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    pub service: String,
    pub status: String,
    pub version: String,
}
