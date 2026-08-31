use thiserror::Error;

/// Failures surfaced by the PostgreSQL repository boundary.
#[derive(Debug, Error)]
pub enum RepositoryError {
    /// PostgreSQL or the connection pool rejected an operation.
    #[error("PostgreSQL operation failed")]
    Database(#[from] sqlx::Error),
    /// A replay key was reused for different canonical input.
    #[error("the idempotency key was already used for different input")]
    IdempotencyConflict,
    /// A committed reservation was found without its response.
    #[error("the idempotency reservation is incomplete")]
    IdempotencyIncomplete,
    /// An internal event ID was reused with a different payload.
    #[error("the internal event ID was already used for a different payload")]
    EventReceiptConflict,
    /// The IAM principal has no active, provisioned Silicon binding.
    #[error("the Silicon identity is unavailable for schedule mutation")]
    SiliconUnavailable,
    /// The tenant-scoped resource was absent or invisible to the caller.
    #[error("the requested resource was not found")]
    NotFound,
    /// An optimistic schedule version did not match the stored version.
    #[error("the schedule changed before the operation could be applied")]
    VersionConflict,
    /// A worker attempted to finish a lease which it no longer owns.
    #[error("the delivery lease is no longer owned by this worker")]
    LeaseLost,
    /// The requested transition is not valid for the stored lifecycle state.
    #[error("the requested lifecycle transition is not valid")]
    InvalidState,
    /// A repository input violated an infrastructure-level invariant.
    #[error("invalid repository input: {0}")]
    InvalidInput(&'static str),
    /// A stored idempotent response could not be represented as JSON.
    #[error("could not serialize an idempotent response")]
    Serialization(#[from] serde_json::Error),
}
