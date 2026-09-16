//! IAM-owned sandbox discovery and lifecycle synchronization.
use super::*;
type PriorLifecycle = (Option<DateTime<Utc>>, Option<i64>, Option<DateTime<Utc>>);

impl TestEnvironments {
    pub(super) async fn discover(
        &self,
        secret: &SecretString,
    ) -> Result<EnvironmentLease, AppError> {
        let (iam, current) = IamClient::discover(&self.iam_settings, secret)
            .await
            .map_err(|error| match error {
                super::super::iam::IamError::Unauthenticated => AppError::Unauthenticated,
                super::super::iam::IamError::Unavailable(_) => {
                    AppError::DependencyUnavailable { dependency: "iam" }
                }
            })?;
        let meta = current
            .environment
            .as_ref()
            .ok_or(AppError::Unauthenticated)?;
        let id = current.environment_id;
        let cleaned = meta
            .cleaned_at
            .map(|v| {
                DateTime::from_timestamp(v.unix_timestamp(), v.nanosecond())
                    .ok_or(AppError::Unauthenticated)
            })
            .transpose()?;
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, true).await?;
        let prior: Option<PriorLifecycle> = sqlx::query_as("SELECT deleted_at,iam_control_version,iam_cleaned_at FROM public.testing_environments WHERE id=$1")
            .bind(id).fetch_optional(&mut *tx).await?;
        if prior.as_ref().is_some_and(|(deleted, version, _)| {
            deleted.is_some() || version.is_some_and(|v| v > meta.version)
        }) {
            return Err(AppError::Unauthenticated);
        }
        let reset = prior.as_ref().is_some_and(|(_, _, old)| *old != cleaned);
        let credentials = Credentials {
            key: secret.expose_secret().to_owned(),
            iam_key: String::new(),
            iam_app_secret: Some(secret.expose_secret().to_owned()),
        };
        sqlx::query("INSERT INTO public.testing_environments(id,org_id,creator_id,name,description,iam_environment_id,key_hash,secrets,iam_control_version,iam_cleaned_at,webhook_key_digest) VALUES($1,$2,$3,$4,$5,$1,$6,$7,$8,$9,$10) ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name,description=EXCLUDED.description,key_hash=EXCLUDED.key_hash,secrets=EXCLUDED.secrets,iam_control_version=EXCLUDED.iam_control_version,iam_cleaned_at=EXCLUDED.iam_cleaned_at,webhook_key_digest=EXCLUDED.webhook_key_digest,version=CASE WHEN testing_environments.iam_control_version IS DISTINCT FROM EXCLUDED.iam_control_version THEN testing_environments.version+1 ELSE testing_environments.version END")
            .bind(id).bind(&meta.org_id).bind(&meta.creator_id).bind(&meta.name).bind(&meta.description)
            .bind(hash(secret.expose_secret())).bind(self.seal(id,&credentials)?).bind(meta.version).bind(cleaned).bind(&current.webhook_key_digest).execute(&mut *tx).await?;
        if prior.is_none() {
            sqlx::raw_sql(AssertSqlSafe(format!(
                "CREATE SCHEMA {}; SET LOCAL search_path TO {}",
                schema(id),
                schema(id)
            )))
            .execute(&mut *tx)
            .await?;
            migrate_data_schema(&mut tx).await?;
        }
        let environment = sqlx::query_as("SELECT * FROM public.testing_environments WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        // Publish new schema before opening a separate pool. The lifecycle fence
        // is reacquired and revision-checked below before any data access.
        if reset {
            clean_schema(&mut tx, id).await?;
        }
        tx.commit().await?;
        let mut guard = self.control.begin().await?;
        lock(&mut guard, id, false).await?;
        let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.testing_environments WHERE id=$1 AND deleted_at IS NULL AND iam_control_version=$2 AND key_hash=$3)")
            .bind(id).bind(meta.version).bind(hash(secret.expose_secret())).fetch_one(&mut *guard).await?;
        if !valid {
            return Err(AppError::Unauthenticated);
        }
        Ok(EnvironmentLease {
            environment,
            pool: self.pool(id).await?,
            iam: Some(iam),
            iam_key: None,
            webhook_key_digest: current.webhook_key_digest,
            guard,
        })
    }
}

pub(super) async fn clean_schema(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<(), AppError> {
    let schema = schema(id);
    sqlx::raw_sql(AssertSqlSafe(format!("ALTER TABLE {schema}.audit_records DISABLE TRIGGER audit_records_reject_truncate; TRUNCATE {schema}.iam_organization_bindings, {schema}.organization_lifecycle, {schema}.silicon_identities, {schema}.schedules, {schema}.executions, {schema}.deleted_reminders, {schema}.hook_destinations, {schema}.idempotency_records, {schema}.internal_event_receipts, {schema}.telemetry_events, {schema}.bug_reports, {schema}.audit_records RESTART IDENTITY CASCADE; ALTER TABLE {schema}.audit_records ENABLE TRIGGER audit_records_reject_truncate")))
        .execute(&mut **tx).await?;
    sqlx::query(AssertSqlSafe(format!(
        "UPDATE {schema}.api_contract_versions SET request_count=0,last_requested_at=NULL"
    )))
    .execute(&mut **tx)
    .await?;
    Ok(())
}
