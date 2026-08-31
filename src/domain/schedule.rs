//! Schedule definitions, command validation, and lifecycle transitions.

use std::{fmt, str::FromStr};

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use uuid::Uuid;

/// Maximum UTF-8 size of reminder text accepted by the public contract.
pub const MAX_TEXT_BYTES: usize = 100_000;

/// Number of days for which completed and deleted schedules are retained.
pub const ARCHIVE_RETENTION_DAYS: i64 = 45;

/// Public lifecycle states for a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleStatus {
    /// Future occurrences may be materialized.
    Active,
    /// Future occurrences are suppressed until the owner resumes the schedule.
    Paused,
    /// A one-time schedule reached a worker-owned terminal state.
    Completed,
}

impl ScheduleStatus {
    fn validate_client_transition(self, target: Self) -> Result<(), ScheduleStatusTransitionError> {
        if self == Self::Completed {
            return Err(ScheduleStatusTransitionError::CompletedIsTerminal);
        }
        if target == Self::Completed {
            return Err(ScheduleStatusTransitionError::ClientCannotComplete);
        }
        Ok(())
    }
}

/// A validated five-field Linux cron expression.
///
/// The stored value is normalized to one ASCII space between fields. Its day
/// fields follow Vixie cron semantics: Sunday is `0` or `7`, and restricted
/// day-of-month and day-of-week fields are matched in union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpression {
    source: String,
}

impl CronExpression {
    /// Parses and validates exactly five cron fields.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleValidationError::InvalidCronFieldCount`] unless the
    /// input has exactly five whitespace-separated fields, or
    /// [`ScheduleValidationError::InvalidCronExpression`] when those fields do
    /// not form a valid Linux cron expression.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ScheduleValidationError> {
        let fields: Vec<&str> = value.as_ref().split_whitespace().collect();
        if fields.len() != 5 {
            return Err(ScheduleValidationError::InvalidCronFieldCount {
                actual: fields.len(),
            });
        }

        validate_standard_cron_fields(&fields)?;

        let source = fields.join(" ");
        cronexpr::parse_crontab(&format!("{source} UTC")).map_err(|error| {
            ScheduleValidationError::InvalidCronExpression {
                reason: error.to_string(),
            }
        })?;

        Ok(Self { source })
    }

    /// Returns the normalized five-field source expression.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Calculates the first matching UTC instant strictly after `after`.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleValidationError::NoFutureOccurrence`] when the
    /// expression has no representable occurrence after `after`.
    pub fn next_after(
        &self,
        timezone: Tz,
        after: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, ScheduleValidationError> {
        next_recurring_occurrence(self, timezone, after)
    }
}

impl fmt::Display for CronExpression {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.source)
    }
}

impl Serialize for CronExpression {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.source)
    }
}

impl<'de> Deserialize<'de> for CronExpression {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let source = String::deserialize(deserializer)?;
        Self::parse(source).map_err(de::Error::custom)
    }
}

impl FromStr for CronExpression {
    type Err = ScheduleValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// The mutually exclusive timing representation for a schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleTiming {
    /// A single execution at an absolute UTC instant.
    OneTime {
        /// The immutable absolute execution instant.
        run_at: DateTime<Utc>,
    },
    /// Repeated wall-clock evaluation in the schedule's IANA timezone.
    Recurring {
        /// The validated five-field expression.
        expression: CronExpression,
    },
}

impl ScheduleTiming {
    /// Returns the one-time instant, if this is a one-time schedule.
    #[must_use]
    pub const fn run_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::OneTime { run_at } => Some(*run_at),
            Self::Recurring { .. } => None,
        }
    }

    /// Returns the cron expression, if this is a recurring schedule.
    #[must_use]
    pub const fn cron(&self) -> Option<&CronExpression> {
        match self {
            Self::OneTime { .. } => None,
            Self::Recurring { expression } => Some(expression),
        }
    }

    /// Returns whether this schedule has exactly one execution.
    #[must_use]
    pub const fn is_one_time(&self) -> bool {
        matches!(self, Self::OneTime { .. })
    }

    fn next_after(
        &self,
        timezone: Tz,
        after: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, ScheduleValidationError> {
        match self {
            Self::OneTime { run_at } => Ok(*run_at),
            Self::Recurring { expression } => expression.next_after(timezone, after),
        }
    }
}

/// A validated schedule that is ready to be assigned identity and ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSchedule {
    text: String,
    timezone: Tz,
    timing: ScheduleTiming,
    next_run_at: DateTime<Utc>,
}

impl NewSchedule {
    /// Returns the reminder text exactly as submitted.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the validated IANA timezone.
    #[must_use]
    pub const fn timezone(&self) -> Tz {
        self.timezone
    }

    /// Returns the validated timing definition.
    #[must_use]
    pub const fn timing(&self) -> &ScheduleTiming {
        &self.timing
    }

    /// Returns the first execution instant.
    #[must_use]
    pub const fn next_run_at(&self) -> DateTime<Utc> {
        self.next_run_at
    }
}

/// Raw fields accepted by schedule creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateScheduleCommand {
    /// Reminder text, preserved verbatim after validation.
    pub text: String,
    /// IANA timezone name.
    pub timezone: String,
    /// Absolute instant for a one-time schedule.
    pub run_at: Option<DateTime<Utc>>,
    /// Five-field expression for a recurring schedule.
    pub cron: Option<String>,
}

impl CreateScheduleCommand {
    /// Validates the command and calculates its initial execution instant.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleValidationError`] when text or timezone validation
    /// fails, the timing fields are missing or conflict, `run_at` is not in the
    /// future, the cron expression is invalid, or no future occurrence exists.
    pub fn validate(self, now: DateTime<Utc>) -> Result<NewSchedule, ScheduleValidationError> {
        let text = validate_text(self.text)?;
        let timezone = parse_timezone(&self.timezone)?;
        let timing = match (self.run_at, self.cron) {
            (Some(run_at), None) => {
                validate_future_run_at(run_at, now)?;
                ScheduleTiming::OneTime { run_at }
            }
            (None, Some(expression)) => ScheduleTiming::Recurring {
                expression: CronExpression::parse(expression)?,
            },
            (None, None) => return Err(ScheduleValidationError::TimingRequired),
            (Some(_), Some(_)) => return Err(ScheduleValidationError::TimingConflict),
        };
        let next_run_at = timing.next_after(timezone, now)?;

        Ok(NewSchedule {
            text,
            timezone,
            timing,
            next_run_at,
        })
    }
}

/// Tri-state semantics for nullable fields in a merge patch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PatchValue<T> {
    /// The property was omitted and its stored value is retained.
    #[default]
    Unchanged,
    /// The property was explicitly set to JSON `null`.
    Clear,
    /// The property was supplied with a new value.
    Set(T),
}

impl<T> PatchValue<T> {
    /// Returns whether the property was omitted.
    #[must_use]
    pub const fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

/// Raw fields accepted by schedule patching.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PatchScheduleCommand {
    /// Replacement text when present.
    pub text: Option<String>,
    /// Replacement IANA timezone when present.
    pub timezone: Option<String>,
    /// One-time instant merge-patch operation.
    pub run_at: PatchValue<DateTime<Utc>>,
    /// Cron expression merge-patch operation.
    pub cron: PatchValue<String>,
    /// Requested public lifecycle status.
    pub status: Option<ScheduleStatus>,
}

impl PatchScheduleCommand {
    /// Merges and validates a patch against a locked current schedule.
    ///
    /// The returned patch records the current version, preventing accidental
    /// application to a different in-memory revision.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleValidationError`] if the patch is empty, targets an
    /// archived schedule, produces invalid content or timing, requests a
    /// forbidden status transition, or has no representable next occurrence.
    pub fn validate(
        self,
        current: &Schedule,
        now: DateTime<Utc>,
    ) -> Result<ValidatedSchedulePatch, ScheduleValidationError> {
        if self.text.is_none()
            && self.timezone.is_none()
            && self.run_at.is_unchanged()
            && self.cron.is_unchanged()
            && self.status.is_none()
        {
            return Err(ScheduleValidationError::NoFieldsToUpdate);
        }
        if current.deleted_at.is_some() {
            return Err(ScheduleValidationError::ArchivedScheduleImmutable);
        }

        let text = match self.text {
            Some(text) => validate_text(text)?,
            None => current.text.clone(),
        };
        let timezone = match self.timezone {
            Some(timezone) => parse_timezone(&timezone)?,
            None => current.timezone,
        };

        let run_at_was_set = matches!(self.run_at, PatchValue::Set(_));
        let merged_run_at = merge_patch_value(self.run_at, current.timing.run_at());
        if run_at_was_set && let Some(run_at) = merged_run_at {
            validate_future_run_at(run_at, now)?;
        }

        let merged_cron = match self.cron {
            PatchValue::Unchanged => current.timing.cron().cloned(),
            PatchValue::Clear => None,
            PatchValue::Set(expression) => Some(CronExpression::parse(expression)?),
        };
        let timing = match (merged_run_at, merged_cron) {
            (Some(run_at), None) => ScheduleTiming::OneTime { run_at },
            (None, Some(expression)) => ScheduleTiming::Recurring { expression },
            (None, None) => return Err(ScheduleValidationError::TimingRequired),
            (Some(_), Some(_)) => return Err(ScheduleValidationError::TimingConflict),
        };

        let status = self.status.unwrap_or(current.status);
        current.status.validate_client_transition(status)?;

        let timing_changed = timing != current.timing;
        let timezone_changes_recurrence = timezone != current.timezone && timing.cron().is_some();
        let resumed = current.status == ScheduleStatus::Paused && status == ScheduleStatus::Active;
        let next_run_at = match status {
            ScheduleStatus::Paused => None,
            ScheduleStatus::Active if timing_changed || timezone_changes_recurrence || resumed => {
                Some(timing.next_after(timezone, now)?)
            }
            ScheduleStatus::Active => current.next_run_at,
            ScheduleStatus::Completed => {
                return Err(ScheduleStatusTransitionError::ClientCannotComplete.into());
            }
        };

        Ok(ValidatedSchedulePatch {
            expected_version: current.version,
            text,
            timezone,
            timing,
            status,
            next_run_at,
            updated_at: now,
        })
    }
}

/// A fully merged and validated schedule patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSchedulePatch {
    expected_version: u64,
    text: String,
    timezone: Tz,
    timing: ScheduleTiming,
    status: ScheduleStatus,
    next_run_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

impl ValidatedSchedulePatch {
    /// Returns the version against which this patch was validated.
    #[must_use]
    pub const fn expected_version(&self) -> u64 {
        self.expected_version
    }
}

/// A persisted schedule aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Stable schedule identifier.
    pub id: Uuid,
    /// Owning organization identifier.
    pub org_id: String,
    /// Stable IAM identifier of the owning Silicon principal.
    pub owner_principal_id: String,
    /// Public global identifier of the owning Silicon.
    pub silicon_id: String,
    /// Reminder text delivered in future execution snapshots.
    pub text: String,
    /// IANA timezone retained for wall-clock evaluation and display.
    pub timezone: Tz,
    /// Exactly one one-time or recurring timing definition.
    pub timing: ScheduleTiming,
    /// Public lifecycle state.
    pub status: ScheduleStatus,
    /// Next due occurrence, or `None` while paused/completed/materialized.
    pub next_run_at: Option<DateTime<Utc>>,
    /// Creation time in UTC.
    pub created_at: DateTime<Utc>,
    /// Last mutation time in UTC.
    pub updated_at: DateTime<Utc>,
    /// Internal monotonic optimistic version.
    pub version: u64,
    /// Soft-deletion time; deleted schedules are hidden from public reads.
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Schedule {
    /// Assigns identity and ownership to a validated new schedule.
    #[must_use]
    pub fn new(
        id: Uuid,
        org_id: impl Into<String>,
        owner_principal_id: impl Into<String>,
        silicon_id: impl Into<String>,
        new_schedule: NewSchedule,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            org_id: org_id.into(),
            owner_principal_id: owner_principal_id.into(),
            silicon_id: silicon_id.into(),
            text: new_schedule.text,
            timezone: new_schedule.timezone,
            timing: new_schedule.timing,
            status: ScheduleStatus::Active,
            next_run_at: Some(new_schedule.next_run_at),
            created_at,
            updated_at: created_at,
            version: 1,
            deleted_at: None,
        }
    }

    /// Returns the one-time instant exposed by the public API.
    #[must_use]
    pub const fn run_at(&self) -> Option<DateTime<Utc>> {
        self.timing.run_at()
    }

    /// Returns the recurring expression exposed by the public API.
    #[must_use]
    pub const fn cron(&self) -> Option<&CronExpression> {
        self.timing.cron()
    }

    /// Applies a patch only to the exact revision against which it was validated.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleValidationError::StalePatch`] if the aggregate version
    /// changed after validation, or [`ScheduleValidationError::VersionExhausted`]
    /// if its monotonic version cannot be advanced.
    pub fn apply_patch(
        &mut self,
        patch: ValidatedSchedulePatch,
    ) -> Result<(), ScheduleValidationError> {
        if self.version != patch.expected_version {
            return Err(ScheduleValidationError::StalePatch {
                expected: patch.expected_version,
                actual: self.version,
            });
        }
        let version = next_version(self.version)?;

        self.text = patch.text;
        self.timezone = patch.timezone;
        self.timing = patch.timing;
        self.status = patch.status;
        self.next_run_at = patch.next_run_at;
        self.updated_at = patch.updated_at;
        self.version = version;
        Ok(())
    }

    /// Marks a one-time schedule completed after terminal delivery processing.
    ///
    /// Repeating the operation on an already completed schedule is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleValidationError::ArchivedScheduleImmutable`] for a
    /// deleted schedule, a status-transition error for a recurring schedule, or
    /// [`ScheduleValidationError::VersionExhausted`] if the version cannot be
    /// advanced.
    pub fn complete_one_time(
        &mut self,
        completed_at: DateTime<Utc>,
    ) -> Result<(), ScheduleValidationError> {
        if self.deleted_at.is_some() {
            return Err(ScheduleValidationError::ArchivedScheduleImmutable);
        }
        if !self.timing.is_one_time() {
            return Err(ScheduleStatusTransitionError::RecurringCannotComplete.into());
        }
        if self.status == ScheduleStatus::Completed {
            return Ok(());
        }
        let version = next_version(self.version)?;
        self.status = ScheduleStatus::Completed;
        self.next_run_at = None;
        self.updated_at = completed_at;
        self.version = version;
        Ok(())
    }

    /// Soft-deletes a schedule and prevents further occurrence materialization.
    ///
    /// Repeating deletion is idempotent and does not advance the version twice.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleValidationError::VersionExhausted`] if the schedule's
    /// monotonic version cannot be advanced.
    pub fn soft_delete(
        &mut self,
        deleted_at: DateTime<Utc>,
    ) -> Result<(), ScheduleValidationError> {
        if self.deleted_at.is_some() {
            return Ok(());
        }
        let version = next_version(self.version)?;
        self.deleted_at = Some(deleted_at);
        self.next_run_at = None;
        self.updated_at = deleted_at;
        self.version = version;
        Ok(())
    }

    /// Returns the permanent-purge deadline for an archived schedule.
    #[must_use]
    pub fn retention_deadline(&self) -> Option<DateTime<Utc>> {
        let archived_at = self
            .deleted_at
            .or_else(|| (self.status == ScheduleStatus::Completed).then_some(self.updated_at))?;
        archived_at.checked_add_signed(Duration::days(ARCHIVE_RETENTION_DAYS))
    }
}

/// Invalid client-visible schedule status transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ScheduleStatusTransitionError {
    /// Public clients cannot select the worker-owned completed state.
    #[error("clients cannot set a schedule to completed")]
    ClientCannotComplete,
    /// Completed schedules cannot be changed by public clients.
    #[error("a completed schedule is terminal")]
    CompletedIsTerminal,
    /// Recurring schedules never transition to completed.
    #[error("a recurring schedule cannot be completed")]
    RecurringCannotComplete,
}

/// Validation failures for schedule creation, patching, and calculation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScheduleValidationError {
    /// Text is empty after trimming Unicode whitespace.
    #[error("reminder text must not be empty")]
    EmptyText,
    /// Text exceeds the public contract's UTF-8 byte limit.
    #[error("reminder text is {actual} bytes; the maximum is {maximum}")]
    TextTooLong {
        /// Actual UTF-8 byte count.
        actual: usize,
        /// Accepted maximum byte count.
        maximum: usize,
    },
    /// The timezone is not present in the IANA timezone database.
    #[error("timezone `{timezone}` is not a valid IANA identifier")]
    InvalidTimezone {
        /// Rejected timezone identifier.
        timezone: String,
    },
    /// Neither timing representation remains after validation.
    #[error("exactly one of run_at or cron is required")]
    TimingRequired,
    /// Both mutually exclusive timing representations remain after validation.
    #[error("run_at and cron cannot both be set")]
    TimingConflict,
    /// A newly supplied one-time instant is not strictly in the future.
    #[error("run_at must be strictly after the current time")]
    RunAtNotFuture,
    /// A cron expression does not contain exactly five fields.
    #[error("cron must contain exactly five fields, received {actual}")]
    InvalidCronFieldCount {
        /// Number of whitespace-separated fields received.
        actual: usize,
    },
    /// The cron parser rejected a five-field expression.
    #[error("cron expression is invalid: {reason}")]
    InvalidCronExpression {
        /// Parser diagnostic without surrounding request data.
        reason: String,
    },
    /// A valid expression has no representable future occurrence.
    #[error("cron expression has no future occurrence")]
    NoFutureOccurrence,
    /// A patch contained no supported fields.
    #[error("at least one patch field is required")]
    NoFieldsToUpdate,
    /// A deleted schedule cannot be mutated.
    #[error("an archived schedule cannot be changed")]
    ArchivedScheduleImmutable,
    /// A validated patch was applied after the aggregate changed.
    #[error("patch was validated for version {expected}, current version is {actual}")]
    StalePatch {
        /// Version captured during validation.
        expected: u64,
        /// Version observed while applying.
        actual: u64,
    },
    /// The monotonic schedule version cannot be advanced.
    #[error("schedule version is exhausted")]
    VersionExhausted,
    /// A lifecycle transition violates schedule policy.
    #[error(transparent)]
    StatusTransition(#[from] ScheduleStatusTransitionError),
}

/// Calculates the first recurring occurrence strictly after a UTC instant.
///
/// Cron fields are interpreted in `timezone`. Nonexistent wall-clock values are
/// skipped, while both UTC instants in a repeated local interval are returned
/// on successive calls.
///
/// # Errors
///
/// Returns [`ScheduleValidationError::NoFutureOccurrence`] when the expression
/// has no representable occurrence strictly after `after`.
pub fn next_recurring_occurrence(
    expression: &CronExpression,
    timezone: Tz,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>, ScheduleValidationError> {
    let crontab = cronexpr::parse_crontab(&format!("{} {}", expression.as_str(), timezone.name()))
        .map_err(|error| ScheduleValidationError::InvalidCronExpression {
            reason: error.to_string(),
        })?;
    let start = after.to_rfc3339();
    let next = crontab
        .find_next(start.as_str())
        .map_err(|_| ScheduleValidationError::NoFutureOccurrence)?;
    let timestamp = next.timestamp();
    let nanosecond = u32::try_from(timestamp.subsec_nanosecond())
        .map_err(|_| ScheduleValidationError::NoFutureOccurrence)?;

    DateTime::from_timestamp(timestamp.as_second(), nanosecond)
        .ok_or(ScheduleValidationError::NoFutureOccurrence)
}

fn validate_standard_cron_fields(fields: &[&str]) -> Result<(), ScheduleValidationError> {
    // `cronexpr` intentionally supports Quartz-style L/W/# extensions as a
    // superset. The public contract is Vixie/Linux crontab, so reject those
    // constructs before invoking the otherwise-compatible parser.
    if fields[2].bytes().any(|byte| byte.is_ascii_alphabetic())
        || fields[4].contains(['L', 'l', '#'])
    {
        return Err(ScheduleValidationError::InvalidCronExpression {
            reason: "non-standard day-field extension".to_owned(),
        });
    }

    Ok(())
}

fn validate_text(text: String) -> Result<String, ScheduleValidationError> {
    if text.trim().is_empty() {
        return Err(ScheduleValidationError::EmptyText);
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(ScheduleValidationError::TextTooLong {
            actual: text.len(),
            maximum: MAX_TEXT_BYTES,
        });
    }
    Ok(text)
}

fn parse_timezone(timezone: &str) -> Result<Tz, ScheduleValidationError> {
    Tz::from_str(timezone).map_err(|_| ScheduleValidationError::InvalidTimezone {
        timezone: timezone.to_owned(),
    })
}

fn validate_future_run_at(
    run_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), ScheduleValidationError> {
    if run_at <= now {
        return Err(ScheduleValidationError::RunAtNotFuture);
    }
    Ok(())
}

fn merge_patch_value<T>(patch: PatchValue<T>, current: Option<T>) -> Option<T> {
    match patch {
        PatchValue::Unchanged => current,
        PatchValue::Clear => None,
        PatchValue::Set(value) => Some(value),
    }
}

fn next_version(version: u64) -> Result<u64, ScheduleValidationError> {
    version
        .checked_add(1)
        .ok_or(ScheduleValidationError::VersionExhausted)
}

#[cfg(test)]
mod tests {
    use chrono::{Datelike, TimeZone, Timelike};
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    fn instant(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
    ) -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .ok_or_else(|| "test instant must be valid".into())
    }

    fn recurring_command() -> CreateScheduleCommand {
        CreateScheduleCommand {
            text: "Daily report".to_owned(),
            timezone: "Asia/Kolkata".to_owned(),
            run_at: None,
            cron: Some("0 9 * * *".to_owned()),
        }
    }

    fn active_recurring_schedule() -> Result<Schedule, Box<dyn std::error::Error>> {
        let now = instant(2026, 8, 31, 9, 0)?;
        let validated = recurring_command().validate(now)?;
        Ok(Schedule::new(
            Uuid::from_u128(7),
            "org-1",
            "00000000-0000-0000-0000-000000000007",
            "silicon-1",
            validated,
            now,
        ))
    }

    #[test]
    fn create_requires_exactly_one_timing_representation() -> Result<(), Box<dyn std::error::Error>>
    {
        let now = instant(2026, 8, 31, 9, 0)?;
        let neither = CreateScheduleCommand {
            text: "Reminder".to_owned(),
            timezone: "UTC".to_owned(),
            run_at: None,
            cron: None,
        };
        let both = CreateScheduleCommand {
            text: "Reminder".to_owned(),
            timezone: "UTC".to_owned(),
            run_at: Some(now + Duration::hours(1)),
            cron: Some("0 9 * * *".to_owned()),
        };

        assert_eq!(
            neither.validate(now),
            Err(ScheduleValidationError::TimingRequired)
        );
        assert_eq!(
            both.validate(now),
            Err(ScheduleValidationError::TimingConflict)
        );
        Ok(())
    }

    #[test]
    fn text_is_preserved_and_limited_by_utf8_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let now = instant(2026, 8, 31, 9, 0)?;
        let exact = "é".repeat(MAX_TEXT_BYTES / 2);
        let command = CreateScheduleCommand {
            text: exact.clone(),
            timezone: "UTC".to_owned(),
            run_at: Some(now + Duration::minutes(1)),
            cron: None,
        };
        let validated = command.validate(now)?;
        assert_eq!(validated.text(), exact);

        let too_long = CreateScheduleCommand {
            text: format!("{exact}é"),
            timezone: "UTC".to_owned(),
            run_at: Some(now + Duration::minutes(1)),
            cron: None,
        };
        assert_eq!(
            too_long.validate(now),
            Err(ScheduleValidationError::TextTooLong {
                actual: MAX_TEXT_BYTES + 2,
                maximum: MAX_TEXT_BYTES,
            })
        );
        Ok(())
    }

    #[test]
    fn whitespace_only_text_is_empty_but_nonempty_whitespace_is_preserved()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = instant(2026, 8, 31, 9, 0)?;
        let empty = CreateScheduleCommand {
            text: " \n\t ".to_owned(),
            timezone: "UTC".to_owned(),
            run_at: Some(now + Duration::minutes(1)),
            cron: None,
        };
        assert_eq!(empty.validate(now), Err(ScheduleValidationError::EmptyText));

        let original = "  keep these spaces  ";
        let valid = CreateScheduleCommand {
            text: original.to_owned(),
            timezone: "UTC".to_owned(),
            run_at: Some(now + Duration::minutes(1)),
            cron: None,
        }
        .validate(now)?;
        assert_eq!(valid.text(), original);
        Ok(())
    }

    #[test]
    fn one_time_instant_must_be_strictly_future() -> Result<(), Box<dyn std::error::Error>> {
        let now = instant(2026, 8, 31, 9, 0)?;
        for run_at in [now - Duration::seconds(1), now] {
            let command = CreateScheduleCommand {
                text: "Reminder".to_owned(),
                timezone: "UTC".to_owned(),
                run_at: Some(run_at),
                cron: None,
            };
            assert_eq!(
                command.validate(now),
                Err(ScheduleValidationError::RunAtNotFuture)
            );
        }
        Ok(())
    }

    #[test]
    fn timezone_must_be_an_iana_identifier() -> Result<(), Box<dyn std::error::Error>> {
        let now = instant(2026, 8, 31, 9, 0)?;
        let command = CreateScheduleCommand {
            text: "Reminder".to_owned(),
            timezone: "IST".to_owned(),
            run_at: Some(now + Duration::minutes(1)),
            cron: None,
        };

        assert_eq!(
            command.validate(now),
            Err(ScheduleValidationError::InvalidTimezone {
                timezone: "IST".to_owned(),
            })
        );
        Ok(())
    }

    #[test]
    fn cron_requires_five_valid_fields_and_normalizes_spacing() {
        assert_eq!(
            CronExpression::parse("0 9 * *"),
            Err(ScheduleValidationError::InvalidCronFieldCount { actual: 4 })
        );
        assert!(matches!(
            CronExpression::parse("99 9 * * *"),
            Err(ScheduleValidationError::InvalidCronExpression { .. })
        ));

        let parsed = CronExpression::parse("  0\t9  *  * *  ");
        let Ok(parsed) = parsed else {
            panic!("valid five-field expression must parse");
        };
        assert_eq!(parsed.as_str(), "0 9 * * *");
    }

    #[test]
    fn cron_uses_vixie_sunday_numbering_and_named_weekdays()
    -> Result<(), Box<dyn std::error::Error>> {
        let after = instant(2026, 8, 31, 10, 0)?;
        let expected_sunday = instant(2026, 9, 6, 9, 0)?;

        for source in ["0 9 * * 0", "0 9 * * 7", "0 9 * * SUN"] {
            let expression = CronExpression::parse(source)?;
            assert_eq!(
                expression.next_after(chrono_tz::UTC, after)?,
                expected_sunday
            );
        }
        Ok(())
    }

    #[test]
    fn cron_matches_restricted_day_fields_in_union() -> Result<(), Box<dyn std::error::Error>> {
        let expression = CronExpression::parse("0 9 1 * MON")?;

        // September 1, 2026 is a Tuesday, so the first occurrence is selected
        // by day-of-month and the second by day-of-week.
        let first = expression.next_after(chrono_tz::UTC, instant(2026, 8, 31, 10, 0)?)?;
        let monday = expression.next_after(chrono_tz::UTC, first)?;

        assert_eq!(first, instant(2026, 9, 1, 9, 0)?);
        assert_eq!(monday, instant(2026, 9, 7, 9, 0)?);
        Ok(())
    }

    #[test]
    fn cron_rejects_non_vixie_day_field_extensions() {
        for source in ["0 9 L * *", "0 9 1W * *", "0 9 * * 5L", "0 9 * * 5#3"] {
            assert!(matches!(
                CronExpression::parse(source),
                Err(ScheduleValidationError::InvalidCronExpression { .. })
            ));
        }
    }

    #[test]
    fn recurring_occurrence_is_strictly_after_in_the_selected_zone()
    -> Result<(), Box<dyn std::error::Error>> {
        let expression = CronExpression::parse("0 9 * * *")?;
        let after = instant(2026, 8, 31, 3, 30)?;
        let next = expression.next_after(chrono_tz::Asia::Kolkata, after)?;

        assert_eq!(next, instant(2026, 9, 1, 3, 30)?);
        assert!(next > after);
        Ok(())
    }

    #[test]
    fn nonexistent_dst_occurrence_is_skipped() -> Result<(), Box<dyn std::error::Error>> {
        let expression = CronExpression::parse("30 2 * * *")?;
        let before_gap = instant(2026, 3, 8, 0, 0)?;
        let next = expression.next_after(chrono_tz::America::New_York, before_gap)?;

        assert_eq!(next, instant(2026, 3, 9, 6, 30)?);
        Ok(())
    }

    #[test]
    fn both_instants_in_a_dst_fold_are_returned() -> Result<(), Box<dyn std::error::Error>> {
        let expression = CronExpression::parse("30 1 * * *")?;
        let before_first = instant(2026, 11, 1, 5, 0)?;
        let first = expression.next_after(chrono_tz::America::New_York, before_first)?;
        let second = expression.next_after(chrono_tz::America::New_York, first)?;

        assert_eq!(first, instant(2026, 11, 1, 5, 30)?);
        assert_eq!(second, instant(2026, 11, 1, 6, 30)?);
        Ok(())
    }

    #[test]
    fn switching_timing_requires_explicitly_clearing_the_old_field()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut schedule = active_recurring_schedule()?;
        let now = instant(2026, 8, 31, 10, 0)?;
        let run_at = now + Duration::hours(2);
        let conflict = PatchScheduleCommand {
            run_at: PatchValue::Set(run_at),
            ..PatchScheduleCommand::default()
        };
        assert_eq!(
            conflict.validate(&schedule, now),
            Err(ScheduleValidationError::TimingConflict)
        );

        let valid = PatchScheduleCommand {
            run_at: PatchValue::Set(run_at),
            cron: PatchValue::Clear,
            ..PatchScheduleCommand::default()
        }
        .validate(&schedule, now)?;
        schedule.apply_patch(valid)?;
        assert_eq!(schedule.run_at(), Some(run_at));
        assert!(schedule.cron().is_none());
        assert_eq!(schedule.next_run_at, Some(run_at));
        Ok(())
    }

    #[test]
    fn pause_and_resume_are_client_owned_but_completion_is_not()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut schedule = active_recurring_schedule()?;
        let pause_at = instant(2026, 8, 31, 10, 0)?;
        let pause = PatchScheduleCommand {
            status: Some(ScheduleStatus::Paused),
            ..PatchScheduleCommand::default()
        }
        .validate(&schedule, pause_at)?;
        schedule.apply_patch(pause)?;
        assert_eq!(schedule.status, ScheduleStatus::Paused);
        assert_eq!(schedule.next_run_at, None);

        let resume_at = instant(2026, 8, 31, 11, 0)?;
        let resume = PatchScheduleCommand {
            status: Some(ScheduleStatus::Active),
            ..PatchScheduleCommand::default()
        }
        .validate(&schedule, resume_at)?;
        schedule.apply_patch(resume)?;
        assert_eq!(schedule.status, ScheduleStatus::Active);
        assert!(schedule.next_run_at.is_some_and(|next| next > resume_at));

        let complete = PatchScheduleCommand {
            status: Some(ScheduleStatus::Completed),
            ..PatchScheduleCommand::default()
        };
        assert_eq!(
            complete.validate(&schedule, resume_at),
            Err(ScheduleValidationError::StatusTransition(
                ScheduleStatusTransitionError::ClientCannotComplete
            ))
        );
        Ok(())
    }

    #[test]
    fn text_only_patch_does_not_shift_the_next_occurrence() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut schedule = active_recurring_schedule()?;
        let original_next = schedule.next_run_at;
        let patch_at = instant(2026, 8, 31, 10, 0)?;
        let patch = PatchScheduleCommand {
            text: Some("Updated report".to_owned()),
            ..PatchScheduleCommand::default()
        }
        .validate(&schedule, patch_at)?;
        schedule.apply_patch(patch)?;

        assert_eq!(schedule.text, "Updated report");
        assert_eq!(schedule.next_run_at, original_next);
        Ok(())
    }

    #[test]
    fn validated_patch_cannot_be_applied_to_a_newer_revision()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut schedule = active_recurring_schedule()?;
        let patch_at = instant(2026, 8, 31, 10, 0)?;
        let stale = PatchScheduleCommand {
            text: Some("Stale".to_owned()),
            ..PatchScheduleCommand::default()
        }
        .validate(&schedule, patch_at)?;
        let current = PatchScheduleCommand {
            text: Some("Current".to_owned()),
            ..PatchScheduleCommand::default()
        }
        .validate(&schedule, patch_at)?;
        schedule.apply_patch(current)?;

        assert_eq!(
            schedule.apply_patch(stale),
            Err(ScheduleValidationError::StalePatch {
                expected: 1,
                actual: 2,
            })
        );
        Ok(())
    }

    #[test]
    fn only_one_time_worker_flow_can_complete_a_schedule() -> Result<(), Box<dyn std::error::Error>>
    {
        let now = instant(2026, 8, 31, 9, 0)?;
        let one_time = CreateScheduleCommand {
            text: "Reminder".to_owned(),
            timezone: "UTC".to_owned(),
            run_at: Some(now + Duration::minutes(1)),
            cron: None,
        }
        .validate(now)?;
        let mut one_time = Schedule::new(
            Uuid::from_u128(1),
            "org-1",
            "00000000-0000-0000-0000-000000000001",
            "silicon-1",
            one_time,
            now,
        );
        one_time.complete_one_time(now + Duration::minutes(2))?;
        assert_eq!(one_time.status, ScheduleStatus::Completed);
        assert_eq!(one_time.next_run_at, None);
        let version = one_time.version;
        one_time.complete_one_time(now + Duration::minutes(3))?;
        assert_eq!(one_time.version, version);

        let mut recurring = active_recurring_schedule()?;
        assert_eq!(
            recurring.complete_one_time(now),
            Err(ScheduleValidationError::StatusTransition(
                ScheduleStatusTransitionError::RecurringCannotComplete
            ))
        );
        Ok(())
    }

    #[test]
    fn retention_deadline_is_45_days_after_archive() -> Result<(), Box<dyn std::error::Error>> {
        let now = instant(2026, 8, 31, 9, 0)?;
        let mut schedule = active_recurring_schedule()?;
        schedule.soft_delete(now)?;
        assert_eq!(
            schedule.retention_deadline(),
            now.checked_add_signed(Duration::days(ARCHIVE_RETENTION_DAYS))
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn valid_daily_crons_always_advance_strictly(
            minute in 0_u32..60,
            hour in 0_u32..24,
            day_offset in 0_i64..365,
        ) {
            let expression = CronExpression::parse(format!("{minute} {hour} * * *"));
            prop_assert!(expression.is_ok());
            let Ok(expression) = expression else {
                return Ok(());
            };
            let Some(base) = DateTime::<Utc>::from_timestamp(1_767_225_600, 0) else {
                return Ok(());
            };
            let Some(after) = base.checked_add_signed(Duration::days(day_offset)) else {
                return Ok(());
            };
            let next = expression.next_after(chrono_tz::UTC, after);
            prop_assert!(next.is_ok());
            let Ok(next) = next else {
                return Ok(());
            };
            prop_assert!(next > after);
            prop_assert_eq!(next.minute(), minute);
            prop_assert_eq!(next.hour(), hour);
            prop_assert!(next.year() >= after.year());
        }
    }
}
