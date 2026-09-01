use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::domain::{ReminderReadScope, ScheduleSection};

/// An IAM principal type accepted by persistence and audit records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorType {
    /// A human account.
    Carbon,
    /// An AI-agent account.
    Silicon,
    /// An IAM application acting with its own identity.
    Application,
    /// An authenticated platform service.
    Service,
    /// An internal worker or maintenance process.
    System,
}

impl ActorType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Carbon => "carbon",
            Self::Silicon => "silicon",
            Self::Application => "application",
            Self::Service => "service",
            Self::System => "system",
        }
    }
}

/// Redacted actor and request attribution written to append-only audit records.
#[derive(Clone, Debug)]
pub struct AuditContext {
    /// Actor category.
    pub actor_type: ActorType,
    /// Stable IAM or internal worker identifier.
    pub actor_id: String,
    /// Request correlation identifier, when the operation has one.
    pub request_id: Option<String>,
}

/// Request identity used to scope an idempotency key.
#[derive(Clone, Debug)]
pub struct IdempotencyContext {
    /// Authenticated actor category. System actors are not accepted here.
    pub actor_type: ActorType,
    /// Stable authenticated actor identifier.
    pub actor_id: String,
    /// Client-provided idempotency key.
    pub key: String,
    /// SHA-256 digest of the canonical request.
    pub request_hash: [u8; 32],
    /// Retention deadline for the original response.
    pub expires_at: DateTime<Utc>,
}

/// Stored schedule row, including internal lifecycle and optimistic-lock data.
#[derive(Clone, Debug, FromRow)]
pub struct ScheduleRow {
    /// Schedule UUID.
    pub id: Uuid,
    /// Owning organization.
    pub org_id: String,
    /// Stable IAM UUID of the owning Silicon principal.
    pub owner_principal_id: Uuid,
    /// Public global identifier of the owning Silicon.
    pub silicon_id: String,
    /// Immutable-as-written reminder content for future occurrences.
    pub text: String,
    /// IANA time-zone identifier.
    pub timezone: String,
    /// One-time or recurring materialization behavior.
    pub schedule_kind: String,
    /// Five-field Linux cron expression.
    pub cron: String,
    /// Database-validated lifecycle state.
    pub status: String,
    /// Next instant eligible for occurrence materialization.
    pub next_run_at: Option<DateTime<Utc>>,
    /// Monotonic optimistic-lock version.
    pub version: i64,
    /// Time at which a one-time reminder's sole trigger entered archive.
    pub completed_at: Option<DateTime<Utc>>,
    /// Soft-deletion timestamp.
    pub deleted_at: Option<DateTime<Utc>>,
    /// Bounded-retention deadline.
    pub purge_after: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last mutation timestamp.
    pub updated_at: DateTime<Utc>,
}

impl ScheduleRow {
    /// Returns the first instant at which this reminder entered the archive.
    #[must_use]
    pub fn archived_at(&self) -> Option<DateTime<Utc>> {
        match (self.completed_at, self.deleted_at) {
            (Some(completed_at), Some(deleted_at)) => Some(completed_at.min(deleted_at)),
            (Some(completed_at), None) => Some(completed_at),
            (None, Some(deleted_at)) => Some(deleted_at),
            (None, None) => None,
        }
    }

    /// Returns the product-facing collection section for this row.
    #[must_use]
    pub fn section(&self) -> ScheduleSection {
        if self.archived_at().is_some() {
            ScheduleSection::Archived
        } else {
            ScheduleSection::Current
        }
    }
}

/// Values required to create an active schedule.
#[derive(Clone, Debug)]
pub struct CreateSchedule {
    /// Application-generated UUID.
    pub id: Uuid,
    /// Owning organization.
    pub org_id: String,
    /// Stable authenticated IAM principal UUID.
    pub owner_principal_id: Uuid,
    /// Public global identifier resolved from the active IAM binding.
    pub silicon_id: String,
    /// Reminder content.
    pub text: String,
    /// Validated IANA time-zone identifier.
    pub timezone: String,
    /// One-time or recurring materialization behavior.
    pub schedule_kind: String,
    /// Five-field Linux cron expression.
    pub cron: String,
    /// Calculated first occurrence.
    pub next_run_at: DateTime<Utc>,
}

/// Publicly mutable schedule status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutableScheduleStatus {
    /// Eligible for occurrence materialization.
    Active,
    /// Future occurrence materialization is suspended.
    Paused,
}

impl MutableScheduleStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
        }
    }
}

/// Fully merged, validated replacement persisted by an idempotent PATCH.
#[derive(Clone, Debug)]
pub struct ScheduleReplacement {
    /// Organization scope.
    pub org_id: String,
    /// Stable owner principal required for mutation authority.
    pub owner_principal_id: Uuid,
    /// Public global Silicon identifier retained on the schedule.
    pub silicon_id: String,
    /// Target schedule UUID.
    pub schedule_id: Uuid,
    /// Version observed before domain-level merge and validation.
    pub expected_version: i64,
    /// Resulting reminder content.
    pub text: String,
    /// Resulting IANA time-zone identifier.
    pub timezone: String,
    /// Resulting one-time or recurring behavior.
    pub schedule_kind: String,
    /// Resulting five-field Linux cron expression.
    pub cron: String,
    /// Resulting client-controlled status.
    pub status: MutableScheduleStatus,
    /// Recalculated next occurrence, retained while paused if desired.
    pub next_run_at: Option<DateTime<Utc>>,
}

/// One schedule revision and desired next occurrence in an atomic status batch.
#[derive(Clone, Debug)]
pub struct ScheduleStatusChange {
    /// Target schedule UUID.
    pub id: Uuid,
    /// Version observed during application-level validation.
    pub expected_version: i64,
    /// Desired next occurrence. This is `None` when pausing and is calculated
    /// by the application when resuming a schedule.
    pub next_run_at: Option<DateTime<Utc>>,
}

/// Validated desired-status changes persisted as one atomic operation.
#[derive(Clone, Debug)]
pub struct BulkScheduleStatusReplacement {
    /// Organization scope shared by every target schedule.
    pub org_id: String,
    /// Stable owner principal required for mutation authority.
    pub owner_principal_id: Uuid,
    /// Desired state shared by every target schedule.
    pub status: MutableScheduleStatus,
    /// Target revisions in API request order.
    pub schedules: Vec<ScheduleStatusChange>,
}

/// Opaque keyset position for schedule listings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduleCursor {
    /// Last row's creation instant.
    pub created_at: DateTime<Utc>,
    /// Last row's UUID tie-breaker.
    pub id: Uuid,
}

/// Tenant-scoped schedule listing filters.
#[derive(Clone, Debug)]
pub struct ListSchedules {
    /// Organization scope.
    pub org_id: String,
    /// IAM-authorized owner projection, enforced before pagination.
    pub read_scope: ReminderReadScope,
    /// Optional owner filter.
    pub silicon_id: Option<String>,
    /// Product-facing current or archived partition.
    pub section: ScheduleSection,
    /// Optional lifecycle filter.
    pub status: Option<String>,
    /// Decoded opaque cursor.
    pub cursor: Option<ScheduleCursor>,
    /// Requested page size, from 1 through 100.
    pub limit: u32,
}

/// Stored occurrence and Hook-delivery state.
#[derive(Clone, Debug, FromRow)]
pub struct ExecutionRow {
    /// Stable occurrence and Hook idempotency UUID.
    pub id: Uuid,
    /// Parent schedule UUID.
    pub schedule_id: Uuid,
    /// Organization snapshot.
    pub org_id: String,
    /// Owner Silicon snapshot.
    pub silicon_id: String,
    /// Schedule revision after this occurrence was materialized.
    pub schedule_version: i64,
    /// Schedule kind when this occurrence was materialized.
    pub schedule_kind: String,
    /// Intended schedule instant.
    pub scheduled_for: DateTime<Utc>,
    /// Immutable reminder-content snapshot.
    pub text: String,
    /// Immutable timezone snapshot.
    pub timezone: String,
    /// Database-validated delivery status.
    pub status: String,
    /// Number of acquired delivery attempts.
    pub attempt_count: i32,
    /// Time at which another attempt becomes eligible.
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// Most recent attempt start.
    pub attempted_at: Option<DateTime<Utc>>,
    /// Hook acceptance time.
    pub delivered_at: Option<DateTime<Utc>>,
    /// Stable Hook event UUID returned after acceptance.
    pub hook_event_id: Option<Uuid>,
    /// Bounded, sanitized delivery failure detail.
    pub failure_reason: Option<String>,
    /// Current worker lease holder.
    pub lease_owner: Option<String>,
    /// Current worker lease deadline.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// Materialization timestamp.
    pub created_at: DateTime<Utc>,
    /// Last delivery-state mutation timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Opaque keyset position for execution history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionCursor {
    /// Last row's scheduled instant.
    pub scheduled_for: DateTime<Utc>,
    /// Last row's UUID tie-breaker.
    pub id: Uuid,
}

/// Result of atomically materializing one locked due schedule.
#[derive(Clone, Debug)]
pub struct DueMaterialization {
    /// Durable occurrence row.
    pub execution: ExecutionRow,
    /// Whether this transaction inserted the occurrence rather than observing
    /// an already-materialized unique occurrence.
    pub inserted: bool,
}

/// Encrypted per-Silicon Hook routing and signing material.
#[derive(Clone, Debug, FromRow)]
pub struct HookDestinationRow {
    /// Registry row UUID.
    pub id: Uuid,
    /// Organization scope.
    pub org_id: String,
    /// Stable IAM UUID of the destination Silicon principal.
    pub owner_principal_id: Uuid,
    /// Public global identifier used by Hook routing.
    pub silicon_id: String,
    /// AES-GCM ciphertext containing the endpoint URL.
    pub endpoint_url_ciphertext: Vec<u8>,
    /// Endpoint ciphertext nonce.
    pub endpoint_url_nonce: Vec<u8>,
    /// AES-GCM ciphertext containing the signing secret.
    pub signing_secret_ciphertext: Vec<u8>,
    /// Secret ciphertext nonce.
    pub signing_secret_nonce: Vec<u8>,
    /// Versioned encryption-key identifier.
    pub encryption_key_version: i16,
    /// Monotonic registry version.
    pub version: i64,
    /// Time this destination was disabled.
    pub disabled_at: Option<DateTime<Utc>>,
    /// Deadline after which disabled encrypted material is purged.
    pub purge_after: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last replacement timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Already-encrypted destination values accepted by the registry repository.
#[derive(Clone, Debug)]
pub struct NewHookDestination {
    /// Application-generated registry UUID used when no row exists.
    pub id: Uuid,
    /// Organization scope.
    pub org_id: String,
    /// Stable IAM UUID of the destination Silicon principal.
    pub owner_principal_id: Uuid,
    /// Public global identifier used by Hook routing.
    pub silicon_id: String,
    /// Encrypted URL bytes.
    pub endpoint_url_ciphertext: Vec<u8>,
    /// Unique 96-bit URL nonce.
    pub endpoint_url_nonce: [u8; 12],
    /// Encrypted signing-secret bytes.
    pub signing_secret_ciphertext: Vec<u8>,
    /// Unique 96-bit signing-secret nonce.
    pub signing_secret_nonce: [u8; 12],
    /// Positive key version.
    pub encryption_key_version: i16,
}

/// Optimistically applied re-encryption of one active Hook destination.
#[derive(Clone, Debug)]
pub struct HookDestinationRewrap {
    /// Registry row UUID.
    pub id: Uuid,
    /// Version observed with the old ciphertext.
    pub expected_version: i64,
    /// Re-encrypted URL bytes.
    pub endpoint_url_ciphertext: Vec<u8>,
    /// Fresh URL nonce.
    pub endpoint_url_nonce: [u8; 12],
    /// Re-encrypted signing-secret bytes.
    pub signing_secret_ciphertext: Vec<u8>,
    /// Fresh signing-secret nonce.
    pub signing_secret_nonce: [u8; 12],
    /// New positive key version.
    pub encryption_key_version: i16,
}

/// Active IAM principal-to-public-Silicon binding used for ownership and Hook
/// routing. Revoked bindings are intentionally not returned by public lookup.
#[derive(Clone, Debug, FromRow)]
pub struct SiliconIdentityRow {
    /// Selected organization.
    pub org_id: String,
    /// Stable IAM principal UUID.
    pub principal_id: Uuid,
    /// Immutable global Silicon identifier.
    pub silicon_id: String,
}

/// Deduplicated internal IAM or provisioning event receipt.
#[derive(Clone, Debug, FromRow)]
pub struct InternalEventReceiptRow {
    /// Internal receipt UUID.
    pub id: Uuid,
    /// Sending service.
    pub source: String,
    /// Sender's stable event ID.
    pub event_id: String,
    /// Event type.
    pub event_type: String,
    /// Optional organization scope.
    pub org_id: Option<String>,
    /// Optional event subject.
    pub subject_id: Option<String>,
    /// Original structured payload.
    pub payload: Value,
    /// Canonical SHA-256 payload digest.
    pub payload_hash: Vec<u8>,
    /// Receipt-processing state.
    pub status: String,
    /// Processing claim count.
    pub attempt_count: i32,
    /// Retry eligibility instant.
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// Successful processing instant.
    pub processed_at: Option<DateTime<Utc>>,
    /// Bounded processing failure detail.
    pub failure_reason: Option<String>,
    /// Current processing lease holder.
    pub lease_owner: Option<String>,
    /// Current processing lease deadline.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// Receipt timestamp.
    pub received_at: DateTime<Utc>,
    /// Last processing-state mutation timestamp.
    pub updated_at: DateTime<Utc>,
}

/// New internal event to record idempotently.
#[derive(Clone, Debug)]
pub struct NewInternalEvent {
    /// Application-generated receipt UUID.
    pub id: Uuid,
    /// Sending service.
    pub source: String,
    /// Sender's stable event ID.
    pub event_id: String,
    /// Event type.
    pub event_type: String,
    /// Optional organization scope.
    pub org_id: Option<String>,
    /// Optional subject.
    pub subject_id: Option<String>,
    /// Structured payload.
    pub payload: Value,
    /// Canonical SHA-256 payload digest.
    pub payload_hash: [u8; 32],
    /// Initial processing eligibility instant.
    pub received_at: DateTime<Utc>,
}

/// Result of atomically applying one IAM membership-removal event.
#[derive(Clone, Debug)]
pub struct IamLifecycleOutcome {
    /// Durable, processed inbox receipt. On replay this is the original row.
    pub receipt: InternalEventReceiptRow,
    /// Whether an identical committed event was observed without new changes.
    pub replayed: bool,
    /// Number of active or paused schedules soft-deleted by this transaction.
    pub schedules_deleted: u64,
    /// Number of unaccepted executions terminally failed by this transaction.
    pub executions_failed: u64,
}

/// Result of one bounded cleanup pass over IAM-revoked resources.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RevokedResourceCleanup {
    /// Newly soft-deleted schedules.
    pub schedules_deleted: u64,
    /// Unaccepted executions moved to terminal failure.
    pub executions_failed: u64,
    /// Hook destinations placed into disabled retention.
    pub destinations_disabled: u64,
}

/// Result of one atomic expired-schedule purge and ledger-maintenance pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SchedulePurgeResult {
    /// Schedules permanently removed with their cascading execution history.
    pub purged: u64,
    /// Deleted-reminder records durably written before schedule removal.
    pub logged: u64,
    /// Oldest ledger records removed to preserve the global rolling bound.
    pub trimmed: u64,
}

/// Exact HTTP response retained for an unexpired idempotency key.
#[derive(Clone, Debug)]
pub struct StoredIdempotentResponse {
    /// Original HTTP response status.
    pub status_code: u16,
    /// Exact original JSON response body.
    pub response_body: Value,
}

/// One page of keyset-ordered records.
#[derive(Clone, Debug)]
pub struct Page<T> {
    /// At most the requested number of records.
    pub items: Vec<T>,
    /// Whether another keyset page existed at query time.
    pub has_more: bool,
}

/// Outcome of an idempotent create or patch operation.
#[derive(Clone, Debug)]
pub enum IdempotentMutation<T> {
    /// This request performed the mutation.
    Applied {
        /// Newly persisted resource state.
        value: T,
        /// Exact JSON response retained for later replay.
        response_body: Value,
    },
    /// A prior identical request already committed.
    Replayed {
        /// Original HTTP response status.
        status_code: u16,
        /// Exact original response body.
        response_body: Value,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ScheduleResponse {
    pub id: Uuid,
    pub org_id: String,
    pub silicon_id: String,
    pub text: String,
    pub timezone: String,
    pub kind: String,
    pub cron: String,
    pub status: String,
    pub section: ScheduleSection,
    pub next_run_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub purge_after: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&ScheduleRow> for ScheduleResponse {
    fn from(row: &ScheduleRow) -> Self {
        let archived_at = row.archived_at();
        Self {
            id: row.id,
            org_id: row.org_id.clone(),
            silicon_id: row.silicon_id.clone(),
            text: row.text.clone(),
            timezone: row.timezone.clone(),
            kind: row.schedule_kind.clone(),
            cron: row.cron.clone(),
            status: row.status.clone(),
            section: row.section(),
            next_run_at: row.next_run_at,
            archived_at,
            purge_after: row.purge_after,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Exact compact response stored for an atomic schedule status mutation.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ScheduleStatusBatchResponse {
    /// Schedule results in API request order.
    pub items: Vec<ScheduleStatusResponse>,
}

/// Public schedule fields changed or preserved by a desired-status request.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ScheduleStatusResponse {
    /// Schedule UUID.
    pub id: Uuid,
    /// Resulting lifecycle status.
    pub status: String,
    /// Resulting next occurrence, or `None` while paused.
    pub next_run_at: Option<DateTime<Utc>>,
    /// Last mutation timestamp, preserved for a no-op.
    pub updated_at: DateTime<Utc>,
}

impl From<&ScheduleRow> for ScheduleStatusResponse {
    fn from(row: &ScheduleRow) -> Self {
        Self {
            id: row.id,
            status: row.status.clone(),
            next_run_at: row.next_run_at,
            updated_at: row.updated_at,
        }
    }
}
