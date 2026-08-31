//! Authenticated actor types and organization-scoped authorization helpers.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// An IAM-authorized reminder read projection for the selected organization.
///
/// `None` represents an organization-wide Silicon scope. `Some` represents the
/// exact, possibly empty, set of Silicon principals visible to a Carbon. The
/// field is private so every Carbon scope remains sorted and deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReminderReadScope {
    silicon_principal_ids: Option<Box<[Uuid]>>,
}

impl ReminderReadScope {
    /// Creates an organization-wide scope for an authenticated Silicon.
    #[must_use]
    pub const fn organization() -> Self {
        Self {
            silicon_principal_ids: None,
        }
    }

    /// Creates an exact Carbon scope, sorting and deduplicating its principals.
    #[must_use]
    pub fn silicon_principals(mut principal_ids: Vec<Uuid>) -> Self {
        principal_ids.sort_unstable();
        principal_ids.dedup();
        Self {
            silicon_principal_ids: Some(principal_ids.into_boxed_slice()),
        }
    }

    /// Returns `None` for organization-wide access or the exact Carbon scope.
    #[must_use]
    pub fn silicon_principal_ids(&self) -> Option<&[Uuid]> {
        self.silicon_principal_ids.as_deref()
    }

    /// Returns whether this projection permits reading one Silicon principal.
    #[must_use]
    pub fn permits(&self, owner_principal_id: Uuid) -> bool {
        self.silicon_principal_ids
            .as_deref()
            .is_none_or(|principal_ids| principal_ids.binary_search(&owner_principal_id).is_ok())
    }
}

/// The strict actor model produced after IAM introspection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Actor {
    /// Whether the principal is a Carbon or Silicon.
    pub kind: ActorKind,
    /// The globally unique IAM principal identifier.
    pub id: String,
    /// The organization selected for the current request.
    pub org_id: String,
    /// Stable IAM membership UUID for the selected organization.
    pub membership_id: Uuid,
    /// IAM authorization revision used to build this request's projection.
    pub authorization_epoch: u64,
    /// Exact reminder visibility granted by IAM for this request.
    pub read_scope: ReminderReadScope,
}

impl Actor {
    /// Creates an authenticated Silicon with organization-wide read access.
    #[must_use]
    pub fn silicon(
        id: impl Into<String>,
        org_id: impl Into<String>,
        membership_id: Uuid,
        authorization_epoch: u64,
    ) -> Self {
        Self {
            kind: ActorKind::Silicon,
            id: id.into(),
            org_id: org_id.into(),
            membership_id,
            authorization_epoch,
            read_scope: ReminderReadScope::organization(),
        }
    }

    /// Creates an authenticated Carbon with an exact IAM read projection.
    #[must_use]
    pub fn carbon(
        id: impl Into<String>,
        org_id: impl Into<String>,
        membership_id: Uuid,
        authorization_epoch: u64,
        permitted_silicon_principal_ids: Vec<Uuid>,
    ) -> Self {
        Self {
            kind: ActorKind::Carbon,
            id: id.into(),
            org_id: org_id.into(),
            membership_id,
            authorization_epoch,
            read_scope: ReminderReadScope::silicon_principals(permitted_silicon_principal_ids),
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

    /// Returns whether the actor may read a schedule in its selected org.
    #[must_use]
    pub fn can_read_schedule(&self, schedule: &Schedule) -> bool {
        self.can_read_org(&schedule.org_id)
            && Uuid::parse_str(&schedule.owner_principal_id)
                .is_ok_and(|owner_principal_id| self.read_scope.permits(owner_principal_id))
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
        let owner = Actor::silicon(
            "00000000-0000-0000-0000-000000000001",
            "org-1",
            Uuid::from_u128(101),
            7,
        );
        let carbon = Actor::carbon(
            "00000000-0000-0000-0000-000000000001",
            "org-1",
            Uuid::from_u128(102),
            7,
            vec![Uuid::from_u128(1)],
        );
        let other_silicon = Actor::silicon(
            "00000000-0000-0000-0000-000000000002",
            "org-1",
            Uuid::from_u128(103),
            7,
        );
        let cross_org = Actor::silicon(
            "00000000-0000-0000-0000-000000000001",
            "org-2",
            Uuid::from_u128(104),
            7,
        );

        assert!(owner.owns_schedule(&schedule));
        assert!(!carbon.owns_schedule(&schedule));
        assert!(!other_silicon.owns_schedule(&schedule));
        assert!(!cross_org.owns_schedule(&schedule));
        Ok(())
    }

    #[test]
    fn both_actor_kinds_can_read_their_selected_organization() {
        let carbon = Actor::carbon("carbon-1", "org-1", Uuid::from_u128(1), 1, Vec::new());
        let silicon = Actor::silicon("silicon-1", "org-1", Uuid::from_u128(2), 1);

        assert!(carbon.can_read_org("org-1"));
        assert!(silicon.can_read_org("org-1"));
        assert!(!carbon.can_read_org("org-2"));
        assert!(!silicon.can_read_org("org-2"));
    }

    #[test]
    fn carbon_scope_is_sorted_deduplicated_and_can_be_empty() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let scope = ReminderReadScope::silicon_principals(vec![second, first, second]);

        assert_eq!(
            scope.silicon_principal_ids(),
            Some([first, second].as_slice())
        );
        assert!(scope.permits(first));
        assert!(!scope.permits(Uuid::from_u128(3)));
        assert!(
            ReminderReadScope::silicon_principals(Vec::new())
                .silicon_principal_ids()
                .is_some_and(<[Uuid]>::is_empty)
        );
    }

    #[test]
    fn silicon_scope_permits_every_principal() {
        assert!(ReminderReadScope::organization().permits(Uuid::now_v7()));
    }
}
