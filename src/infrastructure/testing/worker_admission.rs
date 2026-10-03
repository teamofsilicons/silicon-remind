//! Local work hints avoid spending IAM authority checks on idle worker polls.
use super::{AppError, DateTime, EnvironmentLease, TestEnvironments, Utc, Uuid, lock, schema};

impl TestEnvironments {
    /// Skips idle worlds without caching an authorization decision.
    ///
    /// The preliminary query cannot lease work or authorize delivery. A positive
    /// hint always goes through the existing fresh IAM discovery and lifecycle
    /// admission; clean/revocation between these steps remains fenced there.
    ///
    /// # Errors
    /// Returns database errors or the existing live admission failure.
    pub async fn enter_worker_if_pending(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        encryption_key_version: i16,
    ) -> Result<Option<EnvironmentLease>, AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, false).await?;
        let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.testing_environments WHERE id=$1 AND deleted_at IS NULL AND (iam_control_version IS NOT NULL OR last_activity_at > clock_timestamp() - interval '15 days'))")
            .bind(id).fetch_one(&mut *tx).await?;
        if !active
            || Self::honeycomb_fence(&mut tx, id)
                .await?
                .is_some_and(|fence| fence.state != "active")
        {
            return Ok(None);
        }
        // Only this UUID-derived schema is visible. The shared lifecycle lock
        // prevents a clean/drop while inspecting its local candidate rows.
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
