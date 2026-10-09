//! Remind's own test environments: isolated copies of the reminder schema in a
//! shared testing database, each reached with its own 32-character key.
//!
//! An environment belongs to the account that created it. Its owner and, when
//! the owner is a Silicon, the owner's custodian manage it (key, rotation,
//! deletion, restoration); the owner's circle can see it and read its key.
//! Whoever holds the key reads everything inside it and acts there as their
//! own Silicon Accounts account. Environments that Silicon IAM or Honeycomb
//! controlled are dormant: never listed, entered, worked or swept.
//!
//! A transaction-scoped advisory lock guards every request and worker cycle.
//! Lifecycle changes take it exclusively, so a clean or deletion never races a
//! delivery that was already admitted.

use super::crypto::{EncryptedSecret, SecretCipherKeyring};
use crate::{config::DatabaseSettings, domain::AccountRef, error::AppError};
use chrono::{DateTime, Utc};
use rand::{Rng as _, distr::Alphanumeric};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sqlx::{
    AssertSqlSafe, FromRow, PgPool, Postgres, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use uuid::Uuid;

mod worker_admission;

static CONTROL_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./testing/migrations");
const MAX_CACHED_TEST_POOLS: usize = 4;

/// Environments Remind manages itself (not dormant IAM/Honeycomb ones).
const MANAGED: &str = "iam_control_version IS NULL";

/// Metadata of one test environment. Its key is never part of this.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct TestEnvironment {
    /// Public selector, never a credential.
    pub id: Uuid,
    /// The owning account's Silicon Accounts uuid; `None` for an environment
    /// from before the move to Silicon Accounts that no account owns yet.
    #[serde(rename = "owner_uuid")]
    pub owner_uuid: Option<String>,
    /// The owner as Remind shows it (filled in by the API).
    #[sqlx(skip)]
    pub owner: Option<AccountRef>,
    /// Name, unique among the owner's active environments.
    pub name: String,
    /// Optional purpose.
    pub description: Option<String>,
    /// Monotonic metadata revision.
    pub version: i64,
    /// Creation instant.
    pub created_at: DateTime<Utc>,
    /// Most recent successful use; worker ticks do not keep it alive.
    pub last_activity_at: DateTime<Utc>,
    /// Retirement instant, if retired.
    pub deleted_at: Option<DateTime<Utc>>,
    /// Permanent deletion deadline.
    pub purge_after: Option<DateTime<Utc>>,
}

/// Inputs for a new, empty environment.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTestEnvironment {
    /// Name, unique among the caller's active environments.
    pub name: String,
    /// Optional purpose.
    pub description: Option<String>,
    /// Retired: test environments use the caller's Silicon Accounts sign-in.
    #[serde(default)]
    pub iam_test_key: Option<serde::de::IgnoredAny>,
    /// Retired: test environments use the caller's Silicon Accounts sign-in.
    #[serde(default)]
    pub iam_app_secret: Option<serde::de::IgnoredAny>,
}

/// What the caller may do with environments, by owner uuid.
#[derive(Clone, Debug, Default)]
pub struct EnvironmentAccess {
    /// Owners whose environments the caller sees and whose keys it may read
    /// (its circle).
    pub readers: Vec<String>,
    /// Owners whose environments the caller manages (itself, and for a Carbon
    /// the Silicons it looks after).
    pub managers: Vec<String>,
}

impl EnvironmentAccess {
    fn manages(&self, environment: &TestEnvironment) -> bool {
        environment
            .owner_uuid
            .as_ref()
            .is_some_and(|owner| self.managers.contains(owner))
    }
}

/// Sealed per-environment secrets. Older environments also sealed IAM keys,
/// which are ignored now.
#[derive(Serialize, Deserialize)]
struct Credentials {
    key: String,
}

/// Test database dependencies; cache entries hold pools, never authority decisions.
#[derive(Clone)]
pub struct TestEnvironments {
    control: PgPool,
    database: DatabaseSettings,
    cipher: SecretCipherKeyring,
    pools: Arc<Mutex<HashMap<Uuid, PgPool>>>,
}

impl std::fmt::Debug for TestEnvironments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestEnvironments").finish_non_exhaustive()
    }
}

/// A live admission, held until the request or worker cycle finishes.
pub struct EnvironmentLease {
    /// Environment metadata, rechecked under its lifecycle lock.
    pub environment: TestEnvironment,
    /// Isolated data pool.
    pub pool: PgPool,
    guard: Transaction<'static, Postgres>,
}

impl EnvironmentLease {
    /// Releases the admission and optionally records successful use.
    ///
    /// # Errors
    ///
    /// Returns a database error if activity bookkeeping or the commit fails.
    pub async fn finish(mut self, activity: bool) -> Result<(), AppError> {
        if activity {
            sqlx::query("UPDATE public.testing_environments SET last_activity_at = clock_timestamp() WHERE id = $1 AND deleted_at IS NULL")
                .bind(self.environment.id).execute(&mut *self.guard).await?;
        }
        self.guard.commit().await?;
        Ok(())
    }
}

impl TestEnvironments {
    /// Connects to an already migrated control database.
    ///
    /// # Errors
    ///
    /// Returns a connection error for the shared testing database.
    pub async fn connect(
        database: &DatabaseSettings,
        cipher: SecretCipherKeyring,
    ) -> Result<Self, AppError> {
        let control = super::postgres::connect(database).await?;
        Ok(Self {
            control,
            database: database.clone(),
            cipher,
            pools: Arc::default(),
        })
    }

    /// Verifies the shared test database and every embedded control migration.
    ///
    /// # Errors
    ///
    /// Returns unavailable for unreachable storage, missing tables or mismatched migrations.
    pub async fn health_check(&self) -> Result<(), AppError> {
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let applied: Vec<(i64, Vec<u8>, bool)> = sqlx::query_as(
                "SELECT version, checksum, success FROM public._sqlx_migrations"
            ).fetch_all(&self.control).await?;
            let valid = CONTROL_MIGRATOR.iter().all(|expected| applied.iter().any(|(version, checksum, success)|
                *version == expected.version && *success && checksum.as_slice() == expected.checksum.as_ref()
            ));
            sqlx::query("SELECT id, key_hash, owner_uuid, secrets, deleted_at, purge_after FROM public.testing_environments LIMIT 0")
                .execute(&self.control).await?;
            Ok::<_, sqlx::Error>(valid)
        }).await;
        if !matches!(result, Ok(Ok(true))) {
            return Err(AppError::DependencyUnavailable {
                dependency: "testing_postgresql",
            });
        }
        Ok(())
    }

    /// The control pool, for operator tooling (identity linking).
    #[must_use]
    pub const fn control(&self) -> &PgPool {
        &self.control
    }

    /// One-shot control database migration, used only by remind-migrate. Every
    /// environment schema, dormant ones included, gets the same migrations.
    ///
    /// # Errors
    ///
    /// Returns schema, checksum, or database errors.
    pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
        CONTROL_MIGRATOR.run(pool).await?;
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM public.testing_environments ORDER BY id")
                .fetch_all(pool)
                .await?;
        for id in ids {
            let mut tx = pool.begin().await?;
            lock(&mut tx, id, true).await?;
            sqlx::raw_sql(AssertSqlSafe(format!(
                "SET LOCAL search_path TO {}",
                schema(id)
            )))
            .execute(&mut *tx)
            .await?;
            migrate_data_schema(&mut tx).await?;
            tx.commit().await?;
        }
        Ok(())
    }

    /// Creates an empty environment owned by the caller and returns its key.
    ///
    /// # Errors
    ///
    /// Returns invalid input, a duplicate active name, or a schema/storage failure.
    pub async fn create(
        &self,
        owner_uuid: &str,
        input: CreateTestEnvironment,
    ) -> Result<(TestEnvironment, SecretString), AppError> {
        if input.iam_test_key.is_some() || input.iam_app_secret.is_some() {
            return Err(AppError::invalid(
                "test_key_field_retired",
                "A test environment no longer takes a key from another service: it uses your Silicon Accounts sign-in. Send only name and description.",
            ));
        }
        if input.name.trim().is_empty()
            || input.name.len() > 100
            || input.description.as_ref().is_some_and(|s| s.len() > 10000)
        {
            return Err(AppError::Validation);
        }
        let id = Uuid::now_v7();
        let key = generate_key();
        let sealed = self.seal(id, &Credentials { key: key.clone() })?;
        let mut transaction = self.control.begin().await?;
        sqlx::query("INSERT INTO public.testing_environments (id,org_id,creator_id,owner_uuid,name,description,iam_environment_id,key_hash,secrets) VALUES ($1,NULL,$2,$2,$3,$4,NULL,$5,$6)")
            .bind(id).bind(owner_uuid)
            .bind(input.name.trim()).bind(input.description).bind(hash(&key)).bind(sealed)
            .execute(&mut *transaction).await?;
        // DDL and metadata publish atomically. The production migrations build
        // the schema; the test-only quota is added afterwards.
        let schema = schema(id);
        sqlx::raw_sql(AssertSqlSafe(format!(
            "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema};"
        )))
        .execute(&mut *transaction)
        .await?;
        migrate_data_schema(&mut transaction).await?;
        sqlx::raw_sql(QUOTA_SQL).execute(&mut *transaction).await?;
        transaction.commit().await?;
        let access = EnvironmentAccess {
            readers: vec![owner_uuid.to_owned()],
            managers: vec![owner_uuid.to_owned()],
        };
        Ok((self.get(&access, id).await?, SecretString::from(key)))
    }

    /// Lists environments the caller may see, including recoverable retired
    /// ones when `deleted` is set.
    ///
    /// # Errors
    ///
    /// Returns invalid pagination or a database failure.
    pub async fn list(
        &self,
        access: &EnvironmentAccess,
        deleted: bool,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<TestEnvironment>, AppError> {
        if !(1..=100).contains(&limit) {
            return Err(AppError::Validation);
        }
        let sql = format!(
            "SELECT * FROM public.testing_environments WHERE owner_uuid = ANY($1) AND {MANAGED} AND COALESCE(purge_after,last_activity_at+interval '45 days') > clock_timestamp() AND ($2 OR (deleted_at IS NULL AND last_activity_at > clock_timestamp()-interval '15 days')) AND ($3::uuid IS NULL OR id > $3) ORDER BY id LIMIT $4"
        );
        Ok(sqlx::query_as(AssertSqlSafe(sql))
            .bind(&access.readers)
            .bind(deleted)
            .bind(after)
            .bind(limit)
            .fetch_all(&self.control)
            .await?
            .into_iter()
            .map(logical_lifecycle)
            .collect())
    }

    /// Reads one environment the caller may see, inside the recovery window.
    ///
    /// # Errors
    ///
    /// Returns not found for environments outside the caller's circle or past
    /// recovery, or a database error.
    pub async fn get(
        &self,
        access: &EnvironmentAccess,
        id: Uuid,
    ) -> Result<TestEnvironment, AppError> {
        let sql = format!(
            "SELECT * FROM public.testing_environments WHERE id = $1 AND owner_uuid = ANY($2) AND {MANAGED} AND COALESCE(purge_after,last_activity_at+interval '45 days') > clock_timestamp()"
        );
        sqlx::query_as(AssertSqlSafe(sql))
            .bind(id)
            .bind(&access.readers)
            .fetch_optional(&self.control)
            .await?
            .map(logical_lifecycle)
            .ok_or(AppError::NotFound)
    }

    /// Reads the active key. The owner's circle may read it.
    ///
    /// # Errors
    ///
    /// Returns not found for invisible or retired environments, or a
    /// credential-storage error.
    pub async fn key(
        &self,
        access: &EnvironmentAccess,
        id: Uuid,
    ) -> Result<SecretString, AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, false).await?;
        let row = guarded_get(&mut tx, access, id).await?;
        if row.deleted_at.is_some() {
            return Err(AppError::NotFound);
        }
        let credentials = self.credentials(&mut tx, id).await?;
        tx.commit().await?;
        Ok(SecretString::from(credentials.key))
    }

    /// Rotates the key, or restores a retired environment with a new key.
    ///
    /// # Errors
    ///
    /// Returns authorization, lifecycle, name-conflict or storage errors.
    pub async fn rotate(
        &self,
        access: &EnvironmentAccess,
        id: Uuid,
        restore: bool,
    ) -> Result<SecretString, AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, true).await?;
        let row = guarded_get(&mut tx, access, id).await?;
        require_manager(access, &row)?;
        if row.deleted_at.is_some() != restore {
            return Err(AppError::conflict("environment_state_conflict"));
        }
        let key = generate_key();
        sqlx::query("UPDATE public.testing_environments SET key_hash=$2,secrets=$3,deleted_at=NULL,purge_after=NULL,last_activity_at=clock_timestamp(),version=version+1 WHERE id=$1")
            .bind(id).bind(hash(&key)).bind(self.seal(id, &Credentials { key: key.clone() })?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(SecretString::from(key))
    }

    /// Retires an environment now; its data stays recoverable for 30 days.
    ///
    /// # Errors
    ///
    /// Returns authorization, not-found or database errors.
    pub async fn delete(&self, access: &EnvironmentAccess, id: Uuid) -> Result<(), AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, true).await?;
        let row = guarded_get(&mut tx, access, id).await?;
        require_manager(access, &row)?;
        retire(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Retires every active environment a deleted account owned (idempotent).
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub async fn retire_owned_by(&self, owner_uuid: &str) -> Result<u64, AppError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM public.testing_environments WHERE owner_uuid = $1 AND deleted_at IS NULL ORDER BY id",
        )
        .bind(owner_uuid)
        .fetch_all(&self.control)
        .await?;
        let mut retired = 0;
        for id in ids {
            let mut tx = self.control.begin().await?;
            lock(&mut tx, id, true).await?;
            retire(&mut tx, id).await?;
            tx.commit().await?;
            retired += 1;
        }
        Ok(retired)
    }
}

impl TestEnvironments {
    /// Admits a key-bearing request, rechecking active state under the lock.
    ///
    /// # Errors
    ///
    /// Returns unauthorized for malformed, rotated, unknown or retired keys, or
    /// a database error.
    pub async fn enter(
        &self,
        key: &SecretString,
        exclusive: bool,
    ) -> Result<EnvironmentLease, AppError> {
        if key.expose_secret().len() != 32
            || !key
                .expose_secret()
                .bytes()
                .all(|c| c.is_ascii_alphanumeric())
        {
            return Err(AppError::unauthenticated(
                "test_key_invalid",
                "X-Remind-Test-Key must be the 32-character key of an active test environment.",
            ));
        }
        let unknown = || {
            AppError::unauthenticated(
                "test_key_invalid",
                "No active test environment has this key (it may have been rotated, retired or cleaned up).",
            )
        };
        let sql = format!(
            "SELECT id FROM public.testing_environments WHERE key_hash=$1 AND deleted_at IS NULL AND {MANAGED}"
        );
        let id: Uuid = sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(hash(key.expose_secret()))
            .fetch_optional(&self.control)
            .await?
            .ok_or_else(unknown)?;
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, exclusive).await?;
        let sql = format!(
            "SELECT * FROM public.testing_environments WHERE id=$1 AND key_hash=$2 AND deleted_at IS NULL AND {MANAGED} AND last_activity_at > clock_timestamp() - interval '15 days'"
        );
        let environment: TestEnvironment = sqlx::query_as(AssertSqlSafe(sql))
            .bind(id)
            .bind(hash(key.expose_secret()))
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(unknown)?;
        Ok(EnvironmentLease {
            pool: self.pool(environment.id).await?,
            environment,
            guard: tx,
        })
    }

    /// Admits the worker without counting its poll as use.
    ///
    /// # Errors
    ///
    /// Returns a database error; inactive environments return `None`.
    pub async fn enter_worker(&self, id: Uuid) -> Result<Option<EnvironmentLease>, AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, false).await?;
        let sql = format!(
            "SELECT * FROM public.testing_environments WHERE id=$1 AND deleted_at IS NULL AND {MANAGED} AND last_activity_at > clock_timestamp() - interval '15 days'"
        );
        let row: Option<TestEnvironment> = sqlx::query_as(AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        match row {
            Some(environment) => Ok(Some(EnvironmentLease {
                pool: self.pool(environment.id).await?,
                environment,
                guard: tx,
            })),
            None => Ok(None),
        }
    }

    /// Removes all of an environment's data while the caller holds its exclusive lease.
    ///
    /// # Errors
    ///
    /// Returns a database error if the atomic truncate cannot complete.
    pub async fn clean(&self, lease: &mut EnvironmentLease) -> Result<(), AppError> {
        // The production audit trigger is copied into each environment. Its
        // truncate guard is suspended only in this transaction, under the
        // exclusive lock; a rollback restores it.
        let schema = schema(lease.environment.id);
        let tables = [
            "iam_organization_bindings",
            "organization_lifecycle",
            "silicon_identities",
            "schedules",
            "executions",
            "deleted_reminders",
            "hook_destinations",
            "idempotency_records",
            "internal_event_receipts",
            "telemetry_events",
            "bug_reports",
            "audit_records",
            "accounts",
            "account_keys",
            "identity_links",
            "reminder_viewers",
            "silicon_allowances",
        ]
        .map(|table| format!("{schema}.{table}"))
        .join(", ");
        sqlx::raw_sql(AssertSqlSafe(format!("ALTER TABLE {schema}.audit_records DISABLE TRIGGER audit_records_reject_truncate; TRUNCATE {tables} RESTART IDENTITY CASCADE; ALTER TABLE {schema}.audit_records ENABLE TRIGGER audit_records_reject_truncate")))
            .execute(&mut *lease.guard).await?;
        Ok(())
    }

    /// A bounded page of active environment ids for the worker.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn active_ids(&self, after: Option<Uuid>) -> Result<Vec<Uuid>, AppError> {
        let sql = format!(
            "SELECT id FROM public.testing_environments WHERE deleted_at IS NULL AND {MANAGED} AND last_activity_at > clock_timestamp() - interval '15 days' AND ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT 100"
        );
        Ok(sqlx::query_scalar(AssertSqlSafe(sql))
            .bind(after)
            .fetch_all(&self.control)
            .await?)
    }

    /// Applies inactivity retirement and permanent removal in bounded batches.
    ///
    /// # Errors
    ///
    /// Returns database errors while retiring or dropping an environment.
    pub async fn sweep(&self) -> Result<(), AppError> {
        let sql = format!(
            "SELECT id FROM public.testing_environments WHERE {MANAGED} AND ((deleted_at IS NULL AND last_activity_at <= clock_timestamp() - interval '15 days') OR purge_after <= clock_timestamp()) ORDER BY id LIMIT 100"
        );
        let ids: Vec<Uuid> = sqlx::query_scalar(AssertSqlSafe(sql))
            .fetch_all(&self.control)
            .await?;
        for id in ids {
            let mut tx = self.control.begin().await?;
            lock(&mut tx, id, true).await?;
            let expired: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.testing_environments WHERE id=$1 AND COALESCE(purge_after,last_activity_at+interval '45 days') <= clock_timestamp())").bind(id).fetch_one(&mut *tx).await?;
            if expired {
                sqlx::raw_sql(AssertSqlSafe(format!(
                    "DROP SCHEMA IF EXISTS {} CASCADE",
                    schema(id)
                )))
                .execute(&mut *tx)
                .await?;
                sqlx::query("DELETE FROM public.testing_environments WHERE id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                self.pools.lock().await.remove(&id);
            } else {
                sqlx::query("UPDATE public.testing_environments SET key_hash=NULL,deleted_at=last_activity_at+interval '15 days',purge_after=last_activity_at+interval '45 days',version=version+1 WHERE id=$1 AND deleted_at IS NULL AND last_activity_at <= clock_timestamp()-interval '15 days'")
                    .bind(id).execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
        Ok(())
    }

    async fn pool(&self, id: Uuid) -> Result<PgPool, AppError> {
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get(&id) {
            return Ok(pool.clone());
        }
        // Every cached pool can open connections; keep the cache small so a
        // large fleet of environments cannot exhaust the shared database.
        if pools.len() >= MAX_CACHED_TEST_POOLS
            && let Some(oldest) = pools.keys().min().copied()
        {
            pools.remove(&oldest);
        }
        let options: PgConnectOptions = self.database.url.expose_secret().parse()?;
        let path = schema(id);
        let pool = PgPoolOptions::new().max_connections(2).min_connections(0)
            .acquire_timeout(self.database.acquire_timeout).idle_timeout(Duration::from_secs(30))
            .after_connect(move |connection, _| {
                let path = path.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path',$1,false),set_config('timezone','UTC',false),set_config('statement_timeout','10000',false)")
                        .bind(path).execute(connection).await?;
                    Ok(())
                })
            }).connect_with(options).await?;
        pools.insert(id, pool.clone());
        Ok(pool)
    }

    async fn credentials(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<Credentials, AppError> {
        let sealed: serde_json::Value =
            sqlx::query_scalar("SELECT secrets FROM public.testing_environments WHERE id=$1")
                .bind(id)
                .fetch_one(&mut **tx)
                .await?;
        let encrypted: EncryptedSecret = serde_json::from_value(sealed)
            .map_err(|e| AppError::internal("test_credentials", e))?;
        let plaintext = self
            .cipher
            .decrypt(&encrypted, id.as_bytes())
            .map_err(|e| AppError::internal("test_credentials", e))?;
        serde_json::from_str(plaintext.expose_secret())
            .map_err(|e| AppError::internal("test_credentials", e))
    }

    fn seal(&self, id: Uuid, credentials: &Credentials) -> Result<serde_json::Value, AppError> {
        let plaintext = serde_json::to_string(credentials)
            .map_err(|e| AppError::internal("test_credentials", e))?;
        let encrypted = self
            .cipher
            .encrypt(&SecretString::from(plaintext), id.as_bytes())
            .map_err(|e| AppError::internal("test_credentials", e))?;
        serde_json::to_value(encrypted).map_err(|e| AppError::internal("test_credentials", e))
    }
}

fn require_manager(
    access: &EnvironmentAccess,
    environment: &TestEnvironment,
) -> Result<(), AppError> {
    if access.manages(environment) {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "not_environment_manager",
            "Only the environment's owner, or the custodian of the Silicon that owns it, can rotate, delete or restore it.",
        ))
    }
}

async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    exclusive: bool,
) -> Result<(), AppError> {
    let query = if exclusive {
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 78153))"
    } else {
        "SELECT pg_advisory_xact_lock_shared(hashtextextended($1, 78153))"
    };
    sqlx::query(query)
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn retire(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE public.testing_environments SET key_hash=NULL,deleted_at=LEAST(clock_timestamp(),last_activity_at+interval '15 days'),purge_after=LEAST(clock_timestamp(),last_activity_at+interval '15 days')+interval '30 days',version=version+1 WHERE id=$1 AND deleted_at IS NULL")
        .bind(id).execute(&mut **tx).await?;
    Ok(())
}

fn schema(id: Uuid) -> String {
    format!("remind_test_{}", id.simple())
}
fn hash(key: &str) -> Vec<u8> {
    Sha256::digest(key.as_bytes()).to_vec()
}
fn generate_key() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

const QUOTA_SQL: &str = r"
CREATE FUNCTION enforce_test_reminder_limit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(current_schema(), 78154));
    IF (SELECT count(*) FROM schedules) >= 100 THEN
        RAISE EXCEPTION 'Test environments allow at most 100 retained reminders' USING ERRCODE = '23514', CONSTRAINT = 'test_reminder_limit';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER test_reminder_limit BEFORE INSERT ON schedules FOR EACH ROW EXECUTE FUNCTION enforce_test_reminder_limit();
";

// One shared migration source keeps every environment on the production schema.
async fn migrate_data_schema(tx: &mut Transaction<'_, Postgres>) -> Result<(), AppError> {
    static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS _sqlx_migrations (version bigint PRIMARY KEY, description text NOT NULL, installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL, checksum bytea NOT NULL, execution_time bigint NOT NULL)").execute(&mut **tx).await?;
    for migration in MIGRATOR.iter() {
        let checksum: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT checksum FROM _sqlx_migrations WHERE version=$1 AND success",
        )
        .bind(migration.version)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(checksum) = checksum {
            if checksum != migration.checksum.as_ref() {
                return Err(AppError::conflict("test_schema_checksum_mismatch"));
            }
            continue;
        }
        sqlx::raw_sql(AssertSqlSafe(migration.sql.as_ref()))
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO _sqlx_migrations (version,description,success,checksum,execution_time) VALUES ($1,$2,true,$3,0)")
            .bind(migration.version).bind(migration.description.as_ref()).bind(migration.checksum.as_ref()).execute(&mut **tx).await?;
    }
    Ok(())
}

// Deadlines apply independently of when a background sweep physically runs.
fn logical_lifecycle(mut environment: TestEnvironment) -> TestEnvironment {
    let retired_at = environment.last_activity_at + chrono::Duration::days(15);
    if environment.deleted_at.is_none() && retired_at <= Utc::now() {
        environment.deleted_at = Some(retired_at);
        environment.purge_after = Some(retired_at + chrono::Duration::days(30));
    }
    environment
}

async fn guarded_get(
    tx: &mut Transaction<'_, Postgres>,
    access: &EnvironmentAccess,
    id: Uuid,
) -> Result<TestEnvironment, AppError> {
    let sql = format!(
        "SELECT * FROM public.testing_environments WHERE id=$1 AND owner_uuid = ANY($2) AND {MANAGED} AND COALESCE(purge_after,last_activity_at+interval '45 days')>clock_timestamp()"
    );
    sqlx::query_as(AssertSqlSafe(sql))
        .bind(id)
        .bind(&access.readers)
        .fetch_optional(&mut **tx)
        .await?
        .map(logical_lifecycle)
        .ok_or(AppError::NotFound)
}

#[cfg(test)]
mod tests;
