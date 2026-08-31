//! Organization-scoped schedule and execution-history use cases.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::{
    application::ports::Clock,
    domain::{
        Actor, ActorKind, CreateScheduleCommand, CronExpression, CursorKind, PageCursor,
        PatchScheduleCommand, Schedule, ScheduleStatus, ScheduleTiming, ScheduleValidationError,
        silicon_id_belongs_to_org,
    },
    error::AppError,
    infrastructure::postgres::{
        ActorType, AuditContext, CreateSchedule, ExecutionCursor, ExecutionRow, IdempotencyContext,
        IdempotentMutation, ListSchedules, MutableScheduleStatus, Page, PostgresRepository,
        ScheduleCursor, ScheduleReplacement, ScheduleRow,
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

/// Application service enforcing domain and authorization policy.
#[derive(Clone, Debug)]
pub struct ScheduleService {
    repository: PostgresRepository,
    clock: Arc<dyn Clock>,
    idempotency_retention: Duration,
}

impl ScheduleService {
    /// Creates the service with its durable repository and deterministic clock.
    #[must_use]
    pub fn new(
        repository: PostgresRepository,
        clock: Arc<dyn Clock>,
        idempotency_retention: Duration,
    ) -> Self {
        Self {
            repository,
            clock,
            idempotency_retention,
        }
    }

    /// Returns the underlying repository for readiness and worker composition.
    #[must_use]
    pub const fn repository(&self) -> &PostgresRepository {
        &self.repository
    }

    /// Creates an owned schedule with atomic idempotency replay state.
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
        require_silicon(actor)?;
        validate_idempotency_key(&idempotency_key)?;
        let now = self.clock.now();
        let idempotency = self.idempotency(actor, idempotency_key, request_hash, now)?;
        if let Some(stored) = self
            .repository
            .find_idempotent_response(&actor.org_id, "schedule.create", None, &idempotency)
            .await?
        {
            return Ok(MutationResponse {
                status_code: stored.status_code,
                body: stored.response_body,
            });
        }
        let owner_principal_id = principal_id(actor)?;
        let identity = self
            .repository
            .get_active_silicon_identity(&actor.org_id, owner_principal_id)
            .await?
            .ok_or_else(|| AppError::conflict("silicon_unavailable"))?;
        let validated = command.validate(now).map_err(|_| AppError::Validation)?;
        let schedule = CreateSchedule {
            id: Uuid::now_v7(),
            org_id: actor.org_id.clone(),
            owner_principal_id,
            silicon_id: identity.silicon_id,
            text: validated.text().to_owned(),
            timezone: validated.timezone().name().to_owned(),
            run_at: validated.timing().run_at(),
            cron: validated.timing().cron().map(ToString::to_string),
            next_run_at: validated.next_run_at(),
        };
        let audit = audit_context(actor);
        let mutation = self
            .repository
            .create_schedule_idempotent(&schedule, &idempotency, &audit)
            .await?;
        Ok(mutation_response(mutation, 201))
    }

    /// Lists every visible schedule in the selected organization.
    ///
    /// # Errors
    ///
    /// Returns validation or persistence errors.
    pub async fn list(
        &self,
        actor: &Actor,
        silicon_id: Option<String>,
        status: Option<ScheduleStatus>,
        encoded_cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<ScheduleRow>, AppError> {
        if silicon_id
            .as_deref()
            .is_some_and(|value| !silicon_id_belongs_to_org(value, &actor.org_id))
        {
            return Err(AppError::Validation);
        }
        let cursor = encoded_cursor
            .map(|cursor| PageCursor::decode(cursor, CursorKind::Schedules))
            .transpose()
            .map_err(|_| AppError::Validation)?
            .map(|cursor| ScheduleCursor {
                created_at: cursor.timestamp(),
                id: cursor.id(),
            });
        let filters = ListSchedules {
            org_id: actor.org_id.clone(),
            silicon_id,
            status: status.map(schedule_status_name).map(str::to_owned),
            cursor,
            limit: validate_page_limit(limit)?,
        };
        self.repository
            .list_schedules(&filters)
            .await
            .map_err(Into::into)
    }

    /// Gets one organization-visible schedule.
    ///
    /// # Errors
    ///
    /// Returns not found without leaking cross-organization existence.
    pub async fn get(&self, actor: &Actor, schedule_id: Uuid) -> Result<ScheduleRow, AppError> {
        self.repository
            .get_schedule(&actor.org_id, schedule_id)
            .await?
            .ok_or(AppError::NotFound)
    }

    /// Applies an owner-only schedule merge patch atomically and idempotently.
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
        require_silicon(actor)?;
        validate_idempotency_key(&idempotency_key)?;
        let now = self.clock.now();
        let idempotency = self.idempotency(actor, idempotency_key, request_hash, now)?;
        if let Some(stored) = self
            .repository
            .find_idempotent_response(
                &actor.org_id,
                "schedule.patch",
                Some(schedule_id),
                &idempotency,
            )
            .await?
        {
            return Ok(MutationResponse {
                status_code: stored.status_code,
                body: stored.response_body,
            });
        }
        let current_row = self.get(actor, schedule_id).await?;
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
            org_id: actor.org_id.clone(),
            owner_principal_id: principal_id(actor)?,
            silicon_id: current_row.silicon_id.clone(),
            schedule_id,
            expected_version: current_row.version,
            text: schedule.text.clone(),
            timezone: schedule.timezone.name().to_owned(),
            run_at: schedule.run_at(),
            cron: schedule.cron().map(ToString::to_string),
            status,
            next_run_at: schedule.next_run_at,
        };
        let mutation = self
            .repository
            .replace_schedule_idempotent(&replacement, &idempotency, &audit_context(actor))
            .await?;
        Ok(mutation_response(mutation, 200))
    }

    /// Soft-deletes an owner schedule and prevents future delivery claims.
    ///
    /// # Errors
    ///
    /// Returns not found whenever ownership cannot be established.
    pub async fn delete(&self, actor: &Actor, schedule_id: Uuid) -> Result<(), AppError> {
        require_silicon(actor)?;
        let owner_principal_id = principal_id(actor)?;
        self.repository
            .soft_delete_schedule(
                &actor.org_id,
                owner_principal_id,
                schedule_id,
                self.clock.now(),
                &audit_context(actor),
            )
            .await?;
        Ok(())
    }

    /// Lists execution history for an organization-visible schedule.
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
                &actor.org_id,
                schedule_id,
                cursor,
                validate_page_limit(limit)?,
            )
            .await
            .map_err(Into::into)
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
            actor_id: actor.id.clone(),
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
    let timing = match (row.run_at, row.cron.as_deref()) {
        (Some(run_at), None) => ScheduleTiming::OneTime { run_at },
        (None, Some(cron)) => ScheduleTiming::Recurring {
            expression: CronExpression::parse(cron)
                .map_err(|error| AppError::internal("stored_schedule_cron", error))?,
        },
        _ => {
            return Err(AppError::internal(
                "stored_schedule_timing",
                anyhow::anyhow!("schedule timing invariant violated"),
            ));
        }
    };
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
        org_id: row.org_id.clone(),
        owner_principal_id: row.owner_principal_id.to_string(),
        silicon_id: row.silicon_id.clone(),
        text: row.text.clone(),
        timezone,
        timing,
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

fn audit_context(actor: &Actor) -> AuditContext {
    AuditContext {
        actor_type: actor_type(actor.kind),
        actor_id: actor.id.clone(),
        request_id: request_context::current_request_id(),
    }
}

fn require_silicon(actor: &Actor) -> Result<(), AppError> {
    if actor.is_silicon() {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn require_owner(actor: &Actor, schedule: &ScheduleRow) -> Result<(), AppError> {
    if actor.is_silicon() && actor.id == schedule.owner_principal_id.to_string() {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn principal_id(actor: &Actor) -> Result<Uuid, AppError> {
    Uuid::parse_str(&actor.id).map_err(|_| AppError::Unauthenticated)
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

fn mutation_response(
    mutation: IdempotentMutation<ScheduleRow>,
    applied_status: u16,
) -> MutationResponse {
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

    use super::{request_hash, validate_idempotency_key, validate_page_limit};

    #[derive(Serialize)]
    struct Input<'a> {
        value: &'a str,
    }

    #[test]
    fn request_fingerprint_is_stable_and_payload_bound() -> anyhow::Result<()> {
        let first = request_hash(&Input { value: "a" })?;
        let replay = request_hash(&Input { value: "a" })?;
        let different = request_hash(&Input { value: "b" })?;
        assert_eq!(first, replay);
        assert_ne!(first, different);
        Ok(())
    }

    #[test]
    fn validates_contract_bounds() {
        assert!(validate_idempotency_key("12345678").is_ok());
        assert!(validate_idempotency_key("short").is_err());
        assert_eq!(validate_page_limit(None).ok(), Some(20));
        assert!(validate_page_limit(Some(101)).is_err());
    }
}
