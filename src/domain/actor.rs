//! Authenticated actor types and organization-scoped authorization helpers.

use serde::{Deserialize, Serialize};

use super::Schedule;

/// The kind of principal represented by an IAM access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A human account.
    Carbon,
    /// An AI agent account.
    Silicon,
}

/// The strict actor model produced after IAM introspection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Actor {
    /// Whether the principal is a Carbon or Silicon.
    pub kind: ActorKind,
    /// The globally unique IAM principal identifier.
    pub id: String,
    /// The organization selected for the current request.
    pub org_id: String,
}

impl Actor {
    /// Creates an authenticated actor in a selected organization.
    #[must_use]
    pub fn new(kind: ActorKind, id: impl Into<String>, org_id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            org_id: org_id.into(),
        }
    }

    /// Returns whether this actor is an authenticated Silicon.
    #[must_use]
    pub const fn is_silicon(&self) -> bool {
        matches!(self.kind, ActorKind::Silicon)
    }

    /// Returns whether a resource belongs to the actor's selected organization.
    #[must_use]
    pub fn can_read_org(&self, resource_org_id: &str) -> bool {
        self.org_id == resource_org_id
    }

    /// Returns whether this actor owns the supplied schedule.
    ///
    /// Ownership requires a Silicon principal, the same selected organization,
    /// and the same stable IAM principal identifier.
    #[must_use]
    pub fn owns_schedule(&self, schedule: &Schedule) -> bool {
        self.is_silicon()
            && self.org_id == schedule.org_id
            && self.id == schedule.owner_principal_id
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::domain::{CreateScheduleCommand, Schedule, ScheduleKind};

    fn sample_schedule() -> Result<Schedule, Box<dyn std::error::Error>> {
        let now = Utc
            .with_ymd_and_hms(2026, 8, 31, 9, 0, 0)
            .single()
            .ok_or("test instant must be valid")?;
        let new_schedule = CreateScheduleCommand {
            text: "Prepare the report".to_owned(),
            timezone: "Asia/Kolkata".to_owned(),
            kind: ScheduleKind::OneTime,
            cron: "0 15 * * *".to_owned(),
        }
        .validate(now)?;

        Ok(Schedule::new(
            Uuid::from_u128(1),
            "org-1",
            "00000000-0000-0000-0000-000000000001",
            "silicon-1",
            new_schedule,
            now,
        ))
    }

    #[test]
    fn only_the_owner_silicon_owns_a_schedule() -> Result<(), Box<dyn std::error::Error>> {
        let schedule = sample_schedule()?;
        let owner = Actor::new(
            ActorKind::Silicon,
            "00000000-0000-0000-0000-000000000001",
            "org-1",
        );
        let carbon = Actor::new(
            ActorKind::Carbon,
            "00000000-0000-0000-0000-000000000001",
            "org-1",
        );
        let other_silicon = Actor::new(
            ActorKind::Silicon,
            "00000000-0000-0000-0000-000000000002",
            "org-1",
        );
        let cross_org = Actor::new(
            ActorKind::Silicon,
            "00000000-0000-0000-0000-000000000001",
            "org-2",
        );

        assert!(owner.owns_schedule(&schedule));
        assert!(!carbon.owns_schedule(&schedule));
        assert!(!other_silicon.owns_schedule(&schedule));
        assert!(!cross_org.owns_schedule(&schedule));
        Ok(())
    }

    #[test]
    fn both_actor_kinds_can_read_their_selected_organization() {
        let carbon = Actor::new(ActorKind::Carbon, "carbon-1", "org-1");
        let silicon = Actor::new(ActorKind::Silicon, "silicon-1", "org-1");

        assert!(carbon.can_read_org("org-1"));
        assert!(silicon.can_read_org("org-1"));
        assert!(!carbon.can_read_org("org-2"));
        assert!(!silicon.can_read_org("org-2"));
    }
}
