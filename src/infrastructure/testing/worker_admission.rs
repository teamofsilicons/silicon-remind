//! A cheap local hint that keeps the worker out of idle test environments.
use super::{AppError, EnvironmentLease, MANAGED, TestEnvironments, Uuid, lock, schema};
use chrono::{DateTime, Utc};
use sqlx::AssertSqlSafe;

impl TestEnvironments {
    /// Skips environments with no due or maintenance work.
    ///
    /// The hint cannot lease or deliver anything; a positive hint goes through
    /// the normal admission, whose lifecycle lock fences clean and retirement.
    ///
    /// # Errors
    /// Returns database errors.
    pub async fn enter_worker_if_pending(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        encryption_key_version: i16,
    ) -> Result<Option<EnvironmentLease>, AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, false).await?;
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM public.testing_environments WHERE id=$1 AND deleted_at IS NULL AND {MANAGED} AND last_activity_at > clock_timestamp() - interval '15 days')"
        );
        let active: bool = sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if !active {
            return Ok(None);
        }
        // Only this UUID-derived schema is visible. The shared lifecycle lock
        // prevents a clean or drop while its candidate rows are inspected.
        sqlx::query("SELECT set_config('search_path', $1, true)")
            .bind(format!("{}, pg_catalog", schema(id)))
            .execute(&mut *tx)
            .await?;
        let pending: bool = sqlx::query_scalar(include_str!("worker_pending.sql"))
            .bind(now)
            .bind(encryption_key_version)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        if !pending {
            return Ok(None);
        }
        self.enter_worker(id).await
    }
}
