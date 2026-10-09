//! Schedule and execution-history use cases for Silicon Accounts actors.
//!
//! Only a Silicon sets and changes reminders, with its own access token. Every
//! account in the Silicon's circle and every account it granted view reads
//! them. In a test environment the key holder reads everything there.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::{
    application::ports::Clock,
    domain::{
        AccountRef, Actor, ActorKind, CreateScheduleCommand, Credential, CronExpression,
        CursorKind, MAX_SCHEDULE_STATUS_BATCH_SIZE, PageCursor, PatchScheduleCommand, ReadScope,
        Schedule, ScheduleKind, ScheduleSection, ScheduleStatus, ScheduleTiming,
        ScheduleValidationError, is_valid_global_silicon_id,
    },
    error::AppError,
    infrastructure::{
        identity::IdentityStore,
        postgres::{
            ActorType, AuditContext, BulkScheduleStatusReplacement, CreateSchedule,
            ExecutionCursor, ExecutionRow, IdempotencyContext, IdempotentMutation, ListSchedules,
            MutableScheduleStatus, Page, PostgresRepository, ScheduleCursor, ScheduleReplacement,
            ScheduleRow, ScheduleStatusChange,
        },
    },
    request_context,
};

/// Original idempotent HTTP response returned by a mutation.
#[derive(Clone, Debug)]
pub struct MutationResponse {
    /// HTTP status from the first committed request.
    pub status_code: u16,
    /// Exact response JSON retained for replay.
    pub body: Value,
}

/// The accounts behind storage keys, for building responses.
pub type Owners = HashMap<Uuid, AccountRef>;

/// Application service enforcing domain and authorization policy.
#[derive(Clone, Debug)]
pub struct ScheduleService {
    repository: PostgresRepository,
    identity: IdentityStore,
    clock: Arc<dyn Clock>,
    idempotency_retention: Duration,
}

impl ScheduleService {
    /// Creates the service. `repository` holds the selected environment's data;
    /// `identity` is always the production identity store.
    #[must_use]
    pub fn new(
        repository: PostgresRepository,
        identity: IdentityStore,
        clock: Arc<dyn Clock>,
        idempotency_retention: Duration,
    ) -> Self {
        Self {
            repository,
            identity,
            clock,
            idempotency_retention,
        }
    }

    /// Returns the underlying repository for readiness and worker composition.
    #[must_use]
    pub const fn repository(&self) -> &PostgresRepository {
        &self.repository
    }

    /// Creates a reminder owned by the calling Silicon, idempotently.
    ///
    /// # Errors
    ///
    /// Returns authorization, validation, idempotency, or persistence errors.
    pub async fn create(
        &self,
        actor: &Actor,
        command: CreateScheduleCommand,
        idempotency_key: String,
        request_hash: [u8; 32],
    ) -> Result<MutationResponse, AppError> {
        require_writing_silicon(actor)?;
        validate_idempotency_key(&idempotency_key)?;
        let now = self.clock.now();
        let idempotency = self.idempotency(actor, idempotency_key, request_hash, now)?;
        if let Some(stored) = self
            .repository
            .find_idempotent_response("schedule.create", None, &idempotency)
            .await?
        {
            return Ok(MutationResponse {
                status_code: stored.status_code,
                body: stored.response_body,
            });
        }
        let validated = command.validate(now).map_err(|_| AppError::Validation)?;
        if !is_valid_global_silicon_id(&actor.public_id) {
            return Err(AppError::described(
                http::StatusCode::CONFLICT,
                "silicon_id_unknown",
                "Remind does not know this Silicon's si: id yet. Retry in a minute.",
            ));
        }
        let schedule = CreateSchedule {
            id: Uuid::now_v7(),
            owner_key: actor.storage_key,
            silicon_id: actor.public_id.clone(),
            owner: actor.account(),
            text: validated.text().to_owned(),
            timezone: validated.timezone().name().to_owned(),
            schedule_kind: validated.timing().kind().as_str().to_owned(),
            cron: validated.timing().cron().to_string(),
            next_run_at: validated.next_run_at(),
        };
        let mutation = self
            .repository
            .create_schedule_idempotent(&schedule, &idempotency, &audit_context(actor))
            .await?;
        Ok(mutation_response(mutation, 201))
    }

    /// Lists the reminders the caller may read, optionally for one Silicon.
    ///
    /// # Errors
    ///
    /// Returns validation or persistence errors.
    pub async fn list(
        &self,
        actor: &Actor,
        silicon_id: Option<String>,
        section: ScheduleSection,
        status: Option<ScheduleStatus>,
        encoded_cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Page<ScheduleRow>, Owners), AppError> {
        if silicon_id
            .as_deref()
            .is_some_and(|value| !is_valid_global_silicon_id(value))
        {
            return Err(AppError::invalid(
                "silicon_id_invalid",
                "silicon_id must be a Silicon id such as si:scout.",
            ));
        }
        let cursor = encoded_cursor
            .map(|cursor| PageCursor::decode(cursor, CursorKind::Schedules))
            .transpose()
            .map_err(|_| AppError::Validation)?
            .map(|cursor| ScheduleCursor {
                created_at: cursor.timestamp(),
                id: cursor.id(),
            });
        let (owner_keys, silicon_snapshot) = match silicon_id.as_deref() {
            Some(id) => self.owner_filter(actor, id).await?,
            None => (None, None),
        };
        let filters = ListSchedules {
            read_keys: actor.read_keys(),
            owner_keys,
            silicon_snapshot,
            section,
            status: status.map(schedule_status_name).map(str::to_owned),
            cursor,
            limit: validate_page_limit(limit)?,
        };
        let page = self.repository.list_schedules(&filters).await?;
        let owners = self.owners_for(actor, &page.items).await?;
        Ok((page, owners))
    }

    /// Gets one readable reminder and its owner.
    ///
    /// # Errors
    ///
    /// Returns not found for reminders the caller cannot read.
    pub async fn get(
        &self,
        actor: &Actor,
        schedule_id: Uuid,
    ) -> Result<(ScheduleRow, Option<AccountRef>), AppError> {
        let row = self
            .repository
            .get_schedule(actor.read_keys().as_deref(), schedule_id)
            .await?
            .ok_or(AppError::NotFound)?;
        let owner = self
            .owners_for(actor, std::slice::from_ref(&row))
            .await?
            .remove(&row.owner_principal_id);
        Ok((row, owner))
    }

    /// Applies an owner-only merge patch atomically and idempotently.
    ///
    /// # Errors
    ///
    /// Returns authorization, validation, conflict, or persistence errors.
    pub async fn patch(
        &self,
        actor: &Actor,
        schedule_id: Uuid,
        command: PatchScheduleCommand,
        idempotency_key: String,
        request_hash: [u8; 32],
    ) -> Result<MutationResponse, AppError> {
        require_writing_silicon(actor)?;
        validate_idempotency_key(&idempotency_key)?;
        let now = self.clock.now();
        let idempotency = self.idempotency(actor, idempotency_key, request_hash, now)?;
        if let Some(stored) = self
            .repository
            .find_idempotent_response("schedule.patch", Some(schedule_id), &idempotency)
            .await?
        {
            return Ok(MutationResponse {
                status_code: stored.status_code,
                body: stored.response_body,
            });
        }
        let (current_row, _) = self.get(actor, schedule_id).await?;
        require_owner(actor, &current_row)?;
        let mut schedule = schedule_from_row(&current_row)?;
        let patch = command
            .validate(&schedule, now)
            .map_err(|error| map_patch_validation(&error))?;
        schedule
            .apply_patch(patch)
            .map_err(|error| map_patch_validation(&error))?;
        let status = match schedule.status {
            ScheduleStatus::Active => MutableScheduleStatus::Active,
            ScheduleStatus::Paused => MutableScheduleStatus::Paused,
            ScheduleStatus::Completed => return Err(AppError::conflict("schedule_completed")),
        };
        let replacement = ScheduleReplacement {
            owner_keys: actor.own_keys.clone(),
            owner: actor.account(),
            schedule_id,
            expected_version: current_row.version,
            text: schedule.text.clone(),
            timezone: schedule.timezone.name().to_owned(),
            schedule_kind: schedule.kind().as_str().to_owned(),
            cron: schedule.cron().to_string(),
            status,
            next_run_at: schedule.next_run_at,
        };
        let mutation = self
            .repository
            .replace_schedule_idempotent(&replacement, &idempotency, &audit_context(actor))
            .await?;
        Ok(mutation_response(mutation, 200))
    }

    /// Sets the desired status of up to 100 of the caller's reminders atomically.
    ///
    /// # Errors
    ///
    /// Returns authorization, validation, conflict, idempotency, or persistence
    /// errors. A failure leaves every reminder unchanged.
    pub async fn update_statuses(
        &self,
        actor: &Actor,
        schedule_ids: Vec<Uuid>,
        status: ScheduleStatus,
        idempotency_key: String,
        request_hash: [u8; 32],
    ) -> Result<MutationResponse, AppError> {
        require_writing_silicon(actor)?;
        validate_schedule_status_batch(&schedule_ids, status)?;
        validate_idempotency_key(&idempotency_key)?;
        let now = self.clock.now();
        let idempotency = self.idempotency(actor, idempotency_key, request_hash, now)?;
        if let Some(stored) = self
            .repository
            .find_idempotent_response("schedule.bulk_status", None, &idempotency)
            .await?
        {
            return Ok(MutationResponse {
                status_code: stored.status_code,
                body: stored.response_body,
            });
        }
        let rows = self
            .repository
            .get_schedules_for_status_change(actor.read_keys().as_deref(), &schedule_ids)
            .await?;
        if rows.len() != schedule_ids.len() {
            return Err(AppError::NotFound);
        }
        let rows_by_id: HashMap<Uuid, ScheduleRow> =
            rows.into_iter().map(|row| (row.id, row)).collect();
        let ordered_rows = schedule_ids
            .iter()
            .map(|schedule_id| rows_by_id.get(schedule_id).ok_or(AppError::NotFound))
            .collect::<Result<Vec<_>, _>>()?;
        for row in &ordered_rows {
            require_owner(actor, row)?;
        }
        if ordered_rows
            .iter()
            .any(|row| row.archived_at().is_some() || row.status == "completed")
        {
            return Err(AppError::conflict("invalid_schedule_state"));
        }
        let mutable_status = mutable_schedule_status(status)?;
        let schedules = ordered_rows
            .into_iter()
            .map(|row| {
                Ok(ScheduleStatusChange {
                    id: row.id,
                    expected_version: row.version,
                    next_run_at: desired_next_run_at(row, status, now)?,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        let replacement = BulkScheduleStatusReplacement {
            owner_keys: actor.own_keys.clone(),
            status: mutable_status,
            schedules,
        };
        let mutation = self
            .repository
            .replace_schedule_statuses_idempotent(&replacement, &idempotency, &audit_context(actor))
            .await?;
        Ok(mutation_response(mutation, 200))
    }

    /// Archives one of the caller's reminders and stops its future deliveries.
    ///
    /// # Errors
    ///
    /// Returns not found for reminders the caller cannot read, and forbidden for
    /// a readable reminder another Silicon set.
    pub async fn delete(&self, actor: &Actor, schedule_id: Uuid) -> Result<(), AppError> {
        require_writing_silicon(actor)?;
        let (row, _) = self.get(actor, schedule_id).await?;
        require_owner(actor, &row)?;
        self.repository
            .archive_schedule(
                &actor.own_keys,
                schedule_id,
                self.clock.now(),
                &audit_context(actor),
            )
            .await?;
        Ok(())
    }

    /// Lists a readable reminder's execution history.
    ///
    /// # Errors
    ///
    /// Returns validation, not-found, or persistence errors.
    pub async fn list_executions(
        &self,
        actor: &Actor,
        schedule_id: Uuid,
        encoded_cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<ExecutionRow>, AppError> {
        let cursor = encoded_cursor
            .map(|cursor| PageCursor::decode(cursor, CursorKind::Executions))
            .transpose()
            .map_err(|_| AppError::Validation)?
            .map(|cursor| ExecutionCursor {
                scheduled_for: cursor.timestamp(),
                id: cursor.id(),
            });
        self.repository
            .list_executions(
                actor.read_keys().as_deref(),
                schedule_id,
                cursor,
                validate_page_limit(limit)?,
            )
            .await
            .map_err(Into::into)
    }

    /// The owners of rows, from the caller's scope first and then from the
    /// identity store (needed in test environments, where everything is readable).
    ///
    /// # Errors
    ///
    /// Returns persistence errors.
    pub async fn owners_for(
        &self,
        actor: &Actor,
        rows: &[ScheduleRow],
    ) -> Result<Owners, AppError> {
        let mut owners = Owners::new();
        let mut missing = HashSet::new();
        for row in rows {
            match actor.owner_of_key(row.owner_principal_id) {
                Some(owner) => {
                    owners.insert(row.owner_principal_id, owner.account.clone());
                }
                None => {
                    missing.insert(row.owner_principal_id);
                }
            }
        }
        if !missing.is_empty() {
            let missing = missing.into_iter().collect::<Vec<_>>();
            owners.extend(self.identity.owners_by_keys(&missing).await?);
        }
        Ok(owners)
    }

    /// Resolves `?silicon_id=` to storage keys among what the caller may read.
    /// A stale cached id is retried through Silicon Accounts; an id the caller
    /// cannot see matches nothing. In a test environment rows that no account
    /// owns are also matched by their creation-time id.
    async fn owner_filter(
        &self,
        actor: &Actor,
        silicon_id: &str,
    ) -> Result<(Option<Vec<Uuid>>, Option<String>), AppError> {
        match &actor.read {
            ReadScope::Owners(owners) => {
                let mut uuid = actor
                    .visible_by_id(silicon_id)
                    .map(|owner| owner.account.uuid.clone());
                if uuid.is_none()
                    && let Ok(Some(account)) = self.identity.resolve_account(silicon_id).await
                    && actor.visible_by_uuid(&account.uuid).is_some()
                {
                    uuid = Some(account.uuid);
                }
                let keys = uuid
                    .map(|uuid| {
                        owners
                            .iter()
                            .filter(|(_, owner)| owner.account.uuid == uuid)
                            .map(|(key, _)| *key)
                            .collect()
                    })
                    .unwrap_or_default();
                Ok((Some(keys), None))
            }
            ReadScope::Everything => {
                let keys = match self.identity.resolve_account(silicon_id).await {
                    Ok(Some(account)) => self.identity.keys_of(&account.uuid).await?,
                    _ => Vec::new(),
                };
                Ok((Some(keys), Some(silicon_id.to_owned())))
            }
        }
    }

    fn idempotency(
        &self,
        actor: &Actor,
        key: String,
        request_hash: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<IdempotencyContext, AppError> {
        let retention = chrono::Duration::from_std(self.idempotency_retention)
            .map_err(|error| AppError::internal("idempotency_retention", error))?;
        let expires_at = now.checked_add_signed(retention).ok_or_else(|| {
            AppError::internal("idempotency_retention", anyhow::anyhow!("overflow"))
        })?;
        Ok(IdempotencyContext {
            actor_type: actor_type(actor.kind),
            actor_id: actor.uuid.clone(),
            key,
            request_hash,
            expires_at,
        })
    }
}

/// Calculates a deterministic SHA-256 fingerprint for a typed JSON request.
///
/// # Errors
///
/// Returns an internal error if the service-owned request type cannot serialize.
pub fn request_hash(request: &impl Serialize) -> Result<[u8; 32], AppError> {
    let bytes = serde_json::to_vec(request)
        .map_err(|error| AppError::internal("request_fingerprint", error))?;
    Ok(Sha256::digest(bytes).into())
}

/// Emits a schedule cursor for a page when another page exists.
///
/// # Errors
///
/// Returns an internal error if the service-owned cursor cannot be encoded.
pub fn next_schedule_cursor(page: &Page<ScheduleRow>) -> Result<Option<String>, AppError> {
    if !page.has_more {
        return Ok(None);
    }
    page.items
        .last()
        .map(|last| PageCursor::Schedules {
            created_at: last.created_at,
            id: last.id,
        })
        .map(PageCursor::encode)
        .transpose()
        .map_err(|error| AppError::internal("schedule_cursor_encoding", error))
}

/// Emits an execution cursor for a page when another page exists.
///
/// # Errors
///
/// Returns an internal error if the service-owned cursor cannot be encoded.
pub fn next_execution_cursor(page: &Page<ExecutionRow>) -> Result<Option<String>, AppError> {
    if !page.has_more {
        return Ok(None);
    }
    page.items
        .last()
        .map(|last| PageCursor::Executions {
            scheduled_for: last.scheduled_for,
            id: last.id,
        })
        .map(PageCursor::encode)
        .transpose()
        .map_err(|error| AppError::internal("execution_cursor_encoding", error))
}

fn schedule_from_row(row: &ScheduleRow) -> Result<Schedule, AppError> {
    let timezone = row
        .timezone
        .parse::<Tz>()
        .map_err(|error| AppError::internal("stored_schedule_timezone", error))?;
    let kind = row
        .schedule_kind
        .parse::<ScheduleKind>()
        .map_err(|error| AppError::internal("stored_schedule_kind", error))?;
    let expression = CronExpression::parse(&row.cron)
        .map_err(|error| AppError::internal("stored_schedule_cron", error))?;
    let status = match row.status.as_str() {
        "active" => ScheduleStatus::Active,
        "paused" => ScheduleStatus::Paused,
        "completed" => ScheduleStatus::Completed,
        _ => {
            return Err(AppError::internal(
                "stored_schedule_status",
                anyhow::anyhow!("unknown schedule status"),
            ));
        }
    };
    let version = u64::try_from(row.version)
        .map_err(|error| AppError::internal("stored_schedule_version", error))?;
    Ok(Schedule {
        id: row.id,
        owner_key: row.owner_principal_id,
        silicon_id: row.silicon_id.clone(),
        text: row.text.clone(),
        timezone,
        timing: ScheduleTiming::new(kind, expression),
        status,
        next_run_at: row.next_run_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
        version,
        deleted_at: row.deleted_at,
    })
}

const fn actor_type(kind: ActorKind) -> ActorType {
    match kind {
        ActorKind::Carbon => ActorType::Carbon,
        ActorKind::Silicon => ActorType::Silicon,
    }
}

/// Audit attribution: the account's uuid.
#[must_use]
pub fn audit_context(actor: &Actor) -> AuditContext {
    AuditContext {
        actor_type: actor_type(actor.kind),
        actor_id: actor.uuid.clone(),
        request_id: request_context::current_request_id(),
    }
}

/// Refuses proofs: another app acting for an account can only read.
///
/// # Errors
///
/// Returns a 403 that says why.
pub fn require_access_token(actor: &Actor) -> Result<(), AppError> {
    if let Credential::Proof { issuing_app, .. } = &actor.credential {
        return Err(AppError::forbidden(
            "proof_cannot_write",
            format!(
                "This request carries a proof from `{issuing_app}`, which can only read reminders. Changes need the account's own Remind access token."
            ),
        ));
    }
    Ok(())
}

/// Refuses proofs (they only read) and Carbons (only Silicons set reminders).
///
/// # Errors
///
/// Returns a 403 that says why.
pub fn require_writing_silicon(actor: &Actor) -> Result<(), AppError> {
    require_access_token(actor)?;
    if actor.is_silicon() {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "silicon_only",
            "Only a Silicon sets and changes reminders. A Carbon sees the reminders of the Silicons it looks after but cannot change them.",
        ))
    }
}

fn require_owner(actor: &Actor, schedule: &ScheduleRow) -> Result<(), AppError> {
    if actor.is_silicon() && actor.owns_key(schedule.owner_principal_id) {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "not_reminder_owner",
            "Only the Silicon that set this reminder can change it; others it is shared with can only read it.",
        ))
    }
}

fn validate_idempotency_key(key: &str) -> Result<(), AppError> {
    if (8..=255).contains(&key.len()) && !key.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(AppError::Validation)
    }
}

fn validate_page_limit(limit: Option<u32>) -> Result<u32, AppError> {
    let limit = limit.unwrap_or(20);
    if (1..=100).contains(&limit) {
        Ok(limit)
    } else {
        Err(AppError::Validation)
    }
}

fn validate_schedule_status_batch(
    schedule_ids: &[Uuid],
    status: ScheduleStatus,
) -> Result<(), AppError> {
    if schedule_ids.is_empty()
        || schedule_ids.len() > MAX_SCHEDULE_STATUS_BATCH_SIZE
        || schedule_ids.iter().copied().collect::<HashSet<_>>().len() != schedule_ids.len()
        || status == ScheduleStatus::Completed
    {
        Err(AppError::Validation)
    } else {
        Ok(())
    }
}

const fn mutable_schedule_status(
    status: ScheduleStatus,
) -> Result<MutableScheduleStatus, AppError> {
    match status {
        ScheduleStatus::Active => Ok(MutableScheduleStatus::Active),
        ScheduleStatus::Paused => Ok(MutableScheduleStatus::Paused),
        ScheduleStatus::Completed => Err(AppError::Validation),
    }
}

fn desired_next_run_at(
    row: &ScheduleRow,
    status: ScheduleStatus,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, AppError> {
    if row.status == schedule_status_name(status) {
        return Ok(row.next_run_at);
    }
    let mut schedule = schedule_from_row(row)?;
    let patch = PatchScheduleCommand {
        status: Some(status),
        ..PatchScheduleCommand::default()
    }
    .validate(&schedule, now)
    .map_err(|error| map_patch_validation(&error))?;
    schedule
        .apply_patch(patch)
        .map_err(|error| map_patch_validation(&error))?;
    Ok(schedule.next_run_at)
}

const fn schedule_status_name(status: ScheduleStatus) -> &'static str {
    match status {
        ScheduleStatus::Active => "active",
        ScheduleStatus::Paused => "paused",
        ScheduleStatus::Completed => "completed",
    }
}

fn map_patch_validation(error: &ScheduleValidationError) -> AppError {
    match error {
        ScheduleValidationError::ArchivedScheduleImmutable
        | ScheduleValidationError::StalePatch { .. }
        | ScheduleValidationError::StatusTransition(_) => {
            AppError::conflict("invalid_schedule_state")
        }
        _ => AppError::Validation,
    }
}

fn mutation_response<T>(mutation: IdempotentMutation<T>, applied_status: u16) -> MutationResponse {
    match mutation {
        IdempotentMutation::Applied { response_body, .. } => MutationResponse {
            status_code: applied_status,
            body: response_body,
        },
        IdempotentMutation::Replayed {
            status_code,
            response_body,
        } => MutationResponse {
            status_code,
            body: response_body,
        },
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use uuid::Uuid;

    use super::{
        desired_next_run_at, request_hash, require_owner, require_writing_silicon,
        validate_idempotency_key, validate_page_limit, validate_schedule_status_batch,
    };
    use crate::domain::{
        ActorKind, Credential, MAX_SCHEDULE_STATUS_BATCH_SIZE, ScheduleStatus, fixtures::actor,
    };
    use crate::infrastructure::postgres::ScheduleRow;

    #[derive(Serialize)]
    struct Input<'a> {
        value: &'a str,
    }

    #[test]
    fn request_fingerprint_is_stable_and_payload_bound() -> anyhow::Result<()> {
        let first = request_hash(&Input { value: "a" })?;
        assert_eq!(first, request_hash(&Input { value: "a" })?);
        assert_ne!(first, request_hash(&Input { value: "b" })?);
        Ok(())
    }

    #[test]
    fn validates_contract_bounds() {
        assert!(validate_idempotency_key("12345678").is_ok());
        assert!(validate_idempotency_key("short").is_err());
        assert_eq!(validate_page_limit(None).ok(), Some(20));
        assert!(validate_page_limit(Some(101)).is_err());
    }

    #[test]
    fn validates_bulk_status_contract() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        assert!(validate_schedule_status_batch(&[first, second], ScheduleStatus::Paused).is_ok());
        assert!(validate_schedule_status_batch(&[], ScheduleStatus::Active).is_err());
        assert!(validate_schedule_status_batch(&[first, first], ScheduleStatus::Paused).is_err());
        assert!(validate_schedule_status_batch(&[first], ScheduleStatus::Completed).is_err());
        assert!(
            validate_schedule_status_batch(
                &vec![Uuid::nil(); MAX_SCHEDULE_STATUS_BATCH_SIZE + 1],
                ScheduleStatus::Active,
            )
            .is_err()
        );
    }

    fn row(owner: Uuid) -> anyhow::Result<ScheduleRow> {
        Ok(ScheduleRow {
            id: Uuid::now_v7(),
            org_id: None,
            owner_principal_id: owner,
            silicon_id: "si:test".to_owned(),
            text: "wake up".to_owned(),
            timezone: "UTC".to_owned(),
            schedule_kind: "recurring".to_owned(),
            cron: "* * * * *".to_owned(),
            status: "paused".to_owned(),
            next_run_at: None,
            version: 2,
            completed_at: None,
            deleted_at: None,
            purge_after: None,
            created_at: "2026-09-01T09:00:00Z".parse()?,
            updated_at: "2026-09-01T09:30:00Z".parse()?,
        })
    }

    #[test]
    fn resumed_schedule_uses_first_cron_occurrence_after_shared_time() -> anyhow::Result<()> {
        let now = "2026-09-01T10:00:30Z".parse()?;
        assert_eq!(
            desired_next_run_at(&row(Uuid::now_v7())?, ScheduleStatus::Active, now)?,
            Some("2026-09-01T10:01:00Z".parse()?)
        );
        Ok(())
    }

    #[test]
    fn only_silicons_with_their_own_token_change_their_own_reminders() -> anyhow::Result<()> {
        let key = Uuid::from_u128(1);
        let silicon = actor("aaa", "si:scout", ActorKind::Silicon, key, vec![]);
        let carbon = actor(
            "ccc",
            "c:ada",
            ActorKind::Carbon,
            Uuid::from_u128(2),
            vec![],
        );
        let mut proof = silicon.clone();
        proof.credential = Credential::Proof {
            issuing_app: "interface".to_owned(),
            scopes: vec!["remind.schedules.read".to_owned()],
        };
        assert!(require_writing_silicon(&silicon).is_ok());
        assert_eq!(
            require_writing_silicon(&carbon)
                .map_err(|error| error.code())
                .err(),
            Some("silicon_only".into())
        );
        assert_eq!(
            require_writing_silicon(&proof)
                .map_err(|error| error.code())
                .err(),
            Some("proof_cannot_write".into())
        );
        assert!(require_owner(&silicon, &row(key)?).is_ok());
        assert!(require_owner(&silicon, &row(Uuid::from_u128(3))?).is_err());
        Ok(())
    }
}
