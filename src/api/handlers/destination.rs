//! Webhook subscriptions: where a Silicon's reminders are delivered.
//!
//! Only the Silicon manages its subscriptions. Its custodian may list them
//! (read-only). Endpoint URLs and signing secrets are stored encrypted; the
//! signing secret is never returned.

use axum::{
    Extension, Json,
    extract::{Path, Query, rejection},
    http::StatusCode,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use url::Url;
use uuid::Uuid;

use crate::{
    api::{ScopedState, models},
    application::schedules::{audit_context, require_writing_silicon},
    config::RuntimeEnvironment,
    domain::{AccountRef, Actor, ReadScope, Relation, is_valid_global_silicon_id},
    error::AppError,
    infrastructure::{
        crypto::{
            EncryptedSecret, destination_field_associated_data_v2,
            stored_destination_associated_data,
        },
        postgres::{HookDestinationRow, NewHookDestination},
        webhook::{destination_url_is_allowed, signing_secret_is_valid},
    },
};

/// Endpoint and optional signing secret for a new subscription.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationRequest {
    endpoint_url: Url,
    #[serde(default)]
    signing_secret: SecretString,
}

/// `PUT /api/v2/webhook` (kept for older clients) and `POST /api/v2/webhooks`:
/// adds one subscription for the calling Silicon.
///
/// # Errors
///
/// Returns forbidden for Carbons and proofs, validation for a disallowed URL or
/// secret, or encryption/storage failures.
pub async fn set(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    body: Result<Json<DestinationRequest>, rejection::JsonRejection>,
) -> Result<(StatusCode, Json<models::HookDestinationResponse>), AppError> {
    require_writing_silicon(&actor)?;
    let Json(input) = body.map_err(|error| super::map_json_rejection(&error))?;
    let production = state.environment == RuntimeEnvironment::Production && !state.is_test;
    let secret = input.signing_secret.expose_secret();
    if !destination_url_is_allowed(&input.endpoint_url, production) {
        return Err(AppError::invalid(
            "endpoint_url_not_allowed",
            "The endpoint URL must be an absolute https URL (http only for this machine outside production), with no credentials.",
        ));
    }
    if !(secret.is_empty() || signing_secret_is_valid(&input.signing_secret)) {
        return Err(AppError::invalid(
            "signing_secret_invalid",
            "The signing secret must be printable text of a bounded length, or omitted.",
        ));
    }
    if !is_valid_global_silicon_id(&actor.public_id) {
        return Err(AppError::described(
            StatusCode::CONFLICT,
            "silicon_id_unknown",
            "Remind does not know this Silicon's si: id yet. Retry in a minute.",
        ));
    }
    let id = Uuid::now_v7();
    let url_aad = destination_field_associated_data_v2(id, actor.storage_key, "endpoint_url");
    let secret_aad = destination_field_associated_data_v2(id, actor.storage_key, "signing_secret");
    let encrypted_url = state
        .encryption
        .encrypt(&SecretString::from(input.endpoint_url.as_str()), &url_aad)
        .map_err(|error| AppError::internal("destination_encryption", error))?;
    let encrypted_secret = state
        .encryption
        .encrypt(&input.signing_secret, &secret_aad)
        .map_err(|error| AppError::internal("destination_encryption", error))?;
    if encrypted_url.key_version != encrypted_secret.key_version {
        return Err(AppError::internal(
            "destination_encryption",
            anyhow::anyhow!("key version changed during one request"),
        ));
    }
    let row = state
        .repository
        .upsert_hook_destination(
            &NewHookDestination {
                id,
                owner_key: actor.storage_key,
                silicon_id: actor.public_id.clone(),
                endpoint_url_nonce: nonce(&encrypted_url)?,
                endpoint_url_ciphertext: encrypted_url.ciphertext,
                signing_secret_nonce: nonce(&encrypted_secret)?,
                signing_secret_ciphertext: encrypted_secret.ciphertext,
                encryption_key_version: encrypted_url.key_version,
            },
            &audit_context(&actor),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(models::HookDestinationResponse {
            id: row.id,
            silicon_id: actor.public_id.clone(),
            silicon_uuid: actor.uuid.clone(),
            version: row.version,
            updated_at: row.updated_at,
        }),
    ))
}

/// `POST /api/v2/webhooks`: adds one subscription (up to 20 may be active).
///
/// # Errors
///
/// Same as [`set`].
pub async fn subscribe(
    state: ScopedState,
    actor: Extension<Actor>,
    body: Result<Json<DestinationRequest>, rejection::JsonRejection>,
) -> Result<(StatusCode, Json<models::HookDestinationResponse>), AppError> {
    set(state, actor, body).await
}

/// `GET /api/v2/webhook` (kept for older clients): the calling Silicon's first
/// subscription, without its signing secret.
///
/// # Errors
///
/// Returns forbidden for Carbons, webhook-not-configured when there is none,
/// or a storage/decryption failure.
pub async fn get(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_silicon_reader(&actor)?;
    let row = state
        .repository
        .get_hook_destination(&actor.own_keys)
        .await?
        .ok_or(AppError::WebhookNotConfigured)?;
    let endpoint = decrypt_endpoint(&state, &row)?;
    Ok(Json(serde_json::json!({
        "id": row.id,
        "silicon_id": actor.public_id,
        "endpoint_url": endpoint,
        "version": row.version,
        "updated_at": row.updated_at,
    })))
}

/// Which Silicon's subscriptions to list.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionQuery {
    silicon_id: Option<String>,
}

/// `GET /api/v2/webhooks`: a Silicon's own subscriptions, or for a Carbon the
/// subscriptions of the Silicons it looks after (`silicon_id` narrows it).
///
/// # Errors
///
/// Returns validation, decryption, or persistence errors.
pub async fn list(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    query: Result<Query<SubscriptionQuery>, rejection::QueryRejection>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Query(query) = query.map_err(|_| AppError::Validation)?;
    let mut owners: Vec<AccountRef> = if actor.is_silicon() {
        vec![actor.account()]
    } else {
        actor
            .visible
            .iter()
            .filter(|owner| owner.relation == Relation::Custodian)
            .map(|owner| owner.account.clone())
            .collect()
    };
    if let Some(silicon_id) = query.silicon_id.as_deref() {
        owners.retain(|owner| owner.id == silicon_id || owner.uuid == silicon_id);
        if owners.is_empty() {
            return Err(AppError::forbidden(
                "subscriptions_not_visible",
                format!("Only {silicon_id} and its custodian can see its webhook subscriptions."),
            ));
        }
    }
    let mut items = Vec::new();
    for owner in owners {
        let keys = owner_keys(&state, &actor, &owner.uuid).await?;
        for row in state.repository.list_hook_destinations(&keys).await? {
            items.push(models::WebhookSubscriptionResponse {
                id: row.id,
                silicon_id: if owner.id.is_empty() {
                    row.silicon_id.clone()
                } else {
                    owner.id.clone()
                },
                owner: Some(owner.clone()),
                endpoint_url: decrypt_endpoint(&state, &row)?,
                version: row.version,
                updated_at: row.updated_at,
            });
        }
    }
    Ok(Json(serde_json::json!({ "items": items })))
}

/// `DELETE /api/v2/webhooks/{id}`: ends one of the calling Silicon's subscriptions.
///
/// # Errors
///
/// Returns validation, authorization, not-found, or persistence errors.
pub async fn unsubscribe(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
    path: Result<Path<Uuid>, rejection::PathRejection>,
) -> Result<StatusCode, AppError> {
    require_writing_silicon(&actor)?;
    let Path(subscription_id) = path.map_err(|_| AppError::Validation)?;
    state
        .repository
        .disable_hook_destination_by_id(
            &actor.own_keys,
            subscription_id,
            chrono::Utc::now(),
            &audit_context(&actor),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v2/webhook` (kept for older clients): ends every subscription
/// of the calling Silicon.
///
/// # Errors
///
/// Returns forbidden for Carbons or a persistence failure.
pub async fn disable(
    ScopedState(state): ScopedState,
    Extension(actor): Extension<Actor>,
) -> Result<StatusCode, AppError> {
    require_writing_silicon(&actor)?;
    state
        .repository
        .disable_hook_destination(&actor.own_keys, chrono::Utc::now(), &audit_context(&actor))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn require_silicon_reader(actor: &Actor) -> Result<(), AppError> {
    if actor.is_silicon() {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "silicon_only",
            "Only a Silicon has its own webhook subscriptions; as a custodian list your Silicons' with GET /api/v2/webhooks.",
        ))
    }
}

/// Storage keys of an account: from the caller's read scope when it maps
/// them, else from the identity store (test environments read everything).
async fn owner_keys(
    state: &crate::api::ApiState,
    actor: &Actor,
    account_uuid: &str,
) -> Result<Vec<Uuid>, AppError> {
    if account_uuid == actor.uuid {
        return Ok(actor.own_keys.clone());
    }
    if let ReadScope::Owners(owners) = &actor.read {
        let keys: Vec<Uuid> = owners
            .iter()
            .filter(|(_, owner)| owner.account.uuid == account_uuid)
            .map(|(key, _)| *key)
            .collect();
        if !keys.is_empty() {
            return Ok(keys);
        }
    }
    state.identity.keys_of(account_uuid).await
}

fn decrypt_endpoint(
    state: &crate::api::ApiState,
    row: &HookDestinationRow,
) -> Result<String, AppError> {
    let aad = stored_destination_associated_data(
        row.aad_version,
        row.org_id.as_deref(),
        &row.silicon_id,
        row.id,
        row.owner_principal_id,
        "endpoint_url",
    )
    .map_err(|error| AppError::internal("destination_decryption", error))?;
    let endpoint = state
        .encryption
        .decrypt(
            &EncryptedSecret {
                key_version: row.encryption_key_version,
                nonce: row.endpoint_url_nonce.clone(),
                ciphertext: row.endpoint_url_ciphertext.clone(),
            },
            &aad,
        )
        .map_err(|error| AppError::internal("destination_decryption", error))?;
    Ok(endpoint.expose_secret().to_owned())
}

fn nonce(encrypted: &EncryptedSecret) -> Result<[u8; 12], AppError> {
    encrypted
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| AppError::internal("destination_encryption", anyhow::anyhow!("bad nonce")))
}
