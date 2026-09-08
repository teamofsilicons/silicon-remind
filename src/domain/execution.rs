//! Execution history records and delivery-state transitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Public delivery states for one materialized occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// The occurrence is durable and has not completed a delivery attempt.
    Pending,
    /// webhook durably accepted the event.
    Delivered,
    /// A retryable attempt failed and another attempt is scheduled.
    Retrying,
    /// Delivery reached a terminal failure.
    Failed,
}

impl ExecutionStatus {
    /// Validates a delivery-state transition.
    ///
    /// `retrying -> retrying` is allowed because each failed retry updates the
    /// same public state while recording a new attempt time and failure reason.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionStatusTransitionError`] when `self` is terminal or
    /// `target` is not a permitted delivery state.
    pub fn validate_transition(self, target: Self) -> Result<(), ExecutionStatusTransitionError> {
        let allowed = matches!(
            (self, target),
            (
                Self::Pending | Self::Retrying,
                Self::Delivered | Self::Retrying | Self::Failed
            )
        );
        if allowed {
            Ok(())
        } else {
            Err(ExecutionStatusTransitionError {
                from: self,
                to: target,
            })
        }
    }

    /// Returns whether no later state transition is allowed.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Failed)
    }
}

/// One stable, idempotent occurrence in schedule execution history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Execution {
    /// Stable UUID reused as the webhook idempotency key.
    pub id: Uuid,
    /// Schedule that materialized this occurrence.
    pub schedule_id: Uuid,
    /// Original due instant, immutable across retries.
    pub scheduled_for: DateTime<Utc>,
    /// Most recent delivery-attempt instant.
    pub attempted_at: Option<DateTime<Utc>>,
    /// Instant at which webhook durably accepted the event.
    pub delivered_at: Option<DateTime<Utc>>,
    /// Current public delivery state.
    pub status: ExecutionStatus,
    /// Stable event identifier returned by webhook after acceptance.
    pub hook_event_id: Option<Uuid>,
    /// Bounded, sanitized diagnostic for the most recent or terminal failure.
    pub failure_reason: Option<String>,
}

impl Execution {
    /// Creates a newly materialized pending occurrence.
    #[must_use]
    pub const fn new(id: Uuid, schedule_id: Uuid, scheduled_for: DateTime<Utc>) -> Self {
        Self {
            id,
            schedule_id,
            scheduled_for,
            attempted_at: None,
            delivered_at: None,
            status: ExecutionStatus::Pending,
            hook_event_id: None,
            failure_reason: None,
        }
    }

    /// Records a retryable delivery failure.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionStatusTransitionError`] if this execution is already
    /// in a terminal state.
    pub fn mark_retrying(
        &mut self,
        attempted_at: DateTime<Utc>,
        failure_reason: impl Into<String>,
    ) -> Result<(), ExecutionStatusTransitionError> {
        self.transition_to(ExecutionStatus::Retrying)?;
        self.attempted_at = Some(attempted_at);
        self.failure_reason = Some(failure_reason.into());
        Ok(())
    }

    /// Records durable webhook acceptance and its provider event identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionStatusTransitionError`] if this execution is already
    /// in a terminal state.
    pub fn mark_delivered(
        &mut self,
        delivered_at: DateTime<Utc>,
        hook_event_id: Uuid,
    ) -> Result<(), ExecutionStatusTransitionError> {
        self.transition_to(ExecutionStatus::Delivered)?;
        self.attempted_at = Some(delivered_at);
        self.delivered_at = Some(delivered_at);
        self.hook_event_id = Some(hook_event_id);
        self.failure_reason = None;
        Ok(())
    }

    /// Records a terminal delivery failure.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionStatusTransitionError`] if this execution is already
    /// in a terminal state.
    pub fn mark_failed(
        &mut self,
        attempted_at: DateTime<Utc>,
        failure_reason: impl Into<String>,
    ) -> Result<(), ExecutionStatusTransitionError> {
        self.transition_to(ExecutionStatus::Failed)?;
        self.attempted_at = Some(attempted_at);
        self.failure_reason = Some(failure_reason.into());
        Ok(())
    }

    fn transition_to(
        &mut self,
        target: ExecutionStatus,
    ) -> Result<(), ExecutionStatusTransitionError> {
        self.status.validate_transition(target)?;
        self.status = target;
        Ok(())
    }
}

/// An invalid execution delivery-state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("execution cannot transition from {from:?} to {to:?}")]
pub struct ExecutionStatusTransitionError {
    /// Current execution state.
    pub from: ExecutionStatus,
    /// Requested execution state.
    pub to: ExecutionStatus,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};
    use pretty_assertions::assert_eq;

    use super::*;

    fn scheduled_for() -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        Utc.with_ymd_and_hms(2026, 8, 31, 9, 0, 0)
            .single()
            .ok_or_else(|| "test instant must be valid".into())
    }

    #[test]
    fn new_execution_matches_the_public_pending_shape() -> Result<(), Box<dyn std::error::Error>> {
        let execution = Execution::new(Uuid::from_u128(1), Uuid::from_u128(2), scheduled_for()?);

        assert_eq!(execution.status, ExecutionStatus::Pending);
        assert_eq!(execution.attempted_at, None);
        assert_eq!(execution.delivered_at, None);
        assert_eq!(execution.hook_event_id, None);
        assert_eq!(execution.failure_reason, None);
        Ok(())
    }

    #[test]
    fn retry_failures_can_be_updated_until_terminal() -> Result<(), Box<dyn std::error::Error>> {
        let due = scheduled_for()?;
        let mut execution = Execution::new(Uuid::from_u128(1), Uuid::from_u128(2), due);
        execution.mark_retrying(due + Duration::seconds(1), "timeout")?;
        execution.mark_retrying(due + Duration::seconds(31), "server error")?;

        assert_eq!(execution.status, ExecutionStatus::Retrying);
        assert_eq!(execution.attempted_at, Some(due + Duration::seconds(31)));
        assert_eq!(execution.failure_reason.as_deref(), Some("server error"));
        Ok(())
    }

    #[test]
    fn delivery_clears_prior_failure_and_records_hook_acceptance()
    -> Result<(), Box<dyn std::error::Error>> {
        let due = scheduled_for()?;
        let delivered_at = due + Duration::seconds(31);
        let hook_event_id = Uuid::from_u128(3);
        let mut execution = Execution::new(Uuid::from_u128(1), Uuid::from_u128(2), due);
        execution.mark_retrying(due + Duration::seconds(1), "timeout")?;
        execution.mark_delivered(delivered_at, hook_event_id)?;

        assert_eq!(execution.status, ExecutionStatus::Delivered);
        assert_eq!(execution.attempted_at, Some(delivered_at));
        assert_eq!(execution.delivered_at, Some(delivered_at));
        assert_eq!(execution.hook_event_id, Some(hook_event_id));
        assert_eq!(execution.failure_reason, None);
        Ok(())
    }

    #[test]
    fn delivered_and_failed_states_are_terminal() -> Result<(), Box<dyn std::error::Error>> {
        let due = scheduled_for()?;
        let mut delivered = Execution::new(Uuid::from_u128(1), Uuid::from_u128(2), due);
        delivered.mark_delivered(due, Uuid::from_u128(3))?;
        assert_eq!(
            delivered.mark_retrying(due, "late retry"),
            Err(ExecutionStatusTransitionError {
                from: ExecutionStatus::Delivered,
                to: ExecutionStatus::Retrying,
            })
        );

        let mut failed = Execution::new(Uuid::from_u128(4), Uuid::from_u128(2), due);
        failed.mark_failed(due, "bad request")?;
        assert_eq!(
            failed.mark_delivered(due, Uuid::from_u128(5)),
            Err(ExecutionStatusTransitionError {
                from: ExecutionStatus::Failed,
                to: ExecutionStatus::Delivered,
            })
        );
        assert!(ExecutionStatus::Delivered.is_terminal());
        assert!(ExecutionStatus::Failed.is_terminal());
        assert!(!ExecutionStatus::Pending.is_terminal());
        Ok(())
    }

    #[test]
    fn transition_table_is_exhaustive() {
        let states = [
            ExecutionStatus::Pending,
            ExecutionStatus::Delivered,
            ExecutionStatus::Retrying,
            ExecutionStatus::Failed,
        ];

        for from in states {
            for to in states {
                let expected = matches!(
                    (from, to),
                    (
                        ExecutionStatus::Pending | ExecutionStatus::Retrying,
                        ExecutionStatus::Delivered
                            | ExecutionStatus::Retrying
                            | ExecutionStatus::Failed
                    )
                );
                assert_eq!(from.validate_transition(to).is_ok(), expected);
            }
        }
    }
}
