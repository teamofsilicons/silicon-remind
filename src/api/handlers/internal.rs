//! Service provisioning and HMAC-authenticated IAM webhook endpoints.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State, rejection},
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::{
    api::{ApiState, models},
    config::RuntimeEnvironment,
    domain::{is_valid_iam_label, silicon_id_belongs_to_org},
    error::AppError,
    infrastructure::{
        crypto::{EncryptedSecret, destination_field_associated_data},
        postgres::{ActorType, AuditContext, NewHookDestination, NewInternalEvent},
        webhook::{destination_url_is_allowed, signing_secret_is_valid},
    },
    request_context,
};

/// Registers or rotates one Silicon's encrypted webhook destination.
///
/// # Errors
///
/// Returns authentication-boundary validation, encryption, or persistence
/// errors without exposing credential material.
pub async fn upsert_hook_destination(
    State(state): State<ApiState>,
    body: Result<Json<models::HookDestinationRequest>, rejection::JsonRejection>,
) -> Result<(StatusCode, Json<models::HookDestinationResponse>), AppError> {
    let Json(request) = body.map_err(|error| map_json_rejection(&error))?;
    save_destination(&state, request, &internal_audit()).await
}

pub(crate) async fn save_destination(
    state: &ApiState,
    request: models::HookDestinationRequest,
    audit: &AuditContext,
) -> Result<(StatusCode, Json<models::HookDestinationResponse>), AppError> {
    validate_iam_org_id(&request.org_id)?;
    validate_global_silicon_id(&request.silicon_id, &request.org_id)?;
    validate_hook_destination(state, &request)?;

    let url_aad =
        destination_field_associated_data(&request.org_id, &request.silicon_id, "endpoint_url");
    let secret_aad =
        destination_field_associated_data(&request.org_id, &request.silicon_id, "signing_secret");
    let encrypted_url = state
        .encryption
        .encrypt(&SecretString::from(request.endpoint_url.as_str()), &url_aad)
        .map_err(|error| AppError::internal("destination_encryption", error))?;
    let encrypted_secret = state
        .encryption
        .encrypt(&request.signing_secret, &secret_aad)
        .map_err(|error| AppError::internal("destination_encryption", error))?;
    if encrypted_url.key_version != encrypted_secret.key_version {
        return Err(AppError::internal(
            "destination_encryption",
            anyhow::anyhow!("key version changed during one request"),
        ));
    }
    let endpoint_url_nonce = nonce(&encrypted_url)?;
    let signing_secret_nonce = nonce(&encrypted_secret)?;
    let destination = NewHookDestination {
        id: Uuid::now_v7(),
        org_id: request.org_id.clone(),
        owner_principal_id: request.principal_id,
        silicon_id: request.silicon_id.clone(),
        endpoint_url_ciphertext: encrypted_url.ciphertext,
        endpoint_url_nonce,
        signing_secret_ciphertext: encrypted_secret.ciphertext,
        signing_secret_nonce,
        encryption_key_version: encrypted_url.key_version,
    };
    let row = state
        .repository
        .upsert_hook_destination(&destination, audit)
        .await?;
    let status = if row.version == 1 {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(models::HookDestinationResponse {
            id: row.id,
            org_id: row.org_id,
            silicon_id: row.silicon_id,
            version: row.version,
            updated_at: row.updated_at,
        }),
    ))
}

/// Disables an obsolete destination without disclosing its credential.
///
/// # Errors
///
/// Returns validation, not-found, or persistence errors.
pub async fn disable_hook_destination(
    State(state): State<ApiState>,
    path: Result<Path<(String, String)>, rejection::PathRejection>,
) -> Result<StatusCode, AppError> {
    let Path((org_id, silicon_id)) = path.map_err(|_| AppError::Validation)?;
    validate_iam_org_id(&org_id)?;
    validate_global_silicon_id(&silicon_id, &org_id)?;
    state
        .repository
        .disable_hook_destination(&org_id, &silicon_id, Utc::now(), &internal_audit())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Verifies and durably applies one Silicon IAM application webhook event.
///
/// # Errors
///
/// Returns authentication, validation, event-id conflict, or persistence
/// errors. Authentication is checked over the exact raw body before parsing.
pub async fn accept_iam_event(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Bytes, rejection::BytesRejection>,
) -> Result<(StatusCode, Json<models::InternalEventAccepted>), AppError> {
    let body = body.map_err(|_| AppError::PayloadTooLarge)?;
    let received_at = Utc::now();
    let verified = state
        .iam_webhook
        .verify(&headers, &body, received_at)
        .map_err(|_| AppError::Unauthenticated)?;
    if verified.is_testing() {
        // The SDK authenticated the raw outer body before this routing hint is read.
        // Never persist or log the IAM root key carried in that envelope.
        let wire: Value = serde_json::from_slice(&body).map_err(|_| AppError::Validation)?;
        let key = wire
            .pointer("/test/testing_key")
            .and_then(Value::as_str)
            .ok_or(AppError::Validation)?;
        let tests = state.tests.as_ref().ok_or(AppError::Unauthenticated)?;
        let ids = tests.webhook_environment_ids(key).await?;
        if ids.is_empty() {
            return Err(AppError::Unauthenticated);
        }
        let mut receipt_id = None;
        for id in ids {
            if let Some(lease) = tests.enter_worker(id).await? {
                verified
                    .verify_testing_environment(&lease.iam_key)
                    .map_err(|_| AppError::Unauthenticated)?;
                let repository =
                    crate::infrastructure::postgres::PostgresRepository::new(lease.pool.clone());
                receipt_id = Some(apply_verified_event(&repository, &verified, received_at).await?);
                lease.finish(false).await?;
            }
        }
        return Ok((
            StatusCode::ACCEPTED,
            Json(models::InternalEventAccepted {
                receipt_id: receipt_id.ok_or(AppError::Unauthenticated)?,
                status: "accepted",
            }),
        ));
    }
    let receipt_id = apply_verified_event(&state.repository, &verified, received_at).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(models::InternalEventAccepted {
            receipt_id,
            status: "accepted",
        }),
    ))
}

async fn apply_verified_event(
    repository: &crate::infrastructure::postgres::PostgresRepository,
    verified: &silicon_iam_client::VerifiedWebhook,
    received_at: chrono::DateTime<Utc>,
) -> Result<Uuid, AppError> {
    let payload = serde_json::to_value(verified.event()).map_err(|_| AppError::Validation)?;
    let event = serde_json::from_value::<models::IamWebhookEvent>(payload.clone())
        .map_err(|_| AppError::Validation)?;
    let payload_hash =
        Sha256::digest(serde_json::to_vec(&payload).map_err(|_| AppError::Validation)?).into();
    if event.data.contains_key("current") {
        let org_id: Option<String> = match verified.event().organization_id {
            Some(id) => {
                sqlx::query_scalar(
                    "SELECT org_id FROM iam_organization_bindings WHERE organization_id=$1",
                )
                .bind(id)
                .fetch_optional(repository.pool())
                .await?
            }
            None => None,
        };
        let current = &event.data["current"];
        let organization = &current["organization"];
        let organization_revoked = organization["status"].as_str() == Some("disabled")
            || organization["authorization"].as_str() == Some("removed");
        let mut principals = Vec::new();
        if let Some(members) = current["members"].as_array() {
            for member in members {
                let resource = &member["resource"];
                if resource["principal_type"].as_str() == Some("silicon")
                    && (resource["status"].as_str() == Some("removed")
                        || member["authorization"].as_str() == Some("removed")
                        || member.pointer("/membership/status").and_then(Value::as_str)
                            == Some("removed"))
                {
                    let id = resource["principal_id"]
                        .as_str()
                        .ok_or(AppError::Validation)?
                        .parse()
                        .map_err(|_| AppError::Validation)?;
                    principals.push(id);
                }
            }
        }
        let new_event = NewInternalEvent {
            id: Uuid::now_v7(),
            source: "silicon-iam".into(),
            event_id: event.event_id.to_string(),
            event_type: event.event_type.as_str().into(),
            org_id,
            subject_id: None,
            payload,
            payload_hash,
            received_at,
        };
        return Ok(repository
            .apply_iam_projection(
                &new_event,
                organization_revoked,
                principals,
                &iam_webhook_audit(),
            )
            .await?
            .id);
    }
    // The earlier minimal IAM projection remains readable for retained in-flight deliveries.
    let (new_event, applies_lifecycle) =
        prepare_iam_event(&event, payload, payload_hash, received_at)?;
    let receipt_id = if applies_lifecycle {
        repository
            .apply_iam_lifecycle_event(&new_event, &iam_webhook_audit())
            .await?
            .receipt
            .id
    } else {
        repository
            .record_processed_internal_event(&new_event, &iam_webhook_audit())
            .await?
            .0
            .id
    };
    Ok(receipt_id)
}

fn validate_hook_destination(
    state: &ApiState,
    request: &models::HookDestinationRequest,
) -> Result<(), AppError> {
    let production = state.environment == RuntimeEnvironment::Production && !state.is_test;
    let signing_secret = request.signing_secret.expose_secret();
    let valid_secret =
        signing_secret.is_empty() || signing_secret_is_valid(&request.signing_secret);
    if !destination_url_is_allowed(&request.endpoint_url, production) || !valid_secret {
        return Err(AppError::Validation);
    }
    Ok(())
}

pub(crate) fn map_json_rejection(rejection: &rejection::JsonRejection) -> AppError {
    if matches!(rejection, rejection::JsonRejection::BytesRejection(_)) {
        AppError::PayloadTooLarge
    } else {
        AppError::Validation
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum MembershipPrincipalType {
    Carbon,
    Silicon,
}

#[derive(Debug, Deserialize)]
struct MembershipRemovedData {
    org_id: String,
    principal_id: Uuid,
    #[serde(alias = "principal_kind")]
    principal_type: MembershipPrincipalType,
}

fn prepare_iam_event(
    event: &models::IamWebhookEvent,
    payload: Value,
    payload_hash: [u8; 32],
    received_at: chrono::DateTime<Utc>,
) -> Result<(NewInternalEvent, bool), AppError> {
    if event.spec_version != "1.0"
        || event.aggregate.version <= 0
        || event.aggregate.aggregate_type.trim().is_empty()
        || event.aggregate.aggregate_type.len() > 255
    {
        return Err(AppError::Validation);
    }

    let mut applies_lifecycle = false;
    let (org_id, subject_id) = if event
        .event_type
        .is(models::IamWebhookEventType::ORGANIZATION_MEMBERSHIP_REMOVED)
    {
        if event.aggregate.aggregate_type != "membership" {
            return Err(AppError::Validation);
        }
        let data =
            serde_json::from_value::<MembershipRemovedData>(Value::Object(event.data.clone()))
                .map_err(|_| AppError::Validation)?;
        validate_iam_org_id(&data.org_id)?;
        applies_lifecycle = data.principal_type == MembershipPrincipalType::Silicon;
        (Some(data.org_id), Some(data.principal_id.to_string()))
    } else {
        let org_id = optional_string_field(&event.data, "org_id")?;
        if let Some(org_id) = org_id.as_deref() {
            validate_iam_org_id(org_id)?;
        }
        let subject_id = optional_uuid_field(&event.data, "principal_id")?;
        if event
            .event_type
            .is(models::IamWebhookEventType::ORGANIZATION_UPDATED)
        {
            applies_lifecycle = matches!(
                optional_string_field(&event.data, "status")?.as_deref(),
                Some("disabled")
            );
        }
        (org_id, subject_id.map(|id| id.to_string()))
    };

    Ok((
        NewInternalEvent {
            id: Uuid::now_v7(),
            source: "silicon-iam".to_owned(),
            event_id: event.event_id.to_string(),
            event_type: event.event_type.as_str().to_owned(),
            org_id,
            subject_id,
            payload,
            payload_hash,
            received_at,
        },
        applies_lifecycle,
    ))
}

fn optional_string_field(
    data: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, AppError> {
    match data.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(AppError::Validation),
    }
}

fn optional_uuid_field(
    data: &serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<Option<Uuid>, AppError> {
    match optional_string_field(data, field)? {
        Some(value) => Uuid::parse_str(&value)
            .map(Some)
            .map_err(|_| AppError::Validation),
        None => Ok(None),
    }
}

fn validate_iam_org_id(value: &str) -> Result<(), AppError> {
    if is_valid_iam_label(value) {
        Ok(())
    } else {
        Err(AppError::Validation)
    }
}

fn validate_global_silicon_id(value: &str, org_id: &str) -> Result<(), AppError> {
    if silicon_id_belongs_to_org(value, org_id) {
        Ok(())
    } else {
        Err(AppError::Validation)
    }
}

fn nonce(encrypted: &EncryptedSecret) -> Result<[u8; 12], AppError> {
    encrypted
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| AppError::internal("destination_encryption", anyhow::anyhow!("bad nonce")))
}

fn internal_audit() -> AuditContext {
    AuditContext {
        actor_type: ActorType::Service,
        actor_id: "internal-api".to_owned(),
        request_id: request_context::current_request_id(),
    }
}

fn iam_webhook_audit() -> AuditContext {
    AuditContext {
        actor_type: ActorType::Application,
        actor_id: "silicon-iam".to_owned(),
        request_id: request_context::current_request_id(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use chrono::{TimeZone as _, Utc};
    use hmac::{Hmac, Mac as _};
    use http::{HeaderMap, HeaderValue};
    use secrecy::SecretString;
    use serde_json::json;
    use sha2::{Digest as _, Sha256};

    use crate::{
        api::models::{IamWebhookEvent, IamWebhookEventType},
        infrastructure::iam_webhook::IamWebhookVerifier,
    };

    use super::{prepare_iam_event, validate_global_silicon_id, validate_iam_org_id};

    fn received_at() -> chrono::DateTime<Utc> {
        match Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0).single() {
            Some(value) => value,
            None => panic!("fixed timestamp must be valid"),
        }
    }

    #[test]
    fn silicon_membership_removal_maps_to_the_atomic_lifecycle_path() -> anyhow::Result<()> {
        let payload = json!({
            "spec_version": "1.0",
            "event_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5ca",
            "event_type": "organization.membership.removed.v1",
            "occurred_at": "2026-08-31T12:00:00Z",
            "aggregate": {
                "id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cb",
                "type": "membership",
                "version": 7
            },
            "data": {
                "org_id": "tos",
                "principal_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cc",
                "principal_type": "silicon"
            }
        });
        let event = serde_json::from_value::<IamWebhookEvent>(payload.clone())?;
        let (event, applies_lifecycle) = prepare_iam_event(
            &event,
            payload,
            Sha256::digest(b"wire bytes").into(),
            received_at(),
        )?;

        assert!(applies_lifecycle);
        assert_eq!(
            event.event_type,
            IamWebhookEventType::ORGANIZATION_MEMBERSHIP_REMOVED
        );
        assert_eq!(event.org_id.as_deref(), Some("tos"));
        assert_eq!(
            event.subject_id.as_deref(),
            Some("0198f74d-7ef7-7c9f-95bf-7d403a61e5cc")
        );
        Ok(())
    }

    #[test]
    fn carbon_membership_removal_is_a_durable_noop() -> anyhow::Result<()> {
        let payload = json!({
            "spec_version": "1.0",
            "event_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5ca",
            "event_type": "organization.membership.removed.v1",
            "occurred_at": "2026-08-31T12:00:00Z",
            "aggregate": {
                "id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cb",
                "type": "membership",
                "version": 7
            },
            "data": {
                "org_id": "tos",
                "principal_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cc",
                "principal_type": "carbon"
            }
        });
        let event = serde_json::from_value::<IamWebhookEvent>(payload.clone())?;
        let (_, applies_lifecycle) = prepare_iam_event(
            &event,
            payload,
            Sha256::digest(b"wire bytes").into(),
            received_at(),
        )?;

        assert!(!applies_lifecycle);
        Ok(())
    }

    #[test]
    fn disabled_organization_maps_to_the_atomic_lifecycle_path() -> anyhow::Result<()> {
        let payload = json!({
            "spec_version": "1.0",
            "event_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5ca",
            "event_type": "organization.updated.v1",
            "occurred_at": "2026-08-31T12:00:00Z",
            "aggregate": {
                "id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cb",
                "type": "organization",
                "version": 8
            },
            "data": {
                "org_id": "tos",
                "status": "disabled"
            }
        });
        let event = serde_json::from_value::<IamWebhookEvent>(payload.clone())?;
        let (event, applies_lifecycle) = prepare_iam_event(
            &event,
            payload,
            Sha256::digest(b"organization wire bytes").into(),
            received_at(),
        )?;

        assert!(applies_lifecycle);
        assert_eq!(event.org_id.as_deref(), Some("tos"));
        assert!(event.subject_id.is_none());
        Ok(())
    }

    #[test]
    fn additive_iam_event_is_prepared_as_a_durable_noop() -> anyhow::Result<()> {
        let payload = json!({
            "spec_version": "1.0",
            "event_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5ca",
            "event_type": "organization.silicon.updated.v1",
            "occurred_at": "2026-08-31T12:00:00Z",
            "aggregate": {
                "id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cb",
                "type": "silicon",
                "version": 9
            },
            "data": {
                "org_id": "tos",
                "principal_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cc"
            }
        });
        let event = serde_json::from_value::<IamWebhookEvent>(payload.clone())?;
        let (event, applies_lifecycle) = prepare_iam_event(
            &event,
            payload,
            Sha256::digest(b"additive event wire bytes").into(),
            received_at(),
        )?;

        assert!(!applies_lifecycle);
        assert_eq!(event.event_type, "organization.silicon.updated.v1");
        assert_eq!(event.org_id.as_deref(), Some("tos"));
        Ok(())
    }

    #[test]
    fn iam_identifier_validation_matches_published_fifty_character_bounds() {
        let org_id = "o".repeat(50);
        let silicon_id = format!("{}:{org_id}", "s".repeat(50));
        assert!(validate_iam_org_id(&org_id).is_ok());
        assert!(validate_global_silicon_id(&silicon_id, &org_id).is_ok());
        assert!(validate_iam_org_id(&"o".repeat(51)).is_err());
    }

    #[test]
    fn normative_iam_sender_fixture_verifies_and_maps() -> anyhow::Result<()> {
        let payload = json!({
            "spec_version": "1.0",
            "event_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5ca",
            "event_type": "organization.membership.removed.v1",
            "occurred_at": "2026-08-31T12:00:00Z",
            "aggregate": {
                "id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cb",
                "type": "membership",
                "version": 7
            },
            "data": {
                "org_id": "tos",
                "principal_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cc",
                "principal_type": "silicon"
            }
        });
        let body = serde_json::to_vec(&payload)?;
        let credential = format!("whs_{}", URL_SAFE_NO_PAD.encode([0x5a_u8; 32]));
        let verifier = IamWebhookVerifier::from_keys(&BTreeMap::from([(
            7,
            SecretString::from(credential.clone()),
        )]))?;
        let timestamp = Utc::now().timestamp().to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(credential.as_bytes())?;
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(&body);
        let signature = format!("v1={}", hex::encode(mac.finalize().into_bytes()));
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-silicon-iam-event-id",
            HeaderValue::from_static("0198f74d-7ef7-7c9f-95bf-7d403a61e5ca"),
        );
        headers.insert(
            "x-silicon-iam-timestamp",
            HeaderValue::from_str(&timestamp)?,
        );
        headers.insert("x-silicon-iam-key-version", HeaderValue::from_static("7"));
        headers.insert(
            "x-silicon-iam-signature",
            HeaderValue::from_str(&signature)?,
        );

        let authentication = verifier.verify(&headers, &body, received_at())?;
        let event = serde_json::from_slice::<IamWebhookEvent>(&body)?;
        assert_eq!(authentication.event_id(), event.event_id);
        let (event, applies_lifecycle) =
            prepare_iam_event(&event, payload, Sha256::digest(&body).into(), received_at())?;
        assert!(applies_lifecycle);
        assert_eq!(event.org_id.as_deref(), Some("tos"));
        Ok(())
    }
}
