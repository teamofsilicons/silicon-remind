//! PostgreSQL connection management and durable repositories.
//!
//! Queries in this module use `SQLx`'s runtime-checked API. Building the crate
//! never requires a live database or a compile-time `DATABASE_URL`.

mod connection;
mod error;
mod models;
mod repository;
#[cfg(test)]
mod tests;

pub use connection::{HealthCheckError, connect, health_check, migrate};
pub use error::RepositoryError;
pub use models::{
    ActorType, AuditContext, BulkScheduleStatusReplacement, CreateSchedule, DueMaterialization,
    ExecutionCursor, ExecutionRow, HookDestinationRewrap, HookDestinationRow, IdempotencyContext,
    IdempotentMutation, InternalEventReceiptRow, ListSchedules, MutableScheduleStatus,
    NewHookDestination, NewInternalEvent, Page, RevokedResourceCleanup, ScheduleCursor,
    SchedulePurgeResult, ScheduleReplacement, ScheduleResponse, ScheduleRow, ScheduleStatusChange,
    StoredIdempotentResponse,
};
pub use repository::PostgresRepository;
pub(crate) use repository::{
    OwnerCleanup, append_audit, cleanup_owner_data, insert_internal_event_receipt,
    mark_internal_event_processed, validate_internal_event,
};
