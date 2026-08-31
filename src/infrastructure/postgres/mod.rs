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
    ActorType, AuditContext, CreateSchedule, DueMaterialization, ExecutionCursor, ExecutionRow,
    HookDestinationRewrap, HookDestinationRow, IamLifecycleOutcome, IdempotencyContext,
    IdempotentMutation, InternalEventReceiptRow, ListSchedules, MutableScheduleStatus,
    NewHookDestination, NewInternalEvent, Page, RevokedResourceCleanup, ScheduleCursor,
    ScheduleReplacement, ScheduleRow, SiliconIdentityRow, StoredIdempotentResponse,
};
pub use repository::PostgresRepository;
