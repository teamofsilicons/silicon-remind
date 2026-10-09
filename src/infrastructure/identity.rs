//! Remind's local view of Silicon Accounts accounts and the custodian circle.
//!
//! Identity is global: it lives in the production database even when a request
//! selects a test environment, because Silicon Accounts has no test copies of
//! accounts. Rows store Remind-private storage keys; `account_keys` maps every
//! key to the account that owns it.
//!
//! The circle: for a Silicon it is the Silicon, its custodian and the other
//! Silicons with the same custodian; for a Carbon it is the Carbon and every
//! Silicon it is custodian of. A caller reads the reminders of the Silicons in
//! its circle and of every owner that granted it view.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, TimeZone as _, Utc};
use silicon_accounts_client::{AccountSummary, AppUser, Claims, ValidProof};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::accounts::{AccountsError, AccountsGateway};
use crate::{
    domain::{
        AccountRef, Actor, ActorKind, Credential, ReadScope, Relation, VisibleOwner,
        is_valid_account_uuid,
    },
    error::AppError,
};

/// Account and storage-key persistence plus actor resolution.
#[derive(Clone, Debug)]
pub struct IdentityStore {
    pool: PgPool,
    gateway: AccountsGateway,
    lookup_ttl: std::time::Duration,
}

/// One cached account row.
#[derive(Clone, Debug, FromRow)]
pub struct AccountRow {
    /// Silicon Accounts uuid.
    pub uuid: String,
    /// `carbon`, `silicon`, or unknown.
    pub kind: Option<String>,
    /// Current public id (empty when unknown or deleted).
    pub public_id: String,
    /// Display name.
    pub display_name: String,
    /// Profile photo URL.
    pub pfp_url: String,
    /// A Silicon's custodian uuid.
    pub custodian_uuid: Option<String>,
    /// A Silicon's custodian's public id.
    pub custodian_id: Option<String>,
    /// `active`, `access_removed` or `deleted`.
    pub status: String,
    /// Tokens issued at or before this instant are refused.
    pub revoked_before: Option<DateTime<Utc>>,
    /// Newest token issue time seen.
    pub last_token_iat: Option<DateTime<Utc>>,
    /// Last authoritative lookup.
    pub looked_up_at: Option<DateTime<Utc>>,
}

impl AccountRow {
    /// Carbon or Silicon, when known.
    #[must_use]
    pub fn kind(&self) -> Option<ActorKind> {
        self.kind.as_deref().and_then(ActorKind::parse)
    }

    /// The account as responses show it.
    #[must_use]
    pub fn reference(&self) -> AccountRef {
        AccountRef {
            uuid: self.uuid.clone(),
            id: self.public_id.clone(),
            kind: self.kind(),
        }
    }
}

/// One account as Silicon Accounts shows it to Remind right now.
#[derive(Debug, Default)]
pub struct Reread {
    /// The lookup: current id, status and a Silicon's custodian.
    pub summary: Option<AccountSummary>,
    /// The user base entry: display name and photo; `None` when the account
    /// never signed in to Remind.
    pub member: Option<AppUser>,
}

const ACCOUNT_COLUMNS: &str = "uuid, kind, public_id, display_name, pfp_url, custodian_uuid, \
     custodian_id, status, revoked_before, last_token_iat, looked_up_at";

#[derive(FromRow)]
struct VisibleRow {
    uuid: String,
    kind: String,
    public_id: String,
    display_name: String,
    pfp_url: String,
    relation: i32,
    storage_id: Option<Uuid>,
}

impl IdentityStore {
    /// Creates the store over the production pool.
    #[must_use]
    pub const fn new(
        pool: PgPool,
        gateway: AccountsGateway,
        lookup_ttl: std::time::Duration,
    ) -> Self {
        Self {
            pool,
            gateway,
            lookup_ttl,
        }
    }

    /// The production pool that holds identity.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The Silicon Accounts gateway.
    #[must_use]
    pub const fn gateway(&self) -> &AccountsGateway {
        &self.gateway
    }

    /// Resolves the account behind a verified access token.
    ///
    /// # Errors
    ///
    /// Refuses tokens issued before the account's sign-in at Remind ended, and
    /// tokens of deleted accounts; returns database errors.
    pub async fn resolve_bearer(&self, claims: &Claims) -> Result<Actor, AppError> {
        let kind = claims
            .kind
            .map(|kind| match kind {
                silicon_accounts_client::AccountKind::Carbon => ActorKind::Carbon,
                silicon_accounts_client::AccountKind::Silicon => ActorKind::Silicon,
            })
            .ok_or_else(|| {
                AppError::unauthenticated(
                    "token_missing_claim",
                    "The access token has no `kind` claim, so Remind cannot tell a Carbon from a Silicon. Sign in again.",
                )
            })?;
        if !is_valid_account_uuid(&claims.sub) {
            return Err(AppError::unauthenticated(
                "token_missing_claim",
                "The access token's `sub` is not a Silicon Accounts uuid.",
            ));
        }
        let issued_at = claims
            .iat
            .and_then(|iat| Utc.timestamp_opt(iat, 0).single())
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        let token_id = claims.id.clone().unwrap_or_default();
        let row = self
            .observe_token(&claims.sub, kind, &token_id, issued_at)
            .await?;
        let row = self.refresh_if_stale(row).await?;
        self.actor_for(
            &row,
            Credential::AccessToken {
                scopes: claims.scopes().into_iter().map(str::to_owned).collect(),
                family: claims.fid.clone(),
            },
        )
        .await
    }

    /// Resolves the account a valid User verification proof speaks for.
    ///
    /// # Errors
    ///
    /// Refuses deleted accounts; returns database errors.
    pub async fn resolve_proof(
        &self,
        proof: &ValidProof,
        scopes: Vec<String>,
    ) -> Result<Actor, AppError> {
        let user = proof.user.as_ref().ok_or_else(|| {
            AppError::forbidden(
                "proof_kind_not_accepted",
                "Remind accepts only User verification proofs, which name the account they act for.",
            )
        })?;
        if !is_valid_account_uuid(&user.uuid) {
            return Err(AppError::unauthenticated(
                "proof_invalid",
                "The proof names an account uuid Remind cannot read.",
            ));
        }
        let kind = user.kind.map(|kind| match kind {
            silicon_accounts_client::AccountKind::Carbon => "carbon",
            silicon_accounts_client::AccountKind::Silicon => "silicon",
        });
        sqlx::query(
            "INSERT INTO accounts (uuid, kind, public_id) VALUES ($1, $2, $3) \
             ON CONFLICT (uuid) DO UPDATE SET kind = COALESCE(accounts.kind, EXCLUDED.kind)",
        )
        .bind(&user.uuid)
        .bind(kind)
        .bind(public_id_or_empty(&user.id))
        .execute(&self.pool)
        .await?;
        self.ensure_primary_key(&user.uuid).await?;
        let row = self.require_row(&user.uuid).await?;
        if row.status == "deleted" {
            return Err(account_deleted());
        }
        let row = self.refresh_if_stale(row).await?;
        self.actor_for(
            &row,
            Credential::Proof {
                issuing_app: proof.issuing_app.app_id.clone(),
                scopes,
            },
        )
        .await
    }

    async fn observe_token(
        &self,
        uuid: &str,
        kind: ActorKind,
        token_id: &str,
        issued_at: DateTime<Utc>,
    ) -> Result<AccountRow, AppError> {
        if let Some(row) = self.find(uuid).await?
            && token_is_current(&row, issued_at)?
            && row.kind() == Some(kind)
            && row.status == "active"
            && row.last_token_iat.is_some_and(|seen| seen >= issued_at)
            && self.primary_key(uuid).await?.is_some()
        {
            return Ok(row);
        }
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO accounts (uuid, kind, public_id, last_token_iat) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (uuid) DO NOTHING",
        )
        .bind(uuid)
        .bind(kind.as_str())
        .bind(public_id_or_empty(token_id))
        .bind(issued_at)
        .execute(&mut *transaction)
        .await?;
        let row = sqlx::query_as::<_, AccountRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE uuid = $1 FOR UPDATE"
        )))
        .bind(uuid)
        .fetch_one(&mut *transaction)
        .await?;
        token_is_current(&row, issued_at)?;
        if row.kind().is_some_and(|stored| stored != kind) {
            return Err(AppError::unauthenticated(
                "token_kind_mismatch",
                "The access token's kind does not match the account Remind knows under this uuid. Sign in again.",
            ));
        }
        // A token newer than the cutoff is a new sign-in: it ends a suspension.
        sqlx::query(
            "UPDATE accounts SET \
                 kind = COALESCE(kind, $2), \
                 status = CASE WHEN status = 'access_removed' THEN 'active' ELSE status END, \
                 status_changed_at = CASE WHEN status = 'access_removed' \
                     THEN clock_timestamp() ELSE status_changed_at END, \
                 last_token_iat = GREATEST(COALESCE(last_token_iat, $3), $3), \
                 updated_at = clock_timestamp() \
             WHERE uuid = $1",
        )
        .bind(uuid)
        .bind(kind.as_str())
        .bind(issued_at)
        .execute(&mut *transaction)
        .await?;
        insert_primary_key(&mut transaction, uuid).await?;
        transaction.commit().await?;
        self.require_row(uuid).await
    }

    /// Refreshes the cached account when it was never read or is older than the
    /// lookup TTL. A failed read keeps the cached copy.
    async fn refresh_if_stale(&self, row: AccountRow) -> Result<AccountRow, AppError> {
        let ttl = chrono::Duration::from_std(self.lookup_ttl)
            .map_err(|error| AppError::internal("lookup_ttl", error))?;
        if row
            .looked_up_at
            .is_some_and(|looked_up_at| Utc::now() - looked_up_at < ttl)
        {
            return Ok(row);
        }
        let reread = self.reread(&row.uuid).await;
        if reread.summary.is_none() && reread.member.is_none() {
            return Ok(row);
        }
        self.store_reread(&reread).await?;
        self.require_row(&row.uuid).await
    }

    /// Reads an account from Silicon Accounts now, in parallel: the lookup gives
    /// its current id, status and (for a Silicon) custodian; the user base read
    /// gives what it shares with Remind by signing in to it (display name and
    /// photo), which a lookup never shows. Failures are logged and leave that
    /// half empty.
    pub async fn reread(&self, uuid: &str) -> Reread {
        let (summary, member) = tokio::join!(self.gateway.lookup(uuid), self.gateway.member(uuid));
        let summary = summary.unwrap_or_else(|error| {
            tracing::warn!(error = %error, "Silicon Accounts lookup failed; keeping the cached account");
            None
        });
        let member = member.unwrap_or_else(|error| {
            tracing::warn!(error = %error, "Silicon Accounts user base read failed; keeping the cached profile");
            None
        });
        Reread { summary, member }
    }

    /// Writes what [`Self::reread`] found.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub async fn store_reread(&self, reread: &Reread) -> Result<(), AppError> {
        if let Some(summary) = &reread.summary {
            apply_summary(&self.pool, summary).await?;
        }
        if let Some(member) = &reread.member {
            apply_member(&self.pool, member).await?;
        }
        Ok(())
    }

    /// Writes the authoritative state Silicon Accounts returned for an account.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub async fn apply_summary(&self, summary: &AccountSummary) -> Result<(), AppError> {
        apply_summary(&self.pool, summary).await
    }

    /// Resolves a `c:`/`si:` id or a uuid to the account Silicon Accounts knows now,
    /// and caches it. `None` when no such account exists.
    ///
    /// # Errors
    ///
    /// Returns an error when Silicon Accounts cannot answer.
    pub async fn resolve_account(&self, id_or_uuid: &str) -> Result<Option<AccountRow>, AppError> {
        let found = if id_or_uuid.contains(':') {
            self.gateway.lookup_by_id(id_or_uuid).await
        } else {
            self.gateway.lookup(id_or_uuid).await
        }
        .map_err(lookup_error)?;
        let Some(summary) = found else {
            return Ok(None);
        };
        self.apply_summary(&summary).await?;
        self.find(&summary.uuid).await
    }

    /// Builds the actor and its read scope from the cached circle and grants.
    async fn actor_for(&self, row: &AccountRow, credential: Credential) -> Result<Actor, AppError> {
        let kind = row.kind().ok_or_else(|| {
            AppError::internal("account_kind", anyhow::anyhow!("account kind is unknown"))
        })?;
        let custodian = (kind == ActorKind::Silicon)
            .then(|| row.custodian_uuid.clone())
            .flatten();
        let rows = sqlx::query_as::<_, VisibleRow>(
            "WITH visible AS (\
                 SELECT a.uuid, a.kind, a.public_id, a.display_name, a.pfp_url, \
                        CASE WHEN a.uuid = $1 THEN 0 \
                             WHEN a.custodian_uuid = $1 THEN 1 \
                             WHEN $2::text IS NOT NULL AND a.custodian_uuid = $2 THEN 2 \
                             ELSE 3 END AS relation \
                 FROM accounts a \
                 WHERE a.status = 'active' AND a.kind IS NOT NULL AND (\
                       a.uuid = $1 \
                    OR a.custodian_uuid = $1 \
                    OR ($2::text IS NOT NULL AND a.custodian_uuid = $2) \
                    OR EXISTS (SELECT 1 FROM reminder_viewers v \
                               WHERE v.owner_uuid = a.uuid AND v.viewer_uuid = $1 \
                                 AND v.revoked_at IS NULL))\
             ) \
             SELECT v.uuid, v.kind, v.public_id, v.display_name, v.pfp_url, v.relation, \
                    k.storage_id \
             FROM visible v LEFT JOIN account_keys k ON k.account_uuid = v.uuid \
             ORDER BY v.relation, v.uuid, k.storage_id",
        )
        .bind(&row.uuid)
        .bind(custodian.as_deref())
        .fetch_all(&self.pool)
        .await?;

        let mut visible: Vec<VisibleOwner> = Vec::new();
        let mut owners = BTreeMap::new();
        let mut own_keys = Vec::new();
        for visible_row in rows {
            let owner = VisibleOwner {
                account: AccountRef {
                    uuid: visible_row.uuid.clone(),
                    id: visible_row.public_id,
                    kind: ActorKind::parse(&visible_row.kind),
                },
                display_name: visible_row.display_name,
                pfp_url: visible_row.pfp_url,
                relation: relation(visible_row.relation),
            };
            if let Some(key) = visible_row.storage_id {
                if visible_row.uuid == row.uuid {
                    own_keys.push(key);
                }
                owners.insert(key, owner.clone());
            }
            if !visible.iter().any(|v| v.account.uuid == owner.account.uuid) {
                visible.push(owner);
            }
        }
        // The caller's own keys count even while its account is suspended.
        for key in self.keys_of(&row.uuid).await? {
            if !own_keys.contains(&key) {
                own_keys.push(key);
            }
        }
        own_keys.sort_unstable();
        let storage_key = self.primary_key(&row.uuid).await?.ok_or_else(|| {
            AppError::internal(
                "account_key",
                anyhow::anyhow!("primary storage key missing"),
            )
        })?;
        Ok(Actor {
            uuid: row.uuid.clone(),
            kind,
            public_id: row.public_id.clone(),
            display_name: row.display_name.clone(),
            pfp_url: row.pfp_url.clone(),
            custodian: custodian.map(|uuid| AccountRef {
                uuid,
                id: row.custodian_id.clone().unwrap_or_default(),
                kind: Some(ActorKind::Carbon),
            }),
            storage_key,
            own_keys,
            visible,
            read: ReadScope::Owners(owners),
            credential,
        })
    }

    /// Finds a cached account.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub(crate) async fn find(&self, uuid: &str) -> Result<Option<AccountRow>, AppError> {
        Ok(sqlx::query_as::<_, AccountRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE uuid = $1"
        )))
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn require_row(&self, uuid: &str) -> Result<AccountRow, AppError> {
        self.find(uuid).await?.ok_or_else(|| {
            AppError::internal("account_row", anyhow::anyhow!("account row disappeared"))
        })
    }

    async fn primary_key(&self, uuid: &str) -> Result<Option<Uuid>, AppError> {
        Ok(sqlx::query_scalar(
            "SELECT storage_id FROM account_keys WHERE account_uuid = $1 AND origin = 'accounts'",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn ensure_primary_key(&self, uuid: &str) -> Result<(), AppError> {
        let mut transaction = self.pool.begin().await?;
        insert_primary_key(&mut transaction, uuid).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Every storage key of an account.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub async fn keys_of(&self, uuid: &str) -> Result<Vec<Uuid>, AppError> {
        Ok(sqlx::query_scalar(
            "SELECT storage_id FROM account_keys WHERE account_uuid = $1 ORDER BY storage_id",
        )
        .bind(uuid)
        .fetch_all(&self.pool)
        .await?)
    }

    /// The accounts behind storage keys (unknown keys are absent from the map).
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub async fn owners_by_keys(
        &self,
        keys: &[Uuid],
    ) -> Result<HashMap<Uuid, AccountRef>, AppError> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
            "SELECT k.storage_id, a.uuid, a.public_id, a.kind \
             FROM account_keys k JOIN accounts a ON a.uuid = k.account_uuid \
             WHERE k.storage_id = ANY($1)",
        )
        .bind(keys)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(key, uuid, id, kind)| {
                (
                    key,
                    AccountRef {
                        uuid,
                        id,
                        kind: kind.as_deref().and_then(ActorKind::parse),
                    },
                )
            })
            .collect())
    }

    /// The circle of an account: for a Silicon itself, its custodian and the
    /// custodian's other Silicons; for a Carbon itself and its Silicons.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub async fn circle_of(&self, uuid: &str) -> Result<Vec<String>, AppError> {
        Ok(sqlx::query_scalar(
            "SELECT a.uuid FROM accounts a, \
                 (SELECT kind, custodian_uuid FROM accounts WHERE uuid = $1) me \
             WHERE a.uuid = $1 \
                OR (me.kind = 'carbon' AND a.custodian_uuid = $1) \
                OR (me.kind = 'silicon' AND me.custodian_uuid IS NOT NULL \
                    AND (a.uuid = me.custodian_uuid OR a.custodian_uuid = me.custodian_uuid)) \
             ORDER BY a.uuid",
        )
        .bind(uuid)
        .fetch_all(&self.pool)
        .await?)
    }
}

/// Refuses tokens issued at or before the account's revocation cutoff, and
/// every token of a deleted account. JWT `iat` has one-second precision, so a
/// token from the same second as the cutoff is refused too.
fn token_is_current(row: &AccountRow, issued_at: DateTime<Utc>) -> Result<bool, AppError> {
    if row.status == "deleted" {
        return Err(account_deleted());
    }
    if row
        .revoked_before
        .is_some_and(|cutoff| issued_at.timestamp() <= cutoff.timestamp())
    {
        return Err(AppError::unauthenticated(
            "token_revoked",
            "This access token was issued before the account's sign-in at Remind ended (a sign-out or removed access at Silicon Accounts). Sign in again.",
        ));
    }
    Ok(true)
}

fn account_deleted() -> AppError {
    AppError::unauthenticated(
        "account_deleted",
        "This Silicon Accounts account was deleted, so Remind no longer accepts its tokens or proofs.",
    )
}

pub(crate) fn lookup_error(error: AccountsError) -> AppError {
    match error {
        AccountsError::RateLimited => AppError::RateLimited {
            retry_after_seconds: 60,
        },
        AccountsError::Rejected { code, message } => AppError::invalid(code, message),
        AccountsError::Unavailable(_) => AppError::DependencyUnavailable {
            dependency: "accounts",
        },
    }
}

const fn relation(code: i32) -> Relation {
    match code {
        0 => Relation::Own,
        1 => Relation::Custodian,
        2 => Relation::Sibling,
        _ => Relation::Shared,
    }
}

fn public_id_or_empty(id: &str) -> &str {
    if crate::domain::is_valid_public_id(id) {
        id
    } else {
        ""
    }
}

async fn insert_primary_key(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    uuid: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO account_keys (storage_id, account_uuid, origin) \
         SELECT $1, $2, 'accounts' \
         WHERE NOT EXISTS (SELECT 1 FROM account_keys \
                           WHERE account_uuid = $2 AND origin = 'accounts') \
         ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(uuid)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Upserts an account from a Silicon Accounts lookup (authoritative and current).
pub(crate) async fn apply_summary(pool: &PgPool, summary: &AccountSummary) -> Result<(), AppError> {
    if !is_valid_account_uuid(&summary.uuid) {
        return Err(AppError::internal(
            "account_summary",
            anyhow::anyhow!("Silicon Accounts returned an unreadable uuid"),
        ));
    }
    let kind = match summary.kind {
        silicon_accounts_client::AccountKind::Carbon => "carbon",
        silicon_accounts_client::AccountKind::Silicon => "silicon",
    };
    let custodian = summary
        .custodian
        .as_ref()
        .filter(|_| kind == "silicon")
        .filter(|custodian| is_valid_account_uuid(&custodian.uuid));
    let deleted = summary.status == "deleted";
    // A lookup never carries the display name or photo: those come from the
    // user base (`apply_member`) and from `account.updated`.
    sqlx::query(
        "INSERT INTO accounts (uuid, kind, public_id, custodian_uuid, custodian_id, status, \
             looked_up_at, id_observed_at, custodian_observed_at) \
         VALUES ($1, $2, $3, $4, $5, CASE WHEN $6 THEN 'deleted' ELSE 'active' END, \
             clock_timestamp(), clock_timestamp(), clock_timestamp()) \
         ON CONFLICT (uuid) DO UPDATE SET \
             kind = EXCLUDED.kind, public_id = EXCLUDED.public_id, \
             custodian_uuid = EXCLUDED.custodian_uuid, custodian_id = EXCLUDED.custodian_id, \
             status = CASE WHEN $6 THEN 'deleted' ELSE accounts.status END, \
             looked_up_at = clock_timestamp(), id_observed_at = clock_timestamp(), \
             custodian_observed_at = clock_timestamp(), updated_at = clock_timestamp()",
    )
    .bind(&summary.uuid)
    .bind(kind)
    .bind(public_id_or_empty(&summary.id))
    .bind(custodian.map(|custodian| custodian.uuid.as_str()))
    .bind(custodian.map(|custodian| public_id_or_empty(&custodian.id)))
    .bind(deleted)
    .execute(pool)
    .await?;
    Ok(())
}

/// Stores what an account shares with Remind, from Remind's user base: its
/// display name and photo. A deleted account stays anonymised.
pub(crate) async fn apply_member(pool: &PgPool, member: &AppUser) -> Result<(), AppError> {
    if !is_valid_account_uuid(&member.uuid) {
        return Err(AppError::internal(
            "account_member",
            anyhow::anyhow!("Silicon Accounts returned an unreadable uuid"),
        ));
    }
    sqlx::query(
        "UPDATE accounts SET display_name = $2, pfp_url = $3, updated_at = clock_timestamp() \
         WHERE uuid = $1 AND status <> 'deleted' \
           AND (display_name IS DISTINCT FROM $2 OR pfp_url IS DISTINCT FROM $3)",
    )
    .bind(&member.uuid)
    .bind(truncate(&member.display_name, 1000))
    .bind(truncate(
        member.pfp_url.as_deref().unwrap_or_default(),
        4096,
    ))
    .execute(pool)
    .await?;
    Ok(())
}

fn truncate(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
