//! Durable due-occurrence materialization.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use uuid::Uuid;

use crate::{
    domain::CronExpression,
    infrastructure::postgres::{ActorType, AuditContext, PostgresRepository, RepositoryError},
    metrics::Metrics,
};

/// Materializes a bounded due batch and atomically advances schedule state.
///
/// # Errors
///
/// Returns an error when stored schedule data is invalid or PostgreSQL cannot
/// complete the transaction. No partial batch is committed.
pub async fn materialize_due(
    repository: &PostgresRepository,
    worker_id: &str,
    now: DateTime<Utc>,
    limit: u32,
    metrics: &Metrics,
) -> anyhow::Result<u64> {
    let mut transaction = repository.begin().await?;
    let schedules = repository
        .lock_due_schedules(&mut transaction, now, limit)
        .await?;
    let audit = AuditContext {
        actor_type: ActorType::System,
        actor_id: worker_id.to_owned(),
        request_id: None,
    };
    let mut inserted = 0_u64;

    for schedule in schedules {
        let next_run_at = next_after_materialization(
            &schedule.timezone,
            &schedule.schedule_kind,
            &schedule.cron,
            now,
        )?;
        let occurrence = repository
            .materialize_locked_occurrence(
                &mut transaction,
                &schedule,
                Uuid::now_v7(),
                now,
                next_run_at,
                &audit,
            )
            .await?;
        if occurrence.inserted {
            inserted += 1;
        }
    }
    transaction.commit().await.map_err(RepositoryError::from)?;
    metrics.executions_materialized.inc_by(inserted);
    Ok(inserted)
}

fn next_after_materialization(
    timezone: &str,
    schedule_kind: &str,
    cron: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    if schedule_kind == "one_time" {
        return Ok(None);
    }
    anyhow::ensure!(
        schedule_kind == "recurring",
        "stored schedule has an invalid kind"
    );
    let timezone = timezone
        .parse::<Tz>()
        .map_err(|_| anyhow::anyhow!("stored schedule has an invalid timezone"))?;
    let expression = CronExpression::parse(cron)
        .map_err(|_| anyhow::anyhow!("stored schedule has an invalid cron expression"))?;
    Ok(Some(expression.next_after(timezone, now)?))
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::next_after_materialization;

    #[test]
    fn one_time_has_no_next_occurrence() -> anyhow::Result<()> {
        let now = "2026-08-31T09:00:00Z".parse::<DateTime<Utc>>()?;
        assert_eq!(
            next_after_materialization("UTC", "one_time", "0 9 * * *", now)?,
            None
        );
        Ok(())
    }

    #[test]
    fn recurring_next_is_strictly_after_worker_time() -> anyhow::Result<()> {
        let now = "2026-08-31T09:00:00Z".parse::<DateTime<Utc>>()?;
        let next = next_after_materialization("UTC", "recurring", "0 9 * * *", now)?;
        assert!(next.is_some_and(|next| next > now));
        Ok(())
    }
}
