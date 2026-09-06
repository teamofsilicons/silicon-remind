//! Shared test database with isolated copies of the production reminder schema.
//!
//! A transaction-scoped advisory lock guards the entire API request or worker
//! cycle. Lifecycle mutations take the exclusive lock, so a successful clean or
//! deletion cannot race a delivery which was already admitted.

use super::{
    crypto::{EncryptedSecret, SecretCipherKeyring},
    iam::IamClient,
};
use crate::{
    config::{DatabaseSettings, IamSettings},
    domain::Actor,
    error::AppError,
};
use chrono::{DateTime, Utc};
use rand::{Rng as _, distr::Alphanumeric};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use silicon_iam_client::{Client, Credential, EnvironmentKey, models};
use sqlx::{
    AssertSqlSafe, FromRow, PgPool, Postgres, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use uuid::Uuid;

static CONTROL_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./testing/migrations");

/// Metadata visible to an environment's owning organization. Credentials are separate.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct TestEnvironment {
    /// Public environment selector, never an authentication credential.
    pub id: Uuid,
    /// Production organization that owns this test environment.
    pub org_id: String,
    /// Production principal that created it.
    pub creator_id: Uuid,
    /// Human-readable name.
    pub name: String,
    /// Optional purpose.
    pub description: Option<String>,
    /// IAM sandbox to which all authentication is bound.
    pub iam_environment_id: Uuid,
    /// Monotonic metadata revision.
    pub version: i64,
    /// Creation instant.
    pub created_at: DateTime<Utc>,
    /// Most recent successful user activity; worker ticks do not keep it alive.
    pub last_activity_at: DateTime<Utc>,
    /// Retirement instant, if inactive.
    pub deleted_at: Option<DateTime<Utc>>,
    /// Permanent deletion deadline.
    pub purge_after: Option<DateTime<Utc>>,
}

/// Inputs for an empty Remind replica, paired with an existing IAM replica.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTestEnvironment {
    /// Unique active name within the production organization.
    pub name: String,
    /// Optional purpose.
    pub description: Option<String>,
    /// IAM root environment key. The IAM environment ID is verified remotely.
    pub iam_test_key: SecretString,
    /// Test-only credential from creating/importing the Remind app into IAM.
    #[serde(default)]
    pub iam_app_secret: Option<SecretString>,
}

#[derive(Serialize, Deserialize)]
struct Credentials {
    key: String,
    iam_key: String,
    #[serde(default)]
    iam_app_secret: Option<String>,
}

/// Test database dependencies; cache entries contain pools, never authority decisions.
#[derive(Clone)]
pub struct TestEnvironments {
    control: PgPool,
    database: DatabaseSettings,
    iam_settings: IamSettings,
    cipher: SecretCipherKeyring,
    pools: Arc<Mutex<HashMap<Uuid, PgPool>>>,
}

impl std::fmt::Debug for TestEnvironments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestEnvironments").finish_non_exhaustive()
    }
}

/// A live environment admission, held until the request or delivery finishes.
pub struct EnvironmentLease {
    /// Environment metadata, rechecked under its lifecycle lock.
    pub environment: TestEnvironment,
    /// Isolated data pool.
    pub pool: PgPool,
    /// Official IAM client with mandatory test credentials.
    pub iam: Option<IamClient>,
    /// Expected IAM root key for signed test webhook verification.
    pub iam_key: EnvironmentKey,
    guard: Transaction<'static, Postgres>,
}

impl EnvironmentLease {
    /// Releases the admission and optionally records successful user activity.
    ///
    /// # Errors
    ///
    /// Returns a database error if activity bookkeeping or transaction commit fails.
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
    /// Returns a connection or configuration error for the shared testing database.
    pub async fn connect(
        database: &DatabaseSettings,
        iam: &IamSettings,
        cipher: SecretCipherKeyring,
    ) -> Result<Self, AppError> {
        let control = super::postgres::connect(database).await?;
        Ok(Self {
            control,
            database: database.clone(),
            iam_settings: iam.clone(),
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
            sqlx::query("SELECT id, key_hash, iam_key_hash, secrets, deleted_at, purge_after FROM public.testing_environments LIMIT 0")
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

    /// Installs the test-only Application credential without changing the IAM root binding.
    ///
    /// # Errors
    ///
    /// Returns a rejected IAM credential or a storage error. The caller must hold
    /// the exclusive environment lease selected by its root key.
    pub async fn configure_iam(
        &self,
        lease: &mut EnvironmentLease,
        secret: SecretString,
    ) -> Result<(), AppError> {
        let iam = Client::builder(self.iam_settings.base_url.as_str())
            .map_err(iam_error)?
            .auto_update(false)
            .timeout(self.iam_settings.request_timeout)
            .environment(lease.iam_key.clone())
            .build()
            .map_err(iam_error)?;
        self.validate_app(&iam, &secret).await?;
        let id = lease.environment.id;
        let mut credentials = self.credentials(&mut lease.guard, id).await?;
        credentials.iam_app_secret = Some(secret.expose_secret().to_owned());
        sqlx::query(
            "UPDATE public.testing_environments SET secrets=$2,version=version+1 WHERE id=$1",
        )
        .bind(id)
        .bind(self.seal(id, &credentials)?)
        .execute(&mut *lease.guard)
        .await?;
        Ok(())
    }

    async fn validate_app(&self, iam: &Client, secret: &SecretString) -> Result<(), AppError> {
        // Invalid tokens answer inactive only after the test app authenticates.
        iam.with_credential(Credential::application(
            &self.iam_settings.app_id,
            secret.expose_secret(),
        ))
        .oauth()
        .introspect(
            &models::TokenIntrospectionRequest {
                token: "oat_remind_configuration_probe".to_owned(),
                token_type_hint: None,
            },
            None,
        )
        .await
        .map_err(iam_error)?;
        Ok(())
    }

    /// One-shot control database migration, used only by remind-migrate.
    ///
    /// # Errors
    ///
    /// Returns schema, checksum, or database errors; runtime processes never call this method.
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

    /// Verifies IAM binding, initializes production tables, and publishes an empty environment.
    ///
    /// # Errors
    ///
    /// Returns invalid input, an invalid IAM test binding, a duplicate active name, or a schema/storage failure.
    pub async fn create(
        &self,
        actor: &Actor,
        input: CreateTestEnvironment,
    ) -> Result<(TestEnvironment, SecretString), AppError> {
        if input.name.trim().is_empty()
            || input.name.len() > 100
            || input.description.as_ref().is_some_and(|s| s.len() > 10000)
        {
            return Err(AppError::Validation);
        }
        let iam_key = EnvironmentKey::new(input.iam_test_key.expose_secret())
            .map_err(|_| AppError::Validation)?;
        let iam = Client::builder(self.iam_settings.base_url.as_str())
            .map_err(iam_error)?
            .auto_update(false)
            .timeout(self.iam_settings.request_timeout)
            .environment(iam_key)
            .build()
            .map_err(iam_error)?;
        let binding = iam.environments().current().await.map_err(iam_error)?;
        if let Some(secret) = &input.iam_app_secret {
            self.validate_app(&iam, secret).await?;
        }
        let id = Uuid::now_v7();
        let key = generate_key();
        let credentials = Credentials {
            key: key.clone(),
            iam_key: input.iam_test_key.expose_secret().to_owned(),
            iam_app_secret: input
                .iam_app_secret
                .map(|value| value.expose_secret().to_owned()),
        };
        let sealed = self.seal(id, &credentials)?;
        let mut transaction = self.control.begin().await?;
        sqlx::query("INSERT INTO public.testing_environments (id,org_id,creator_id,name,description,iam_environment_id,key_hash,secrets,iam_key_hash) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(id).bind(&actor.org_id).bind(Uuid::parse_str(&actor.id).map_err(|_| AppError::Unauthenticated)?)
            .bind(input.name.trim()).bind(input.description).bind(binding.id).bind(hash(&key)).bind(sealed)
            .bind(hash(&credentials.iam_key))
            .execute(&mut *transaction).await?;
        // DDL and metadata publish atomically. Migrations are the same embedded SQL
        // used in production; the test-only quota is added afterward.
        let schema = schema(id);
        sqlx::raw_sql(AssertSqlSafe(format!(
            "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema};"
        )))
        .execute(&mut *transaction)
        .await?;
        migrate_data_schema(&mut transaction).await?;
        sqlx::raw_sql(QUOTA_SQL).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok((self.get(actor, id).await?, SecretString::from(key)))
    }

    /// Organization-scoped listing, including recoverable deleted environments when requested.
    ///
    /// # Errors
    ///
    /// Returns invalid pagination or a database failure.
    pub async fn list(
        &self,
        actor: &Actor,
        deleted: bool,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<TestEnvironment>, AppError> {
        if !(1..=100).contains(&limit) {
            return Err(AppError::Validation);
        }
        Ok(sqlx::query_as("SELECT * FROM public.testing_environments WHERE org_id = $1 AND (COALESCE(purge_after,last_activity_at+interval '45 days') > clock_timestamp()) AND ($2 OR (deleted_at IS NULL AND last_activity_at>clock_timestamp()-interval '15 days')) AND ($3::uuid IS NULL OR id > $3) ORDER BY id LIMIT $4")
            .bind(&actor.org_id).bind(deleted).bind(after).bind(limit).fetch_all(&self.control).await?.into_iter().map(logical_lifecycle).collect())
    }

    /// Reads one owned environment inside the recovery window.
    ///
    /// # Errors
    ///
    /// Returns not found for other organizations or expired environments, or a database error.
    pub async fn get(&self, actor: &Actor, id: Uuid) -> Result<TestEnvironment, AppError> {
        sqlx::query_as("SELECT * FROM public.testing_environments WHERE id = $1 AND org_id = $2 AND (COALESCE(purge_after,last_activity_at+interval '45 days') > clock_timestamp())")
            .bind(id).bind(&actor.org_id).fetch_optional(&self.control).await?.map(logical_lifecycle).ok_or(AppError::NotFound)
    }

    /// Reads the active key for its creator or an org owner/admin.
    ///
    /// # Errors
    ///
    /// Returns forbidden for non-administrators, not found for inactive environments, or a credential-storage error.
    pub async fn key(&self, actor: &Actor, id: Uuid) -> Result<SecretString, AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, false).await?;
        let row = guarded_get(&mut tx, actor, id).await?;
        require_manager(actor, &row)?;
        if row.deleted_at.is_some() {
            return Err(AppError::NotFound);
        }
        let credentials = self.credentials(&mut tx, id).await?;
        tx.commit().await?;
        Ok(SecretString::from(credentials.key))
    }

    /// Rotates or restores a key under exclusive lifecycle admission.
    ///
    /// # Errors
    ///
    /// Returns authorization, expired-window, lifecycle, name-conflict or encryption/storage errors.
    pub async fn rotate(
        &self,
        actor: &Actor,
        id: Uuid,
        restore: bool,
    ) -> Result<SecretString, AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, true).await?;
        let row = guarded_get(&mut tx, actor, id).await?;
        require_manager(actor, &row)?;
        if row.deleted_at.is_some() != restore {
            return Err(AppError::conflict("environment_state_conflict"));
        }
        let mut credentials = self.credentials(&mut tx, id).await?;
        credentials.key = generate_key();
        sqlx::query("UPDATE public.testing_environments SET key_hash=$2,secrets=$3,deleted_at=NULL,purge_after=NULL,last_activity_at=clock_timestamp(),version=version+1 WHERE id=$1")
            .bind(id).bind(hash(&credentials.key)).bind(self.seal(id, &credentials)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(SecretString::from(credentials.key))
    }

    /// Retires an environment immediately while retaining its data for 30 days.
    ///
    /// # Errors
    ///
    /// Returns authorization, not-found or database errors.
    pub async fn delete(&self, actor: &Actor, id: Uuid) -> Result<(), AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, true).await?;
        let row = guarded_get(&mut tx, actor, id).await?;
        require_manager(actor, &row)?;
        retire(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Admits a key-bearing request, rechecking active state after taking the lock.
    ///
    /// # Errors
    ///
    /// Returns unauthorized for malformed, revoked, unknown or inactive keys, or a database error.
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
            return Err(AppError::Unauthenticated);
        }
        let id: Uuid = sqlx::query_scalar(
            "SELECT id FROM public.testing_environments WHERE key_hash=$1 AND deleted_at IS NULL",
        )
        .bind(hash(key.expose_secret()))
        .fetch_optional(&self.control)
        .await?
        .ok_or(AppError::Unauthenticated)?;
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, exclusive).await?;
        let environment: TestEnvironment = sqlx::query_as("SELECT * FROM public.testing_environments WHERE id=$1 AND key_hash=$2 AND deleted_at IS NULL AND last_activity_at > clock_timestamp() - interval '15 days'")
            .bind(id).bind(hash(key.expose_secret())).fetch_optional(&mut *tx).await?.ok_or(AppError::Unauthenticated)?;
        let credentials = self.credentials(&mut tx, id).await?;
        self.lease(environment, credentials, tx).await
    }

    /// Admits a worker without counting its poll as user activity.
    ///
    /// # Errors
    ///
    /// Returns a database or credential-decryption error; inactive environments return None.
    pub async fn enter_worker(&self, id: Uuid) -> Result<Option<EnvironmentLease>, AppError> {
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, false).await?;
        let row: Option<TestEnvironment> = sqlx::query_as("SELECT * FROM public.testing_environments WHERE id=$1 AND deleted_at IS NULL AND last_activity_at > clock_timestamp() - interval '15 days'")
            .bind(id).fetch_optional(&mut *tx).await?;
        match row {
            Some(environment) => {
                let credentials = self.credentials(&mut tx, id).await?;
                Ok(Some(self.lease(environment, credentials, tx).await?))
            }
            None => Ok(None),
        }
    }

    /// Removes all environment data while the caller holds its exclusive lease.
    ///
    /// # Errors
    ///
    /// Returns a database error if the atomic truncate cannot complete.
    pub async fn clean(&self, lease: &mut EnvironmentLease) -> Result<(), AppError> {
        // The production audit trigger is copied verbatim into each replica.
        // Suspend only its truncate guard under the exclusive lifecycle lock,
        // in this transaction; success re-enables it and rollback restores it.
        let schema = schema(lease.environment.id);
        sqlx::raw_sql(AssertSqlSafe(format!("ALTER TABLE {schema}.audit_records DISABLE TRIGGER audit_records_reject_truncate; TRUNCATE {schema}.iam_organization_bindings, {schema}.organization_lifecycle, {schema}.silicon_identities, {schema}.schedules, {schema}.executions, {schema}.deleted_reminders, {schema}.hook_destinations, {schema}.idempotency_records, {schema}.internal_event_receipts, {schema}.audit_records RESTART IDENTITY CASCADE; ALTER TABLE {schema}.audit_records ENABLE TRIGGER audit_records_reject_truncate")))
            .execute(&mut *lease.guard).await?;
        Ok(())
    }

    /// Finds active Remind replicas bound to an authenticated IAM test key.
    ///
    /// # Errors
    ///
    /// Returns unauthorized for malformed keys or a database error.
    pub async fn webhook_environment_ids(&self, key: &str) -> Result<Vec<Uuid>, AppError> {
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(AppError::Unauthenticated);
        }
        Ok(sqlx::query_scalar("SELECT id FROM public.testing_environments WHERE iam_key_hash=$1 AND deleted_at IS NULL ORDER BY id")
            .bind(hash(key)).fetch_all(&self.control).await?)
    }

    /// Bounded active environment page for workers.
    ///
    /// # Errors
    ///
    /// Returns a database error if the bounded page cannot be read.
    pub async fn active_ids(&self, after: Option<Uuid>) -> Result<Vec<Uuid>, AppError> {
        Ok(sqlx::query_scalar("SELECT id FROM public.testing_environments WHERE deleted_at IS NULL AND last_activity_at > clock_timestamp() - interval '15 days' AND ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT 100")
            .bind(after).fetch_all(&self.control).await?)
    }

    /// Applies inactivity retirement and permanent removal in bounded batches.
    ///
    /// # Errors
    ///
    /// Returns database errors while retiring or dropping an environment under its lifecycle lock.
    pub async fn sweep(&self) -> Result<(), AppError> {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM public.testing_environments WHERE (deleted_at IS NULL AND last_activity_at <= clock_timestamp() - interval '15 days') OR purge_after <= clock_timestamp() ORDER BY id LIMIT 100")
            .fetch_all(&self.control).await?;
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

    async fn lease(
        &self,
        environment: TestEnvironment,
        credentials: Credentials,
        guard: Transaction<'static, Postgres>,
    ) -> Result<EnvironmentLease, AppError> {
        let iam_key = EnvironmentKey::new(credentials.iam_key).map_err(iam_error)?;
        let iam = credentials
            .iam_app_secret
            .map(|secret| {
                IamClient::new(&self.iam_settings)
                    .map(|client| {
                        client.in_environment(
                            environment.iam_environment_id,
                            iam_key.clone(),
                            &SecretString::from(secret),
                        )
                    })
                    .map_err(|error| AppError::internal("test_iam", error))
            })
            .transpose()?;
        Ok(EnvironmentLease {
            pool: self.pool(environment.id).await?,
            environment,
            iam,
            iam_key,
            guard,
        })
    }

    async fn pool(&self, id: Uuid) -> Result<PgPool, AppError> {
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get(&id) {
            return Ok(pool.clone());
        }
        // Bounded cache; dropping the last pool handle closes idle connections.
        if pools.len() >= 32
            && let Some(oldest) = pools.keys().min().copied()
        {
            pools.remove(&oldest);
        }
        let options: PgConnectOptions = self.database.url.expose_secret().parse()?;
        let path = schema(id);
        let pool = PgPoolOptions::new().max_connections(4).min_connections(0)
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

fn require_manager(actor: &Actor, environment: &TestEnvironment) -> Result<(), AppError> {
    if actor.id == environment.creator_id.to_string()
        || matches!(actor.org_role.as_deref(), Some("owner" | "admin"))
    {
        Ok(())
    } else {
        Err(AppError::Forbidden)
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
fn iam_error(_error: silicon_iam_client::Error) -> AppError {
    AppError::conflict("iam_test_binding_invalid")
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

// A shared migration source prevents future production changes from silently
// leaving already-created test environments on an older schema.
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
    actor: &Actor,
    id: Uuid,
) -> Result<TestEnvironment, AppError> {
    sqlx::query_as("SELECT * FROM public.testing_environments WHERE id=$1 AND org_id=$2 AND (COALESCE(purge_after,last_activity_at+interval '45 days')>clock_timestamp())")
        .bind(id).bind(&actor.org_id).fetch_optional(&mut **tx).await?.map(logical_lifecycle).ok_or(AppError::NotFound)
}
