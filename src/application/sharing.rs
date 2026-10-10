//! Who may read a Silicon's reminders beyond its circle, and who may share with a Silicon.
//!
//! A Silicon's reminders are always readable by its custodian and the
//! custodian's other Silicons. The Silicon (or its custodian) can also grant
//! any account view. Silicons are not open to the world: a grant to a Silicon
//! outside the owner's circle needs that Silicon (or its custodian) to have
//! allowed the owner, or the custodian making the grant, first. Carbons can
//! receive grants from anyone.

use crate::{
    domain::{Actor, ActorKind, Relation},
    error::AppError,
    infrastructure::{
        identity::{AccountRow, IdentityStore},
        sharing::{self, Allowance, ViewerGrant},
    },
};

/// Grants the caller manages and grants it received.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ViewerGrants {
    /// Grants on reminders of the caller (a Silicon) or of the Silicons it looks after.
    pub granted: Vec<ViewerGrant>,
    /// Grants that let the caller read someone else's reminders.
    pub received: Vec<ViewerGrant>,
}

/// Viewer grants and Silicon allow-lists.
#[derive(Clone, Debug)]
pub struct SharingService {
    identity: IdentityStore,
}

impl SharingService {
    /// Creates the service over the production identity store.
    #[must_use]
    pub const fn new(identity: IdentityStore) -> Self {
        Self { identity }
    }

    /// Lists the grants the caller manages and received.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub async fn grants(&self, actor: &Actor) -> Result<ViewerGrants, AppError> {
        let owners = managed_silicons(actor);
        Ok(ViewerGrants {
            granted: sharing::grants_by_owners(self.identity.pool(), &owners).await?,
            received: sharing::grants_to(self.identity.pool(), &actor.uuid).await?,
        })
    }

    /// Lets `viewer` read the reminders of `silicon_id` (or the calling Silicon).
    ///
    /// # Errors
    ///
    /// Refuses callers that do not manage the Silicon, unknown or deleted
    /// accounts, and Silicons outside the circle that have not allowed the owner.
    pub async fn grant(
        &self,
        actor: &Actor,
        viewer: &str,
        silicon_id: Option<&str>,
    ) -> Result<(ViewerGrant, bool), AppError> {
        require_access_token(actor)?;
        let owner = self.managed_owner(actor, silicon_id).await?;
        let viewer = self.existing_account(actor, viewer).await?;
        if viewer.uuid == owner || viewer.uuid == actor.uuid {
            return Err(AppError::invalid(
                "grant_to_self",
                "You already have access to these reminders; grant view to another account.",
            ));
        }
        if viewer.kind() == Some(ActorKind::Silicon) {
            let circle = self.identity.circle_of(&owner).await?;
            let allowed = circle.contains(&viewer.uuid)
                || sharing::allows_any(
                    self.identity.pool(),
                    &viewer.uuid,
                    &[owner.clone(), actor.uuid.clone()],
                )
                .await?;
            if !allowed {
                return Err(AppError::forbidden(
                    "silicon_not_open",
                    format!(
                        "{} only receives shared reminders from accounts it has allowed. Ask {} or its custodian to allow the owner first (`remind allow add <owner id>`, or POST /api/v2/allowed-accounts).",
                        display(&viewer),
                        display(&viewer)
                    ),
                ));
            }
        }
        sharing::grant(self.identity.pool(), &owner, &viewer.uuid, &actor.uuid).await
    }

    /// Ends a grant on the reminders of `silicon_id` (or the calling Silicon).
    ///
    /// # Errors
    ///
    /// Refuses callers that do not manage the Silicon; not found when no such grant exists.
    pub async fn revoke(
        &self,
        actor: &Actor,
        viewer: &str,
        silicon_id: Option<&str>,
    ) -> Result<(), AppError> {
        require_access_token(actor)?;
        let owner = self.managed_owner(actor, silicon_id).await?;
        let viewer_uuid = self.account_uuid(actor, viewer).await?;
        if sharing::revoke_grant(self.identity.pool(), &owner, &viewer_uuid, &actor.uuid).await? {
            Ok(())
        } else {
            Err(AppError::described(
                http::StatusCode::NOT_FOUND,
                "grant_not_found",
                "That account has no active grant on these reminders.",
            ))
        }
    }

    /// Lists allow-list entries of the calling Silicon or the Silicons it looks after.
    ///
    /// # Errors
    ///
    /// Refuses callers that do not manage the named Silicon; returns database errors.
    pub async fn allowances(
        &self,
        actor: &Actor,
        silicon_id: Option<&str>,
    ) -> Result<Vec<Allowance>, AppError> {
        let silicons = match silicon_id {
            Some(_) => vec![self.managed_owner(actor, silicon_id).await?],
            None => managed_silicons(actor),
        };
        sharing::allowances_of(self.identity.pool(), &silicons).await
    }

    /// Allows `account` to share reminders with `silicon_id` (or the calling Silicon).
    ///
    /// # Errors
    ///
    /// Refuses callers that do not manage the Silicon and unknown or deleted accounts.
    pub async fn allow(
        &self,
        actor: &Actor,
        account: &str,
        silicon_id: Option<&str>,
    ) -> Result<(Allowance, bool), AppError> {
        require_access_token(actor)?;
        let silicon = self.managed_owner(actor, silicon_id).await?;
        let allowed = self.existing_account(actor, account).await?;
        if allowed.uuid == silicon || allowed.uuid == actor.uuid {
            return Err(AppError::invalid(
                "allow_self",
                "A Silicon does not need to allow itself.",
            ));
        }
        sharing::allow(self.identity.pool(), &silicon, &allowed.uuid, &actor.uuid).await
    }

    /// Removes `account` from the allow-list and ends the grants it made possible.
    ///
    /// # Errors
    ///
    /// Refuses callers that do not manage the Silicon; not found when the entry does not exist.
    pub async fn disallow(
        &self,
        actor: &Actor,
        account: &str,
        silicon_id: Option<&str>,
    ) -> Result<(), AppError> {
        require_access_token(actor)?;
        let silicon = self.managed_owner(actor, silicon_id).await?;
        let allowed = self.account_uuid(actor, account).await?;
        let circle = self.identity.circle_of(&silicon).await?;
        if sharing::disallow(
            self.identity.pool(),
            &silicon,
            &allowed,
            &circle,
            &actor.uuid,
        )
        .await?
        {
            Ok(())
        } else {
            Err(AppError::described(
                http::StatusCode::NOT_FOUND,
                "allowance_not_found",
                "That account is not on this Silicon's allow-list.",
            ))
        }
    }

    /// The Silicon a sharing change is about: the caller itself when it is a
    /// Silicon, or one of the Silicons the calling Carbon looks after.
    async fn managed_owner(
        &self,
        actor: &Actor,
        silicon_id: Option<&str>,
    ) -> Result<String, AppError> {
        if actor.is_silicon() {
            return match silicon_id {
                None => Ok(actor.uuid.clone()),
                Some(id) if id == actor.public_id || id == actor.uuid => Ok(actor.uuid.clone()),
                Some(id) => Err(AppError::forbidden(
                    "not_your_silicon",
                    format!("A Silicon manages only its own sharing; {id} is another account."),
                )),
            };
        }
        let id = silicon_id.ok_or_else(|| {
            AppError::invalid(
                "silicon_id_required",
                "As a Carbon, say which of your Silicons this is about: add silicon_id, for example si:scout.",
            )
        })?;
        if let Some(owner) = actor.visible.iter().find(|owner| {
            owner.relation == Relation::Custodian
                && (owner.account.id == id || owner.account.uuid == id)
        }) {
            return Ok(owner.account.uuid.clone());
        }
        // The cached id may be stale; ask Silicon Accounts who has it now.
        self.identity
            .gateway()
            .admit_caller_lookup(&actor.uuid)
            .await
            .map_err(crate::infrastructure::identity::lookup_error)?;
        if let Some(account) = self.identity.resolve_account(id).await?
            && actor.looks_after(&account.uuid)
        {
            return Ok(account.uuid);
        }
        Err(AppError::forbidden(
            "not_custodian",
            format!("You are not the custodian of {id}, so you cannot manage its sharing."),
        ))
    }

    async fn existing_account(
        &self,
        actor: &Actor,
        id_or_uuid: &str,
    ) -> Result<AccountRow, AppError> {
        if !(crate::domain::is_valid_public_id(id_or_uuid)
            || crate::domain::is_valid_account_uuid(id_or_uuid))
        {
            return Err(AppError::invalid(
                "account_id_invalid",
                "Name the account by its id (c:handle or si:handle) or its Silicon Accounts uuid.",
            ));
        }
        self.identity
            .gateway()
            .admit_caller_lookup(&actor.uuid)
            .await
            .map_err(crate::infrastructure::identity::lookup_error)?;
        let account = self
            .identity
            .resolve_account(id_or_uuid)
            .await?
            .ok_or_else(|| {
                AppError::described(
                    http::StatusCode::NOT_FOUND,
                    "account_not_found",
                    format!("No Silicon Accounts account has the id {id_or_uuid} now (ids can change; check the current one)."),
                )
            })?;
        if account.status == "deleted" || account.kind().is_none() {
            return Err(AppError::described(
                http::StatusCode::CONFLICT,
                "account_unavailable",
                format!("{id_or_uuid} is not an active Silicon Accounts account."),
            ));
        }
        Ok(account)
    }

    async fn account_uuid(&self, actor: &Actor, id_or_uuid: &str) -> Result<String, AppError> {
        if crate::domain::is_valid_account_uuid(id_or_uuid) {
            return Ok(id_or_uuid.to_owned());
        }
        Ok(self.existing_account(actor, id_or_uuid).await?.uuid)
    }
}

/// The Silicons whose sharing the caller manages.
fn managed_silicons(actor: &Actor) -> Vec<String> {
    if actor.is_silicon() {
        return vec![actor.uuid.clone()];
    }
    actor
        .visible
        .iter()
        .filter(|owner| owner.relation == Relation::Custodian)
        .map(|owner| owner.account.uuid.clone())
        .collect()
}

fn require_access_token(actor: &Actor) -> Result<(), AppError> {
    if actor.can_write() {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "proof_cannot_write",
            "Proofs only read reminders; sharing changes need the account's own Remind access token.",
        ))
    }
}

fn display(account: &AccountRow) -> String {
    if account.public_id.is_empty() {
        account.uuid.clone()
    } else {
        account.public_id.clone()
    }
}
