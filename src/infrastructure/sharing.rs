//! Viewer grants (an owner lets another account read its reminders) and the
//! allow-list that keeps Silicons from receiving shares they did not ask for.
//! Policy lives in `application::sharing`; this module only persists.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{
    domain::{AccountRef, ActorKind},
    error::AppError,
};

/// One active viewer grant.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ViewerGrant {
    /// Grant identifier.
    pub id: Uuid,
    /// The Silicon whose reminders are shared.
    pub owner: AccountRef,
    /// The account that may read them.
    pub viewer: AccountRef,
    /// The account that made the grant (the owner or its custodian).
    pub granted_by: String,
    /// When the grant was made.
    pub created_at: DateTime<Utc>,
}

/// One active allow-list entry.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Allowance {
    /// Allow-list entry identifier.
    pub id: Uuid,
    /// The Silicon whose allow-list this is.
    pub silicon: AccountRef,
    /// The account allowed to share reminders with it.
    pub allowed: AccountRef,
    /// The account that added the entry (the Silicon or its custodian).
    pub created_by: String,
    /// When it was added.
    pub created_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct PairRow {
    id: Uuid,
    left_uuid: String,
    left_id: String,
    left_kind: Option<String>,
    right_uuid: String,
    right_id: String,
    right_kind: Option<String>,
    by_uuid: String,
    created_at: DateTime<Utc>,
}

impl PairRow {
    fn refs(&self) -> (AccountRef, AccountRef) {
        (
            AccountRef {
                uuid: self.left_uuid.clone(),
                id: self.left_id.clone(),
                kind: self.left_kind.as_deref().and_then(ActorKind::parse),
            },
            AccountRef {
                uuid: self.right_uuid.clone(),
                id: self.right_id.clone(),
                kind: self.right_kind.as_deref().and_then(ActorKind::parse),
            },
        )
    }

    fn grant(self) -> ViewerGrant {
        let (owner, viewer) = self.refs();
        ViewerGrant {
            id: self.id,
            owner,
            viewer,
            granted_by: self.by_uuid,
            created_at: self.created_at,
        }
    }

    fn allowance(self) -> Allowance {
        let (silicon, allowed) = self.refs();
        Allowance {
            id: self.id,
            silicon,
            allowed,
            created_by: self.by_uuid,
            created_at: self.created_at,
        }
    }
}

const GRANT_SELECT: &str = "SELECT g.id, g.owner_uuid AS left_uuid, o.public_id AS left_id, \
     o.kind AS left_kind, g.viewer_uuid AS right_uuid, v.public_id AS right_id, \
     v.kind AS right_kind, g.granted_by_uuid AS by_uuid, g.created_at \
     FROM reminder_viewers g \
     JOIN accounts o ON o.uuid = g.owner_uuid JOIN accounts v ON v.uuid = g.viewer_uuid \
     WHERE g.revoked_at IS NULL";

const ALLOWANCE_SELECT: &str = "SELECT g.id, g.silicon_uuid AS left_uuid, \
     s.public_id AS left_id, s.kind AS left_kind, g.allowed_uuid AS right_uuid, \
     a.public_id AS right_id, a.kind AS right_kind, g.created_by_uuid AS by_uuid, g.created_at \
     FROM silicon_allowances g \
     JOIN accounts s ON s.uuid = g.silicon_uuid JOIN accounts a ON a.uuid = g.allowed_uuid \
     WHERE g.revoked_at IS NULL";

/// Active grants whose owner is one of `owners`.
///
/// # Errors
///
/// Returns database errors.
pub async fn grants_by_owners(
    pool: &PgPool,
    owners: &[String],
) -> Result<Vec<ViewerGrant>, AppError> {
    let rows = sqlx::query_as::<_, PairRow>(sqlx::AssertSqlSafe(format!(
        "{GRANT_SELECT} AND g.owner_uuid = ANY($1) ORDER BY g.owner_uuid, g.created_at, g.id"
    )))
    .bind(owners)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(PairRow::grant).collect())
}

/// Active grants made to `viewer`.
///
/// # Errors
///
/// Returns database errors.
pub async fn grants_to(pool: &PgPool, viewer: &str) -> Result<Vec<ViewerGrant>, AppError> {
    let rows = sqlx::query_as::<_, PairRow>(sqlx::AssertSqlSafe(format!(
        "{GRANT_SELECT} AND g.viewer_uuid = $1 ORDER BY g.created_at, g.id"
    )))
    .bind(viewer)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(PairRow::grant).collect())
}

/// Grants `viewer` view of `owner`'s reminders. Returns the grant and whether
/// it is new (an active grant is returned unchanged).
///
/// # Errors
///
/// Returns database errors.
pub async fn grant(
    pool: &PgPool,
    owner: &str,
    viewer: &str,
    granted_by: &str,
) -> Result<(ViewerGrant, bool), AppError> {
    let inserted = sqlx::query(
        "INSERT INTO reminder_viewers (id, owner_uuid, viewer_uuid, granted_by_uuid) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(owner)
    .bind(viewer)
    .bind(granted_by)
    .execute(pool)
    .await?
    .rows_affected()
        == 1;
    let row = sqlx::query_as::<_, PairRow>(sqlx::AssertSqlSafe(format!(
        "{GRANT_SELECT} AND g.owner_uuid = $1 AND g.viewer_uuid = $2"
    )))
    .bind(owner)
    .bind(viewer)
    .fetch_one(pool)
    .await?;
    Ok((row.grant(), inserted))
}

/// Ends an active grant. Returns whether one existed.
///
/// # Errors
///
/// Returns database errors.
pub async fn revoke_grant(
    pool: &PgPool,
    owner: &str,
    viewer: &str,
    revoked_by: &str,
) -> Result<bool, AppError> {
    Ok(sqlx::query(
        "UPDATE reminder_viewers SET revoked_at = clock_timestamp(), revoked_by_uuid = $3 \
         WHERE owner_uuid = $1 AND viewer_uuid = $2 AND revoked_at IS NULL",
    )
    .bind(owner)
    .bind(viewer)
    .bind(revoked_by)
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

/// Active allow-list entries of the given Silicons.
///
/// # Errors
///
/// Returns database errors.
pub async fn allowances_of(pool: &PgPool, silicons: &[String]) -> Result<Vec<Allowance>, AppError> {
    let rows = sqlx::query_as::<_, PairRow>(sqlx::AssertSqlSafe(format!(
        "{ALLOWANCE_SELECT} AND g.silicon_uuid = ANY($1) ORDER BY g.silicon_uuid, g.created_at, g.id"
    )))
    .bind(silicons)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(PairRow::allowance).collect())
}

/// Returns whether `silicon` allows any of `candidates` to share with it.
///
/// # Errors
///
/// Returns database errors.
pub async fn allows_any(
    pool: &PgPool,
    silicon: &str,
    candidates: &[String],
) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM silicon_allowances \
         WHERE silicon_uuid = $1 AND allowed_uuid = ANY($2) AND revoked_at IS NULL)",
    )
    .bind(silicon)
    .bind(candidates)
    .fetch_one(pool)
    .await?)
}

/// Adds `allowed` to `silicon`'s allow-list. Returns the entry and whether it is new.
///
/// # Errors
///
/// Returns database errors.
pub async fn allow(
    pool: &PgPool,
    silicon: &str,
    allowed: &str,
    created_by: &str,
) -> Result<(Allowance, bool), AppError> {
    let inserted = sqlx::query(
        "INSERT INTO silicon_allowances (id, silicon_uuid, allowed_uuid, created_by_uuid) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(silicon)
    .bind(allowed)
    .bind(created_by)
    .execute(pool)
    .await?
    .rows_affected()
        == 1;
    let row = sqlx::query_as::<_, PairRow>(sqlx::AssertSqlSafe(format!(
        "{ALLOWANCE_SELECT} AND g.silicon_uuid = $1 AND g.allowed_uuid = $2"
    )))
    .bind(silicon)
    .bind(allowed)
    .fetch_one(pool)
    .await?;
    Ok((row.allowance(), inserted))
}

/// Removes `allowed` from `silicon`'s allow-list and ends the grants that the
/// allowance made possible: grants to `silicon` from owners outside its circle
/// that `allowed` owns or made. Returns whether an entry existed.
///
/// # Errors
///
/// Returns database errors.
pub async fn disallow(
    pool: &PgPool,
    silicon: &str,
    allowed: &str,
    circle: &[String],
    revoked_by: &str,
) -> Result<bool, AppError> {
    let mut transaction = pool.begin().await?;
    let removed = sqlx::query(
        "UPDATE silicon_allowances SET revoked_at = clock_timestamp(), revoked_by_uuid = $3 \
         WHERE silicon_uuid = $1 AND allowed_uuid = $2 AND revoked_at IS NULL",
    )
    .bind(silicon)
    .bind(allowed)
    .bind(revoked_by)
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        > 0;
    sqlx::query(
        "UPDATE reminder_viewers SET revoked_at = clock_timestamp(), revoked_by_uuid = $3 \
         WHERE viewer_uuid = $1 AND revoked_at IS NULL AND NOT (owner_uuid = ANY($4)) \
           AND (owner_uuid = $2 OR granted_by_uuid = $2)",
    )
    .bind(silicon)
    .bind(allowed)
    .bind(revoked_by)
    .bind(circle)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(removed)
}
