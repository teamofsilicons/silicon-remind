//! Owner-managed reminder delivery configuration and organization directory.
use crate::{
    api::{ScopedState, models},
    domain::{Actor, ActorKind},
    error::AppError,
    infrastructure::{
        crypto::{EncryptedSecret, destination_field_associated_data},
        postgres::{ActorType, AuditContext},
    },
    request_context,
};
use axum::{
    Extension, Json,
    extract::{Query, rejection},
    http::StatusCode,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use url::Url;
use uuid::Uuid;

/// Delivery endpoint and its write-only Silicon Hook credential.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationRequest {
    endpoint_url: Url,
    signing_secret: SecretString,
}

/// Configures the caller's destination, deriving ownership exclusively from IAM.
///
/// # Errors
///
/// Returns forbidden for Carbons, validation for invalid Hook endpoints, or encrypted-storage failures.
pub async fn set(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    body: Result<Json<DestinationRequest>, rejection::JsonRejection>,
) -> Result<(StatusCode, Json<models::HookDestinationResponse>), AppError> {
    let silicon_id = silicon(&actor)?;
    let Json(input) = body.map_err(|error| super::internal::map_json_rejection(&error))?;
    let request = models::HookDestinationRequest {
        org_id: actor.org_id.clone(),
        silicon_id: silicon_id.to_owned(),
        principal_id: Uuid::parse_str(&actor.id).map_err(|_| AppError::Unauthenticated)?,
        endpoint_url: input.endpoint_url,
        signing_secret: input.signing_secret,
    };
    super::internal::save_destination(&state, request, &audit(&actor)).await
}

/// Reads the configured endpoint without revealing its signing credential.
///
/// # Errors
///
/// Returns forbidden for Carbons, webhook-not-configured when absent, or a storage/decryption failure.
pub async fn get(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
) -> Result<Json<serde_json::Value>, AppError> {
    let silicon_id = silicon(&actor)?;
    let row = state
        .repository
        .get_hook_destination(&actor.org_id, silicon_id)
        .await?
        .ok_or(AppError::WebhookNotConfigured)?;
    let aad = destination_field_associated_data(&actor.org_id, silicon_id, "endpoint_url");
    let endpoint = state
        .encryption
        .decrypt(
            &EncryptedSecret {
                key_version: row.encryption_key_version,
                nonce: row.endpoint_url_nonce,
                ciphertext: row.endpoint_url_ciphertext,
            },
            &aad,
        )
        .map_err(|e| AppError::internal("destination_decryption", e))?;
    Ok(Json(
        serde_json::json!({"silicon_id":silicon_id,"endpoint_url":endpoint.expose_secret(),"version":row.version,"updated_at":row.updated_at}),
    ))
}

/// Disables the caller's destination until explicitly configured again.
///
/// # Errors
///
/// Returns forbidden for Carbons or a persistence failure.
pub async fn disable(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
) -> Result<StatusCode, AppError> {
    state
        .repository
        .disable_hook_destination(
            &actor.org_id,
            silicon(&actor)?,
            chrono::Utc::now(),
            &audit(&actor),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Cursor options for known Remind Silicons in the caller's organization.
#[derive(Deserialize)]
pub struct SiliconQuery {
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// Lists registered Silicons and reminder counts, scoped to the authenticated org.
///
/// # Errors
///
/// Returns validation for invalid pagination or a database failure.
pub async fn silicons(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    query: Result<Query<SiliconQuery>, rejection::QueryRejection>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(AppError::Validation);
    }
    let rows: Vec<(Uuid,String,i64)> = sqlx::query_as("SELECT i.principal_id,i.silicon_id,(SELECT count(*) FROM schedules s WHERE s.org_id=i.org_id AND s.owner_principal_id=i.principal_id AND (s.purge_after IS NULL OR s.purge_after>clock_timestamp())) FROM silicon_identities i JOIN organization_lifecycle o ON o.org_id=i.org_id WHERE i.org_id=$1 AND i.state='active' AND o.state='active' AND ($2::uuid IS NULL OR i.principal_id>$2) ORDER BY i.principal_id LIMIT $3")
        .bind(&actor.org_id).bind(query.after).bind(limit).fetch_all(state.repository.pool()).await?;
    let next = if rows.len() == usize::try_from(limit).unwrap_or(0) {
        rows.last().map(|row| row.0)
    } else {
        None
    };
    let items: Vec<_> = rows.into_iter().map(|(id,silicon_id,count)| serde_json::json!({"principal_id":id,"silicon_id":silicon_id,"reminder_count":count})).collect();
    Ok(Json(serde_json::json!({"items":items,"next_cursor":next})))
}

fn silicon(actor: &Actor) -> Result<&str, AppError> {
    if actor.kind != ActorKind::Silicon {
        return Err(AppError::Forbidden);
    }
    actor
        .public_id
        .as_deref()
        .filter(|id| crate::domain::silicon_id_belongs_to_org(id, &actor.org_id))
        .ok_or(AppError::Unauthenticated)
}
fn audit(actor: &Actor) -> AuditContext {
    AuditContext {
        actor_type: ActorType::Silicon,
        actor_id: actor.id.clone(),
        request_id: request_context::current_request_id(),
    }
}
