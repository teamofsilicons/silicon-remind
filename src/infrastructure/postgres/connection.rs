use std::{str::FromStr as _, time::Duration};

use secrecy::ExposeSecret as _;
use sqlx::{
    PgPool,
    migrate::{MigrateError, Migrator},
    postgres::{PgConnectOptions, PgPoolOptions},
};
use thiserror::Error;

use crate::config::DatabaseSettings;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
const REQUIRED_SCHEMA_FIELDS: &[(&str, &str)] = &[
    ("organization_lifecycle", "org_id"),
    ("silicon_identities", "principal_id"),
    ("schedules", "owner_principal_id"),
    ("schedules", "timezone"),
    ("schedules", "schedule_kind"),
    ("schedules", "cron_expression"),
    ("schedules", "next_run_at"),
    ("schedules", "version"),
    ("executions", "schedule_version"),
    ("executions", "lease_expires_at"),
    ("deleted_reminders", "schedule_id"),
    ("deleted_reminders", "owner_principal_id"),
    ("deleted_reminders", "schedule_kind"),
    ("deleted_reminders", "cron_expression"),
    ("deleted_reminders", "last_triggered_at"),
    ("deleted_reminders", "purged_at"),
    ("deleted_reminders", "record_text"),
    ("hook_destinations", "signing_secret_ciphertext"),
    ("hook_destinations", "encryption_key_version"),
    ("idempotency_records", "request_hash"),
    ("idempotency_records", "response_body"),
    ("internal_event_receipts", "payload_hash"),
    ("audit_records", "action"),
];

/// PostgreSQL dependency or schema-readiness failure.
#[derive(Debug, Error)]
pub enum HealthCheckError {
    /// The bounded readiness probe exceeded its deadline.
    #[error("PostgreSQL readiness check timed out")]
    Timeout,
    /// PostgreSQL could not execute a readiness query.
    #[error("PostgreSQL readiness query failed")]
    Database(#[from] sqlx::Error),
    /// The database has not applied the migrations embedded in this binary.
    #[error("PostgreSQL schema is not current for this binary")]
    SchemaNotCurrent,
}

/// Opens and validates the runtime PostgreSQL connection pool.
///
/// Every connection uses UTC and the configured statement timeout. Pool
/// creation eagerly establishes the configured minimum connections so startup
/// fails before the process reports readiness when PostgreSQL is unavailable.
///
/// # Errors
///
/// Returns a `SQLx` error if the URL is invalid, a session cannot be configured,
/// or the initial database connection cannot be established.
pub async fn connect(settings: &DatabaseSettings) -> Result<PgPool, sqlx::Error> {
    let connect_options = PgConnectOptions::from_str(settings.url.expose_secret())?
        .application_name("silicon-remind");
    let statement_timeout = settings.statement_timeout;

    PgPoolOptions::new()
        .max_connections(settings.max_connections.get())
        .min_connections(settings.min_connections)
        .acquire_timeout(settings.acquire_timeout)
        .after_connect(move |connection, _metadata| {
            Box::pin(async move {
                sqlx::query("SET TIME ZONE 'UTC'")
                    .execute(&mut *connection)
                    .await?;

                if let Some(timeout) = statement_timeout {
                    let timeout_value = format!("{}ms", timeout.as_millis());
                    sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                        .bind(timeout_value)
                        .execute(&mut *connection)
                        .await?;
                }

                Ok(())
            })
        })
        .connect_with(connect_options)
        .await
}

/// Applies all embedded migrations in version order.
///
/// # Errors
///
/// Returns a migration error when PostgreSQL cannot acquire the migration lock
/// or any migration fails.
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    MIGRATOR.run(pool).await
}

/// Verifies PostgreSQL connectivity and the schema expected by this binary.
///
/// The probe validates every embedded migration by version and checksum, while
/// tolerating migrations from a newer rolling-deployment binary. It also checks
/// critical table columns so a corrupted or manually altered schema cannot be
/// reported ready solely because its migration ledger is intact.
///
/// # Errors
///
/// Returns an error when PostgreSQL is unavailable, the two-second deadline is
/// exceeded, an embedded migration is absent or changed, or a core schema field
/// is missing.
pub async fn health_check(pool: &PgPool) -> Result<(), HealthCheckError> {
    match tokio::time::timeout(HEALTH_CHECK_TIMEOUT, verify_schema(pool)).await {
        Ok(result) => result,
        Err(_) => Err(HealthCheckError::Timeout),
    }
}

async fn verify_schema(pool: &PgPool) -> Result<(), HealthCheckError> {
    let applied = sqlx::query_as::<_, (i64, Vec<u8>, bool)>(
        r"
        SELECT version, checksum, success
        FROM _sqlx_migrations
        ",
    )
    .fetch_all(pool)
    .await?;

    if !migrations_are_current(&applied) {
        return Err(HealthCheckError::SchemaNotCurrent);
    }

    let table_names = REQUIRED_SCHEMA_FIELDS
        .iter()
        .map(|(table, _)| *table)
        .collect::<Vec<_>>();
    let column_names = REQUIRED_SCHEMA_FIELDS
        .iter()
        .map(|(_, column)| *column)
        .collect::<Vec<_>>();
    let fields_found = sqlx::query_scalar::<_, i64>(
        r"
        SELECT count(*)
        FROM unnest($1::text[], $2::text[]) AS required(table_name, column_name)
        WHERE EXISTS (
            SELECT 1
            FROM pg_catalog.pg_attribute AS attribute
            WHERE attribute.attrelid = to_regclass(required.table_name)
              AND attribute.attname = required.column_name
              AND attribute.attnum > 0
              AND NOT attribute.attisdropped
        )
        ",
    )
    .bind(&table_names)
    .bind(&column_names)
    .fetch_one(pool)
    .await?;
    if usize::try_from(fields_found).ok() != Some(REQUIRED_SCHEMA_FIELDS.len()) {
        return Err(HealthCheckError::SchemaNotCurrent);
    }

    Ok(())
}

fn migrations_are_current(applied: &[(i64, Vec<u8>, bool)]) -> bool {
    MIGRATOR.iter().all(|expected| {
        applied.iter().any(|(version, checksum, success)| {
            *version == expected.version
                && *success
                && checksum.as_slice() == expected.checksum.as_ref()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{MIGRATOR, REQUIRED_SCHEMA_FIELDS, migrations_are_current};

    fn applied_migrations() -> Vec<(i64, Vec<u8>, bool)> {
        MIGRATOR
            .iter()
            .map(|migration| (migration.version, migration.checksum.to_vec(), true))
            .collect()
    }

    #[test]
    fn accepts_all_embedded_migrations_and_tolerates_newer_versions() {
        let mut applied = applied_migrations();
        applied.push((i64::MAX, vec![0; 48], true));
        assert!(migrations_are_current(&applied));
    }

    #[test]
    fn rejects_missing_failed_or_changed_migrations() {
        let applied = applied_migrations();
        let missing = applied.iter().skip(1).cloned().collect::<Vec<_>>();
        assert!(!migrations_are_current(&missing));

        let mut failed = applied.clone();
        if let Some(first) = failed.first_mut() {
            first.2 = false;
        }
        assert!(!migrations_are_current(&failed));

        let mut changed = applied;
        if let Some(first) = changed.first_mut() {
            first.1.fill(0);
        }
        assert!(!migrations_are_current(&changed));
    }

    #[test]
    fn readiness_requires_deleted_reminder_ledger_fields() {
        for field in [
            ("deleted_reminders", "schedule_id"),
            ("deleted_reminders", "owner_principal_id"),
            ("deleted_reminders", "schedule_kind"),
            ("deleted_reminders", "cron_expression"),
            ("deleted_reminders", "last_triggered_at"),
            ("deleted_reminders", "purged_at"),
            ("deleted_reminders", "record_text"),
        ] {
            assert!(REQUIRED_SCHEMA_FIELDS.contains(&field));
        }
    }
}
