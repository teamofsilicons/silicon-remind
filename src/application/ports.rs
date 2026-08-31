//! Infrastructure-independent application boundaries.

use chrono::{DateTime, Utc};

/// Source of wall-clock instants for deterministic scheduling tests.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Returns the current UTC instant.
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock backed by the operating system.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
