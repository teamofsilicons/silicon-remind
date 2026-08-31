//! Pure business rules for identity, scheduling, delivery state, and pagination.
//!
//! Types in this module do not perform I/O. Transport and persistence adapters
//! translate their own representations at the application boundary.

mod actor;
mod cursor;
mod execution;
mod identity;
mod schedule;

pub use actor::{Actor, ActorKind, ReminderReadScope};
pub use cursor::{CursorError, CursorKind, PageCursor};
pub use execution::{Execution, ExecutionStatus, ExecutionStatusTransitionError};
pub use identity::{
    IAM_LABEL_MAX_BYTES, IAM_LABEL_MIN_BYTES, is_valid_global_silicon_id, is_valid_iam_label,
    silicon_id_belongs_to_org,
};
pub use schedule::{
    CreateScheduleCommand, CronExpression, DEFAULT_TIMEZONE, NewSchedule, PatchScheduleCommand,
    PatchValue, Schedule, ScheduleKind, ScheduleStatus, ScheduleStatusTransitionError,
    ScheduleTiming, ScheduleValidationError, ValidatedSchedulePatch, next_recurring_occurrence,
};
