use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::domain::{AccountRef, ScheduleSection};

/// An actor category accepted by persistence and audit records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorType {
    /// A Carbon account.
    Carbon,
    /// A Silicon account.
    Silicon,
    /// An app acting with its own identity (Silicon Accounts webhooks, proofs).
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
    /// Silicon Accounts uuid, app id, or internal worker identifier.
    pub actor_id: String,
    /// Request correlation identifier, when the operation has one.
    pub request_id: Option<String>,
}

/// Request identity used to scope an idempotency key.
#[derive(Clone, Debug)]
pub struct IdempotencyContext {
    /// Authenticated actor category. System actors are not accepted here.
    pub actor_type: ActorType,
    /// The authenticated account's Silicon Accounts uuid.
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
    /// Organization of a row written before the move to Silicon Accounts; `None` since.
    pub org_id: Option<String>,
    /// Storage key of the owning Silicon's account.
    pub owner_principal_id: Uuid,
    /// The owning Silicon's public id when the schedule was created.
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
    /// The creating Silicon's storage key.
    pub owner_key: Uuid,
    /// The creating Silicon's current public id, kept as the creation snapshot.
    pub silicon_id: String,
    /// The creating Silicon, as responses show the owner.
    pub owner: AccountRef,
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
    /// Every storage key of the owner Silicon; the schedule must be stored under one.
    pub owner_keys: Vec<Uuid>,
    /// The owner Silicon, as responses show it.
    pub owner: AccountRef,
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
    /// Every storage key of the owner Silicon; each schedule must be stored under one.
    pub owner_keys: Vec<Uuid>,
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

/// Owner-scoped schedule listing filters.
#[derive(Clone, Debug)]
pub struct ListSchedules {
    /// Storage keys the caller may read, enforced before pagination; `None` reads every row.
    pub read_keys: Option<Vec<Uuid>>,
    /// Optional owner filter: the storage keys of one owner.
    pub owner_keys: Option<Vec<Uuid>>,
    /// Optional owner filter by the creation-time public id, for rows no account owns yet.
    pub silicon_snapshot: Option<String>,
    /// Product-facing current or archived partition.
    pub section: ScheduleSection,
    /// Optional lifecycle filter.
    pub status: Option<String>,
    /// Decoded opaque cursor.
    pub cursor: Option<ScheduleCursor>,
    /// Requested page size, from 1 through 100.
    pub limit: u32,
}

/// Stored occurrence and webhook-delivery state.
#[derive(Clone, Debug, FromRow)]
pub struct ExecutionRow {
    /// Stable occurrence and webhook idempotency UUID.
    pub id: Uuid,
    /// Parent schedule UUID.
    pub schedule_id: Uuid,
    /// Organization of a row written before the move to Silicon Accounts; `None` since.
    pub org_id: Option<String>,
    /// Owner Silicon's public id when the occurrence was materialized.
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
    /// webhook acceptance time.
    pub delivered_at: Option<DateTime<Utc>>,
    /// Stable webhook event UUID returned after acceptance.
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

/// Encrypted per-configured webhook receiver routing and signing material.
#[derive(Clone, Debug, FromRow)]
pub struct HookDestinationRow {
    /// Registry row UUID.
    pub id: Uuid,
    /// Organization of a row written before the move to Silicon Accounts; `None` since.
    pub org_id: Option<String>,
    /// Storage key of the owning Silicon's account.
    pub owner_principal_id: Uuid,
    /// The owning Silicon's public id when the subscription was created.
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
    /// Associated-data scheme of the ciphertexts: 1 binds them to the legacy
    /// organization and handle, 2 to this row's id and owner storage key.
    pub aad_version: i16,
}

/// Already-encrypted destination values accepted by the registry repository.
/// New destinations always use associated-data version 2.
#[derive(Clone, Debug)]
pub struct NewHookDestination {
    /// Application-generated registry UUID (also part of the associated data).
    pub id: Uuid,
    /// The owning Silicon's storage key.
    pub owner_key: Uuid,
    /// The owning Silicon's current public id, kept as the creation snapshot.
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

/// Optimistically applied re-encryption of one active webhook destination.
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

/// Deduplicated internal event receipt (Silicon Accounts webhooks; legacy IAM events).
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

/// Result of one bounded cleanup pass over resources IAM revoked before the move to
/// Silicon Accounts (rows that still carry an organization).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RevokedResourceCleanup {
    /// Newly soft-deleted schedules.
    pub schedules_deleted: u64,
    /// Unaccepted executions moved to terminal failure.
    pub executions_failed: u64,
    /// webhook destinations placed into disabled retention.
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

/// Public schedule representation (`openapi.yaml` `Schedule`), also stored
/// verbatim for idempotent replay.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleResponse {
    /// Schedule UUID.
    pub id: Uuid,
    /// The owner Silicon's account; `null` only for a row written before the
    /// move to Silicon Accounts that no account owns yet.
    pub owner: Option<AccountRef>,
    /// The owner Silicon's current public id (the creation-time id when the
    /// owner is unknown).
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

impl ScheduleResponse {
    /// Builds the response for a row and its owner. The owner's current id wins
    /// over the creation snapshot when Remind knows it.
    #[must_use]
    pub fn new(row: &ScheduleRow, owner: Option<&AccountRef>) -> Self {
        let silicon_id = owner
            .map(|owner| owner.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| row.silicon_id.clone());
        Self {
            id: row.id,
            owner: owner.cloned(),
            silicon_id,
            text: row.text.clone(),
            timezone: row.timezone.clone(),
            kind: row.schedule_kind.clone(),
            cron: row.cron.clone(),
            status: row.status.clone(),
            section: row.section(),
            next_run_at: row.next_run_at,
            archived_at: row.archived_at(),
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
