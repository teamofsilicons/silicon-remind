//! Durable Honeycomb control plane. It never authenticates with a sandbox session.
use super::*;
use serde_json::{Value, json};

/// Exact participant request emitted by Honeycomb, including its replay identity.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// Stable operation identifier, reused on retries.
    pub operation_id: Uuid,
    /// Shared sandbox identifier.
    pub environment_id: Uuid,
    /// Owning production organization.
    pub org_id: String,
    /// Configured participant application.
    pub app_id: String,
    /// Monotonic coordinator revision.
    pub environment_revision: i64,
    /// Cleaning generation.
    pub generation: i64,
    /// Environment root key version.
    pub key_version: i64,
    /// Lifecycle action.
    pub action: String,
    /// Root authority, encrypted separately from runtime credentials.
    pub testing_key: String,
    /// Coordinator snapshot. Only its digest is retained.
    #[serde(default)]
    pub snapshot: Value,
    /// Coordinator reason.
    #[serde(default)]
    pub reason: String,
    /// Exact applications selected for retirement.
    #[serde(default)]
    pub retired_apps: Vec<String>,
}

#[derive(FromRow)]
pub(super) struct Fence {
    pub org_id: String,
    pub app_id: String,
    pub environment_revision: i64,
    pub generation: i64,
    pub key_version: i64,
    pub state: String,
    pub operation_id: Uuid,
    pub root_digest: String,
    pub require_iam_clean: bool,
    pub iam_cleaned_before: Option<DateTime<Utc>>,
}

impl Operation {
    fn validate(&self, app: &str) -> Result<(), AppError> {
        if self.app_id != app
            || self.operation_id.is_nil()
            || self.environment_id.is_nil()
            || !crate::domain::is_valid_iam_label(&self.org_id)
            || self.environment_revision < 1
            || self.generation < 1
            || self.key_version < 1
            || self.testing_key.len() != 32
            || !self.testing_key.bytes().all(|b| b.is_ascii_alphanumeric())
            || !matches!(
                self.action.as_str(),
                "prepare"
                    | "import"
                    | "refresh-import"
                    | "rotate-key"
                    | "clean"
                    | "disable"
                    | "restore"
                    | "purge"
                    | "retire-applications"
            )
            || (self.action == "retire-applications"
                && (self.retired_apps.is_empty() || self.retired_apps.iter().any(String::is_empty)))
        {
            return Err(AppError::Validation);
        }
        Ok(())
    }
    fn retires(&self) -> bool {
        self.action == "retire-applications" && self.retired_apps.contains(&self.app_id)
    }
    fn receipt(&self, state: &str) -> Value {
        json!({"operation_id":self.operation_id,"environment_id":self.environment_id,
            "app_id":self.app_id,"environment_revision":self.environment_revision,
            "generation":self.generation,"key_version":self.key_version,"state":state,
            "retired_apps":self.retired_apps})
    }
    fn transition(&self, old: &Fence) -> Result<String, AppError> {
        let reimport =
            old.state == "retired" && matches!(self.action.as_str(), "prepare" | "import");
        if old.org_id != self.org_id
            || old.app_id != self.app_id
            || self.environment_revision <= old.environment_revision
            || old.state == "pending"
            || old.state == "purged"
            || (old.state == "retired" && !reimport && self.action != "purge")
            || self.generation < old.generation
            || self.key_version < old.key_version
            || (self.generation != old.generation && self.action != "clean" && !reimport)
            || (self.key_version != old.key_version && self.action != "rotate-key" && !reimport)
            || (self.action == "clean" && self.generation <= old.generation)
            || (self.action == "rotate-key" && self.key_version <= old.key_version)
            || (self.key_version == old.key_version
                && hex::encode(hash(&self.testing_key)) != old.root_digest)
            || (self.key_version > old.key_version
                && hex::encode(hash(&self.testing_key)) == old.root_digest)
            || (old.state == "disabled"
                && !matches!(
                    self.action.as_str(),
                    "restore" | "disable" | "purge" | "clean" | "rotate-key"
                ))
            || (self.action == "restore" && old.state != "disabled")
        {
            return Err(AppError::conflict("stale_honeycomb_operation"));
        }
        Ok(match self.action.as_str() {
            "disable" => "disabled",
            "purge" => "purged",
            "retire-applications" if self.retires() => "retired",
            "restore" | "prepare" | "import" => "active",
            _ => &old.state,
        }
        .into())
    }
}

impl TestEnvironments {
    /// Fetches a secret-free durable receipt, including after permanent removal.
    /// # Errors
    /// Returns not found for a mismatched organization or operation.
    pub async fn honeycomb_receipt(
        &self,
        org: &str,
        id: Uuid,
        operation: Uuid,
    ) -> Result<Value, AppError> {
        sqlx::query_scalar("SELECT receipt FROM public.honeycomb_operations WHERE org_id=$1 AND environment_id=$2 AND operation_id=$3")
            .bind(org).bind(id).bind(operation).fetch_optional(&self.control).await?.ok_or(AppError::NotFound)
    }

    /// Persists admission before cleanup and atomically publishes data and completion.
    /// # Errors
    /// Rejects altered replays, stale generations, invalid transitions and database errors.
    pub async fn apply_honeycomb(&self, op: &Operation) -> Result<Value, AppError> {
        op.validate(&self.iam_settings.app_id)?;
        let request_hash = Sha256::digest(
            serde_json::to_vec(op).map_err(|e| AppError::internal("honeycomb_hash", e))?,
        )
        .to_vec();
        let final_state = self.claim_honeycomb(op, &request_hash).await?;
        let Some(final_state) = final_state else {
            return self
                .honeycomb_receipt(&op.org_id, op.environment_id, op.operation_id)
                .await;
        };
        let result = self.finish_honeycomb(op, &final_state).await;
        if result.is_err() {
            sqlx::query("UPDATE public.honeycomb_operations SET receipt=jsonb_set(receipt,'{state}','\"failed\"') WHERE operation_id=$1 AND request_hash=$2 AND receipt->>'state'<>'completed'")
                .bind(op.operation_id).bind(request_hash).execute(&self.control).await?;
        }
        result
    }

    async fn claim_honeycomb(
        &self,
        op: &Operation,
        request_hash: &[u8],
    ) -> Result<Option<String>, AppError> {
        let id = op.environment_id;
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, true).await?;
        let replay: Option<(Vec<u8>, Value)> = sqlx::query_as(
            "SELECT request_hash,receipt FROM public.honeycomb_operations WHERE operation_id=$1",
        )
        .bind(op.operation_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((old_hash, receipt)) = replay {
            if old_hash != request_hash {
                return Err(AppError::conflict("honeycomb_idempotency_conflict"));
            }
            if receipt["state"] == "completed" {
                return Ok(None);
            }
            // The intended state is stored independently of the caller's retry.
            let state: String = sqlx::query_scalar("SELECT receipt->>'target_state' FROM public.honeycomb_operations WHERE operation_id=$1")
                .bind(op.operation_id).fetch_one(&mut *tx).await?;
            return Ok(Some(state));
        }
        let prior: Option<Fence> =
            sqlx::query_as("SELECT * FROM public.honeycomb_environments WHERE environment_id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let final_state = if let Some(old) = &prior {
            op.transition(old)?
        } else {
            if !matches!(op.action.as_str(), "prepare" | "import") {
                return Err(AppError::conflict("honeycomb_prepare_required"));
            }
            let org: Option<String> =
                sqlx::query_scalar("SELECT org_id FROM public.testing_environments WHERE id=$1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if org.is_some_and(|org| org != op.org_id) {
                return Err(AppError::Forbidden);
            }
            "active".into()
        };
        let root_secret = self
            .cipher
            .encrypt(&SecretString::from(op.testing_key.clone()), &root_aad(id))
            .map_err(|e| AppError::internal("honeycomb_key", e))?;
        let root_secret = serde_json::to_value(root_secret)
            .map_err(|e| AppError::internal("honeycomb_key", e))?;
        sqlx::query("INSERT INTO public.honeycomb_environments(environment_id,org_id,app_id,environment_revision,generation,key_version,state,operation_id,root_secret,root_digest) VALUES($1,$2,$3,$4,$5,$6,'pending',$7,$8,$9) ON CONFLICT(environment_id) DO UPDATE SET environment_revision=$4,generation=$5,key_version=$6,state='pending',operation_id=$7,root_secret=$8,root_digest=$9")
            .bind(id).bind(&op.org_id).bind(&op.app_id).bind(op.environment_revision).bind(op.generation).bind(op.key_version).bind(op.operation_id).bind(root_secret).bind(hex::encode(hash(&op.testing_key))).execute(&mut *tx).await?;
        if op.action == "clean" || op.retires() {
            sqlx::query("UPDATE public.honeycomb_environments SET last_activity_at=NULL,activity_reported_at=NULL,events_after=clock_timestamp(),require_iam_clean=$2,iam_cleaned_before=(SELECT iam_cleaned_at FROM public.testing_environments WHERE id=$1) WHERE environment_id=$1")
                .bind(id).bind(op.action == "clean").execute(&mut *tx).await?;
        }
        let mut receipt = op.receipt("pending");
        receipt["target_state"] = json!(final_state);
        sqlx::query("INSERT INTO public.honeycomb_operations(operation_id,environment_id,org_id,request_hash,receipt) VALUES($1,$2,$3,$4,$5)")
            .bind(op.operation_id).bind(id).bind(&op.org_id).bind(request_hash).bind(receipt).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(final_state))
    }

    async fn finish_honeycomb(&self, op: &Operation, final_state: &str) -> Result<Value, AppError> {
        let id = op.environment_id;
        let mut tx = self.control.begin().await?;
        lock(&mut tx, id, true).await?;
        let fence = Self::honeycomb_fence(&mut tx, id)
            .await?
            .ok_or(AppError::NotFound)?;
        let receipt = self
            .honeycomb_receipt(&op.org_id, id, op.operation_id)
            .await?;
        if receipt["state"] == "completed" {
            return Ok(receipt);
        }
        if fence.operation_id != op.operation_id || fence.state != "pending" {
            return Err(AppError::conflict("stale_honeycomb_operation"));
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM public.testing_environments WHERE id=$1)",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if op.action == "purge" {
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
        } else if !exists {
            let blank = Credentials {
                key: String::new(),
                iam_key: String::new(),
                iam_app_secret: None,
            };
            sqlx::query("INSERT INTO public.testing_environments(id,org_id,creator_id,name,iam_environment_id,key_hash,secrets,iam_control_version) VALUES($1,$2,'honeycomb',$3,$1,$4,$5,0)")
                .bind(id).bind(&op.org_id).bind(format!("Honeycomb {id}")).bind(hash(&format!("unusable:{id}"))).bind(self.seal(id,&blank)?).execute(&mut *tx).await?;
            sqlx::raw_sql(AssertSqlSafe(format!(
                "CREATE SCHEMA {}; SET LOCAL search_path TO {}",
                schema(id),
                schema(id)
            )))
            .execute(&mut *tx)
            .await?;
            migrate_data_schema(&mut tx).await?;
        } else if op.action == "clean" || op.retires() {
            discovery::clean_schema(&mut tx, id).await?;
        }
        sqlx::query("UPDATE public.honeycomb_environments SET state=$2 WHERE environment_id=$1")
            .bind(id)
            .bind(final_state)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE public.honeycomb_operations SET receipt=$2 WHERE operation_id=$1")
            .bind(op.operation_id)
            .bind(op.receipt("completed"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(op.receipt("completed"))
    }

    pub(super) async fn honeycomb_fence(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<Option<Fence>, AppError> {
        Ok(
            sqlx::query_as("SELECT * FROM public.honeycomb_environments WHERE environment_id=$1")
                .bind(id)
                .fetch_optional(&mut **tx)
                .await?,
        )
    }
}
fn root_aad(id: Uuid) -> Vec<u8> {
    format!("honeycomb-root:{id}").into_bytes()
}

impl TestEnvironments {
    /// Rechecks live IAM state immediately before a test dispatch, including retries.
    /// The caller retains the shared environment lease throughout the outbound request.
    /// # Errors
    /// Rejects revoked credentials or any change since worker admission.
    pub async fn validate_dispatch(&self, expected: &TestEnvironment) -> Result<(), AppError> {
        let mut tx = self.control.begin().await?;
        let credentials = self.credentials(&mut tx, expected.id).await?;
        let fence = Self::honeycomb_fence(&mut tx, expected.id).await?;
        if fence.as_ref().is_some_and(|f| f.state != "active") {
            return Err(AppError::Unauthenticated);
        }
        if let Some(version) = expected.iam_control_version {
            let secret = credentials
                .iam_app_secret
                .ok_or(AppError::Unauthenticated)?;
            let (_, current) = IamClient::discover(&self.iam_settings, &SecretString::from(secret))
                .await
                .map_err(|_| AppError::DependencyUnavailable { dependency: "iam" })?;
            let meta = current.environment.ok_or(AppError::Unauthenticated)?;
            if current.environment_id != expected.id
                || meta.version != version
                || fence.as_ref().is_some_and(|f| {
                    meta.key_generation != f.key_version
                        || current.webhook_key_digest.as_deref() != Some(f.root_digest.as_str())
                })
            {
                return Err(AppError::Unauthenticated);
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Sends retryable activity reports for Honeycomb retention decisions.
    /// # Errors
    /// Leaves unsent activity durable when Honeycomb is unavailable.
    pub async fn report_honeycomb_activity(&self, origin: &url::Url) -> Result<(), AppError> {
        let unavailable = || AppError::DependencyUnavailable {
            dependency: "honeycomb",
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| unavailable())?;
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT environment_id FROM public.honeycomb_environments WHERE state='active' AND last_activity_at IS NOT NULL AND (activity_reported_at IS NULL OR activity_reported_at<last_activity_at) ORDER BY environment_id LIMIT 100")
            .fetch_all(&self.control).await?;
        let mut failed = false;
        for id in ids {
            let mut tx = self.control.begin().await?;
            lock(&mut tx, id, false).await?;
            let row: Option<(i64,i64,Value,DateTime<Utc>)> = sqlx::query_as("SELECT generation,key_version,root_secret,last_activity_at FROM public.honeycomb_environments WHERE environment_id=$1 AND state='active' AND app_id=$2 AND last_activity_at IS NOT NULL AND (activity_reported_at IS NULL OR activity_reported_at<last_activity_at)")
                .bind(id).bind(&self.iam_settings.app_id).fetch_optional(&mut *tx).await?;
            let Some((generation, key_version, sealed, at)) = row else {
                continue;
            };
            let sealed: EncryptedSecret = serde_json::from_value(sealed)
                .map_err(|e| AppError::internal("honeycomb_key", e))?;
            let key = self
                .cipher
                .decrypt(&sealed, &root_aad(id))
                .map_err(|e| AppError::internal("honeycomb_key", e))?;
            let mut endpoint = origin.join("api/v1/").map_err(|_| unavailable())?;
            endpoint
                .path_segments_mut()
                .map_err(|()| unavailable())?
                .pop_if_empty()
                .extend([
                    "environments",
                    &id.to_string(),
                    "apps",
                    &self.iam_settings.app_id,
                    "activity",
                ]);
            let response = client
                .post(endpoint)
                .header("X-Testing-Environment-Key", key.expose_secret())
                .header(
                    "Idempotency-Key",
                    format!(
                        "remind:{id}:{generation}:{key_version}:{}",
                        at.timestamp_micros()
                    ),
                )
                .json(&json!({"generation":generation,"key_version":key_version}))
                .send()
                .await;
            if response.is_ok_and(|r| r.status().is_success()) {
                sqlx::query("UPDATE public.honeycomb_environments SET activity_reported_at=GREATEST(activity_reported_at,$2) WHERE environment_id=$1 AND generation=$3 AND key_version=$4")
                    .bind(id).bind(at).bind(generation).bind(key_version).execute(&mut *tx).await?;
            } else {
                failed = true;
            }
            tx.commit().await?;
        }
        if failed { Err(unavailable()) } else { Ok(()) }
    }
}

impl TestEnvironments {
    /// Rejects delayed signed events from before a cleanup without recreating receipts.
    /// # Errors
    /// Returns storage failures; callers must already hold an environment lease.
    pub async fn webhook_is_current(
        &self,
        id: Uuid,
        occurred_at: DateTime<Utc>,
    ) -> Result<bool, AppError> {
        Ok(sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM public.honeycomb_environments WHERE environment_id=$1 AND (state<>'active' OR events_after >= $2)) AND NOT EXISTS(SELECT 1 FROM public.testing_environments WHERE id=$1 AND iam_cleaned_at >= $2)")
            .bind(id).bind(occurred_at).fetch_one(&self.control).await?)
    }
}
