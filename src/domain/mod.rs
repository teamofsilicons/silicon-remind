//! Pure business rules for accounts, scheduling, delivery state, and pagination.
//!
//! Types in this module do not perform I/O. Transport and persistence adapters
//! translate their own representations at the application boundary.

mod actor;
mod cursor;
mod execution;
mod identity;
mod schedule;

#[cfg(test)]
pub(crate) use actor::fixtures;
pub use actor::{AccountRef, Actor, ActorKind, Credential, ReadScope, Relation, VisibleOwner};
pub use cursor::{CursorError, CursorKind, PageCursor};
pub use execution::{Execution, ExecutionStatus, ExecutionStatusTransitionError};
pub use identity::{
    HANDLE_MAX_BYTES, HANDLE_MIN_BYTES, is_valid_account_uuid, is_valid_carbon_id,
    is_valid_global_silicon_id, is_valid_handle, is_valid_public_id,
};
pub use schedule::{
    ARCHIVE_RETENTION_DAYS, CreateScheduleCommand, CronExpression, MAX_SCHEDULE_STATUS_BATCH_SIZE,
    NewSchedule, PatchScheduleCommand, PatchValue, Schedule, ScheduleKind, ScheduleSection,
    ScheduleStatus, ScheduleStatusTransitionError, ScheduleTiming, ScheduleValidationError,
    ValidatedSchedulePatch, next_recurring_occurrence,
};
