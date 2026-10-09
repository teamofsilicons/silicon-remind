//! The authenticated Silicon Accounts account and what it may read and change.
//!
//! Remind keys every account on its permanent Silicon Accounts `uuid`; the
//! public `c:`/`si:` id is display-only. Rows store Remind-private storage keys
//! (UUIDs) that map to accounts, so legacy rows written before the move to
//! Silicon Accounts keep their original keys and become an account's data once
//! an operator links them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Schedule;

/// Whether an account belongs to a Carbon or a Silicon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A Carbon account (`c:` ids).
    Carbon,
    /// A Silicon account (`si:` ids).
    Silicon,
}

impl ActorKind {
    /// Returns `carbon` or `silicon`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Carbon => "carbon",
            Self::Silicon => "silicon",
        }
    }

    /// Parses `carbon` or `silicon`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "carbon" => Some(Self::Carbon),
            "silicon" => Some(Self::Silicon),
            _ => None,
        }
    }
}

/// Why the caller may read another account's reminders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// The caller itself.
    #[serde(rename = "self")]
    Own,
    /// A Silicon the caller (a Carbon) is custodian of.
    Custodian,
    /// Another Silicon with the same custodian as the caller (a Silicon).
    Sibling,
    /// The owner, or the owner's custodian, granted the caller view.
    Shared,
}

/// The public identity of an account as Remind shows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountRef {
    /// Permanent Silicon Accounts identifier.
    pub uuid: String,
    /// Current public id (`c:…` or `si:…`); empty when Remind does not know it yet.
    pub id: String,
    /// Carbon or Silicon, when known.
    pub kind: Option<ActorKind>,
}

/// An account whose reminders the caller may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleOwner {
    /// The account.
    pub account: AccountRef,
    /// Display name from Silicon Accounts.
    pub display_name: String,
    /// Profile photo URL from Silicon Accounts.
    pub pfp_url: String,
    /// Why it is visible; the closest relation wins.
    pub relation: Relation,
}

/// How the request proved who is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// A Silicon Accounts access token issued to Remind.
    AccessToken,
    /// A User verification proof another app issued for this account.
    Proof {
        /// The app that issued the proof and is calling on the account's behalf.
        issuing_app: String,
        /// The proof's scopes.
        scopes: Vec<String>,
    },
}

/// The rows the caller may read in the selected environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadScope {
    /// Rows owned through these storage keys, each mapped to its owner.
    Owners(BTreeMap<Uuid, VisibleOwner>),
    /// Every row of the selected test environment: whoever holds an
    /// environment's key sees all of it, while still acting as themselves.
    Everything,
}

/// The authenticated account behind one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// Permanent Silicon Accounts identifier (the access token's `sub`).
    pub uuid: String,
    /// Carbon or Silicon.
    pub kind: ActorKind,
    /// Current public id.
    pub public_id: String,
    /// Display name.
    pub display_name: String,
    /// Profile photo URL.
    pub pfp_url: String,
    /// A Silicon's custodian, when known.
    pub custodian: Option<AccountRef>,
    /// Storage key for rows this account creates from now on.
    pub storage_key: Uuid,
    /// Every storage key of this account (its own key plus linked legacy keys), sorted.
    pub own_keys: Vec<Uuid>,
    /// Every account whose reminders the caller may read, the caller included.
    pub visible: Vec<VisibleOwner>,
    /// The rows the caller may read in the selected environment.
    pub read: ReadScope,
    /// How the request authenticated.
    pub credential: Credential,
}

impl Actor {
    /// Returns whether this actor is a Silicon.
    #[must_use]
    pub const fn is_silicon(&self) -> bool {
        matches!(self.kind, ActorKind::Silicon)
    }

    /// Returns whether the request may change data (proofs only read).
    #[must_use]
    pub const fn can_write(&self) -> bool {
        matches!(self.credential, Credential::AccessToken)
    }

    /// Returns whether a storage key belongs to this account.
    #[must_use]
    pub fn owns_key(&self, key: Uuid) -> bool {
        self.own_keys.binary_search(&key).is_ok()
    }

    /// Returns whether rows stored under `key` are readable.
    #[must_use]
    pub fn can_read_key(&self, key: Uuid) -> bool {
        match &self.read {
            ReadScope::Owners(owners) => owners.contains_key(&key),
            ReadScope::Everything => true,
        }
    }

    /// Storage keys to filter reads with, or `None` when everything is readable.
    #[must_use]
    pub fn read_keys(&self) -> Option<Vec<Uuid>> {
        match &self.read {
            ReadScope::Owners(owners) => Some(owners.keys().copied().collect()),
            ReadScope::Everything => None,
        }
    }

    /// The visible owner behind a storage key, when the caller's scope maps it.
    #[must_use]
    pub fn owner_of_key(&self, key: Uuid) -> Option<&VisibleOwner> {
        match &self.read {
            ReadScope::Owners(owners) => owners.get(&key),
            ReadScope::Everything => None,
        }
    }

    /// Returns whether the caller is the custodian of the given Silicon account.
    #[must_use]
    pub fn looks_after(&self, account_uuid: &str) -> bool {
        self.visible.iter().any(|owner| {
            owner.account.uuid == account_uuid && owner.relation == Relation::Custodian
        })
    }

    /// The visible account with this current public id, if any.
    #[must_use]
    pub fn visible_by_id(&self, public_id: &str) -> Option<&VisibleOwner> {
        self.visible
            .iter()
            .find(|owner| owner.account.id == public_id)
    }

    /// The visible account with this uuid, if any.
    #[must_use]
    pub fn visible_by_uuid(&self, uuid: &str) -> Option<&VisibleOwner> {
        self.visible.iter().find(|owner| owner.account.uuid == uuid)
    }

    /// Returns whether this actor may change the schedule: only its owner Silicon may.
    #[must_use]
    pub fn owns_schedule(&self, schedule: &Schedule) -> bool {
        self.is_silicon() && self.can_write() && self.owns_key(schedule.owner_key)
    }

    /// Returns whether the schedule is readable.
    #[must_use]
    pub fn can_read_schedule(&self, schedule: &Schedule) -> bool {
        self.can_read_key(schedule.owner_key)
    }

    /// The caller as an account reference.
    #[must_use]
    pub fn account(&self) -> AccountRef {
        AccountRef {
            uuid: self.uuid.clone(),
            id: self.public_id.clone(),
            kind: Some(self.kind),
        }
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Small builders for actors in unit and database tests.
    use std::collections::BTreeMap;

    use uuid::Uuid;

    use super::{AccountRef, Actor, ActorKind, Credential, ReadScope, Relation, VisibleOwner};

    /// A visible owner entry.
    pub(crate) fn owner(uuid: &str, id: &str, kind: ActorKind, relation: Relation) -> VisibleOwner {
        VisibleOwner {
            account: AccountRef {
                uuid: uuid.to_owned(),
                id: id.to_owned(),
                kind: Some(kind),
            },
            display_name: String::new(),
            pfp_url: String::new(),
            relation,
        }
    }

    /// An actor that reads exactly its own key plus the given `(key, owner)` pairs.
    pub(crate) fn actor(
        uuid: &str,
        id: &str,
        kind: ActorKind,
        key: Uuid,
        others: Vec<(Uuid, VisibleOwner)>,
    ) -> Actor {
        let me = owner(uuid, id, kind, Relation::Own);
        let mut owners = BTreeMap::from([(key, me.clone())]);
        let mut visible = vec![me];
        for (other_key, other) in others {
            if !visible.iter().any(|v| v.account.uuid == other.account.uuid) {
                visible.push(other.clone());
            }
            owners.insert(other_key, other);
        }
        Actor {
            uuid: uuid.to_owned(),
            kind,
            public_id: id.to_owned(),
            display_name: String::new(),
            pfp_url: String::new(),
            custodian: None,
            storage_key: key,
            own_keys: vec![key],
            visible,
            read: ReadScope::Owners(owners),
            credential: Credential::AccessToken,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::fixtures::{actor, owner};
    use super::*;
    use crate::domain::{CreateScheduleCommand, Schedule, ScheduleKind};

    fn sample_schedule(owner_key: Uuid) -> Result<Schedule, Box<dyn std::error::Error>> {
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
            owner_key,
            "si:scout",
            new_schedule,
            now,
        ))
    }

    #[test]
    fn only_the_owner_silicon_with_an_access_token_owns_a_schedule()
    -> Result<(), Box<dyn std::error::Error>> {
        let owner_key = Uuid::from_u128(11);
        let schedule = sample_schedule(owner_key)?;
        let scout = actor("aaa", "si:scout", ActorKind::Silicon, owner_key, vec![]);
        let custodian = actor(
            "ccc",
            "c:ada",
            ActorKind::Carbon,
            Uuid::from_u128(12),
            vec![(
                owner_key,
                owner("aaa", "si:scout", ActorKind::Silicon, Relation::Custodian),
            )],
        );
        let sibling = actor(
            "bbb",
            "si:atlas",
            ActorKind::Silicon,
            Uuid::from_u128(13),
            vec![(
                owner_key,
                owner("aaa", "si:scout", ActorKind::Silicon, Relation::Sibling),
            )],
        );
        let mut through_proof = scout.clone();
        through_proof.credential = Credential::Proof {
            issuing_app: "interface".to_owned(),
            scopes: vec!["remind.schedules.read".to_owned()],
        };

        assert!(scout.owns_schedule(&schedule));
        assert!(!custodian.owns_schedule(&schedule));
        assert!(!sibling.owns_schedule(&schedule));
        assert!(!through_proof.owns_schedule(&schedule));
        for reader in [&scout, &custodian, &sibling, &through_proof] {
            assert!(reader.can_read_schedule(&schedule));
        }
        assert!(custodian.looks_after("aaa"));
        assert!(!sibling.looks_after("aaa"));
        Ok(())
    }

    #[test]
    fn outsiders_read_nothing_but_test_environment_holders_read_everything()
    -> Result<(), Box<dyn std::error::Error>> {
        let schedule = sample_schedule(Uuid::from_u128(11))?;
        let mut outsider = actor(
            "zzz",
            "c:eve",
            ActorKind::Carbon,
            Uuid::from_u128(99),
            vec![],
        );
        assert!(!outsider.can_read_schedule(&schedule));
        assert_eq!(outsider.read_keys(), Some(vec![Uuid::from_u128(99)]));
        outsider.read = ReadScope::Everything;
        assert!(outsider.can_read_schedule(&schedule));
        assert_eq!(outsider.read_keys(), None);
        assert!(!outsider.owns_schedule(&schedule));
        Ok(())
    }

    #[test]
    fn relation_serializes_as_documented() -> Result<(), serde_json::Error> {
        assert_eq!(serde_json::to_string(&Relation::Own)?, r#""self""#);
        assert_eq!(
            serde_json::to_string(&Relation::Custodian)?,
            r#""custodian""#
        );
        assert_eq!(serde_json::to_string(&Relation::Sibling)?, r#""sibling""#);
        assert_eq!(serde_json::to_string(&Relation::Shared)?, r#""shared""#);
        Ok(())
    }
}
