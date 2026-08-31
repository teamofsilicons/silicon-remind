//! Fail-closed Silicon IAM token-introspection adapter.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::domain::{Actor, ActorKind};

/// IAM authentication/introspection failure.
#[derive(Debug, Error)]
pub enum IamError {
    /// Token is absent, inactive, expired, malformed, or outside the requested org.
    #[error("IAM token is not authorized for the requested organization")]
    Unauthenticated,
    /// IAM could not provide a trustworthy authorization decision.
    #[error("IAM introspection is temporarily unavailable")]
    Unavailable(#[source] anyhow::Error),
}

/// Authenticated client for IAM's opaque-token introspection endpoint.
#[derive(Clone, Debug)]
pub struct IamClient {
    client: reqwest::Client,
    introspection_url: Url,
    authorization: HeaderValue,
    max_response_bytes: usize,
}

impl IamClient {
    /// Builds an IAM introspection client using application Basic credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when credentials cannot be represented as an HTTP
    /// header or the hardened HTTP client cannot be constructed.
    pub fn new(
        introspection_url: Url,
        app_id: &str,
        app_secret: &SecretString,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> anyhow::Result<Self> {
        let credentials = format!("{app_id}:{}", app_secret.expose_secret());
        let encoded = STANDARD.encode(credentials.as_bytes());
        let mut authorization = HeaderValue::from_str(&format!("Basic {encoded}"))?;
        authorization.set_sensitive(true);

        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(false)
            .user_agent(concat!("silicon-remind/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            introspection_url,
            authorization,
            max_response_bytes,
        })
    }

    /// Introspects a bearer token and binds the result to one organization.
    ///
    /// # Errors
    ///
    /// Returns [`IamError::Unauthenticated`] unless IAM positively confirms an
    /// active Carbon or Silicon with active membership in `requested_org_id`.
    pub async fn authenticate(
        &self,
        bearer_token: &SecretString,
        requested_org_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Actor, IamError> {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, self.authorization.clone());
        let organization =
            HeaderValue::from_str(requested_org_id).map_err(|_| IamError::Unauthenticated)?;
        headers.insert("x-org-id", organization);
        let response = self
            .client
            .post(self.introspection_url.clone())
            .headers(headers)
            .form(&[("token", bearer_token.expose_secret())])
            .send()
            .await
            .map_err(|error| IamError::Unavailable(error.into()))?;

        let status = response.status();
        if status != StatusCode::OK {
            if status.is_server_error()
                || status == StatusCode::TOO_MANY_REQUESTS
                || status == StatusCode::REQUEST_TIMEOUT
            {
                return Err(IamError::Unavailable(anyhow::anyhow!(
                    "IAM introspection returned HTTP {}",
                    status.as_u16()
                )));
            }
            return Err(IamError::Unauthenticated);
        }

        let body = read_bounded(response, self.max_response_bytes).await?;
        let response: IntrospectionResponse =
            serde_json::from_slice(&body).map_err(|error| IamError::Unavailable(error.into()))?;
        response.into_actor(requested_org_id, now)
    }
}

#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    #[serde(default)]
    active: bool,
    actor: Option<IntrospectionActor>,
    #[serde(default)]
    principal_id: Option<String>,
    #[serde(default)]
    actor_type: Option<String>,
    #[serde(default, alias = "organization_id")]
    org_id: Option<String>,
    #[serde(default, alias = "organization_ids")]
    org_ids: Vec<String>,
    #[serde(default)]
    organizations: Vec<OrganizationMembership>,
    #[serde(default)]
    memberships: Vec<OrganizationMembership>,
    #[serde(default, alias = "exp")]
    expires_at: Option<i64>,
}

impl IntrospectionResponse {
    fn into_actor(self, requested_org_id: &str, now: DateTime<Utc>) -> Result<Actor, IamError> {
        let expires_at = self.expires_at.ok_or(IamError::Unauthenticated)?;
        if !self.active || expires_at <= now.timestamp() || !self.has_active_org(requested_org_id) {
            return Err(IamError::Unauthenticated);
        }
        let (actor_type, actor_id) = match (self.actor, self.actor_type, self.principal_id) {
            (Some(actor), None, None) => (actor.kind, actor.id),
            (None, Some(actor_type), Some(principal_id)) => (actor_type, principal_id),
            _ => return Err(IamError::Unauthenticated),
        };
        let kind = match actor_type.as_str() {
            "carbon" => ActorKind::Carbon,
            "silicon" => ActorKind::Silicon,
            _ => return Err(IamError::Unauthenticated),
        };
        if Uuid::parse_str(&actor_id).is_err() {
            return Err(IamError::Unauthenticated);
        }
        Ok(Actor {
            kind,
            id: actor_id,
            org_id: requested_org_id.to_owned(),
        })
    }

    fn has_active_org(&self, requested_org_id: &str) -> bool {
        let source_count = usize::from(self.org_id.is_some())
            + usize::from(!self.org_ids.is_empty())
            + usize::from(!self.organizations.is_empty())
            + usize::from(!self.memberships.is_empty());
        if source_count != 1 {
            return false;
        }
        if let Some(org_id) = self.org_id.as_deref() {
            return org_id == requested_org_id;
        }
        if !self.org_ids.is_empty() {
            return self
                .org_ids
                .iter()
                .filter(|org_id| org_id.as_str() == requested_org_id)
                .count()
                == 1;
        }
        let memberships = if self.organizations.is_empty() {
            &self.memberships
        } else {
            &self.organizations
        };
        let mut matching = memberships
            .iter()
            .filter(|membership| membership.org_id() == requested_org_id);
        matching
            .next()
            .is_some_and(OrganizationMembership::is_active)
            && matching.next().is_none()
    }
}

#[derive(Debug, Deserialize)]
struct IntrospectionActor {
    #[serde(rename = "type", alias = "kind", alias = "actor_type")]
    kind: String,
    #[serde(alias = "actor_id")]
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OrganizationMembership {
    Id(String),
    Record {
        #[serde(alias = "organization_id")]
        org_id: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        active: Option<bool>,
    },
}

impl OrganizationMembership {
    fn org_id(&self) -> &str {
        match self {
            Self::Id(org_id) | Self::Record { org_id, .. } => org_id,
        }
    }

    fn is_active(&self) -> bool {
        match self {
            Self::Id(_) => true,
            Self::Record { status, active, .. } => {
                active.unwrap_or(true)
                    && status
                        .as_deref()
                        .is_none_or(|status| matches!(status, "active" | "member"))
            }
        }
    }
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, IamError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(IamError::Unavailable(anyhow::anyhow!(
            "IAM introspection response exceeded the size limit"
        )));
    }
    let mut body = Vec::with_capacity(maximum.min(8_192));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| IamError::Unavailable(error.into()))?
    {
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(IamError::Unavailable(anyhow::anyhow!(
                "IAM introspection response exceeded the size limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::{IamError, IntrospectionResponse};

    fn now() -> chrono::DateTime<Utc> {
        match Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0).single() {
            Some(value) => value,
            None => panic!("fixed test timestamp must be valid"),
        }
    }

    #[test]
    fn accepts_active_membership_object() {
        let parsed = serde_json::from_value::<IntrospectionResponse>(serde_json::json!({
            "active": true,
            "actor": {
                "type": "silicon",
                "id": "0198e7d8-69bb-7d38-9ee1-94e7c143f89a"
            },
            "memberships": [{"org_id": "tos", "status": "active"}],
            "exp": 2_000_000_000
        }));
        assert!(parsed.is_ok());
        let Ok(parsed) = parsed else {
            return;
        };
        let actor = parsed.into_actor("tos", now());
        assert!(actor.is_ok());
    }

    #[test]
    fn accepts_the_published_top_level_introspection_shape() {
        let parsed = serde_json::from_value::<IntrospectionResponse>(serde_json::json!({
            "active": true,
            "principal_id": "0198e7d8-69bb-7d38-9ee1-94e7c143f89a",
            "actor_type": "silicon",
            "org_id": "tos",
            "expires_at": 2_000_000_000
        }));
        assert!(parsed.is_ok());
        let Ok(parsed) = parsed else {
            return;
        };
        let actor = parsed.into_actor("tos", now());
        assert!(actor.is_ok());
        let Ok(actor) = actor else {
            return;
        };
        assert_eq!(actor.id, "0198e7d8-69bb-7d38-9ee1-94e7c143f89a");
    }

    #[test]
    fn rejects_unknown_or_inactive_membership() {
        let parsed = serde_json::from_value::<IntrospectionResponse>(serde_json::json!({
            "active": true,
            "actor": {"type": "carbon", "id": "carbon-a"},
            "organizations": [{"org_id": "tos", "active": false}]
        }));
        assert!(parsed.is_ok());
        let Ok(parsed) = parsed else {
            return;
        };
        assert!(matches!(
            parsed.into_actor("tos", now()),
            Err(IamError::Unauthenticated)
        ));
    }

    #[test]
    fn rejects_application_actor() {
        let parsed = serde_json::from_value::<IntrospectionResponse>(serde_json::json!({
            "active": true,
            "actor": {"type": "application", "id": "app"},
            "org_id": "tos"
        }));
        assert!(parsed.is_ok());
        let Ok(parsed) = parsed else {
            return;
        };
        assert!(matches!(
            parsed.into_actor("tos", now()),
            Err(IamError::Unauthenticated)
        ));
    }

    #[test]
    fn rejects_a_public_handle_where_iam_must_return_a_principal_uuid() {
        let parsed = serde_json::from_value::<IntrospectionResponse>(serde_json::json!({
            "active": true,
            "actor": {"type": "silicon", "id": "assistant:tos"},
            "org_id": "tos"
        }));
        assert!(parsed.is_ok());
        let Ok(parsed) = parsed else {
            return;
        };
        assert!(matches!(
            parsed.into_actor("tos", now()),
            Err(IamError::Unauthenticated)
        ));
    }

    #[test]
    fn rejects_missing_expiry_or_ambiguous_compatibility_fields() {
        for value in [
            serde_json::json!({
                "active": true,
                "principal_id": "0198e7d8-69bb-7d38-9ee1-94e7c143f89a",
                "actor_type": "silicon",
                "org_id": "tos"
            }),
            serde_json::json!({
                "active": true,
                "principal_id": "0198e7d8-69bb-7d38-9ee1-94e7c143f89a",
                "actor_type": "silicon",
                "actor": {
                    "type": "silicon",
                    "id": "0198e7d8-69bb-7d38-9ee1-94e7c143f89a"
                },
                "org_id": "tos",
                "expires_at": 2_000_000_000
            }),
            serde_json::json!({
                "active": true,
                "principal_id": "0198e7d8-69bb-7d38-9ee1-94e7c143f89a",
                "actor_type": "silicon",
                "org_id": "tos",
                "memberships": ["tos"],
                "expires_at": 2_000_000_000
            }),
        ] {
            let parsed = serde_json::from_value::<IntrospectionResponse>(value);
            assert!(parsed.is_ok());
            let Ok(parsed) = parsed else {
                continue;
            };
            assert!(matches!(
                parsed.into_actor("tos", now()),
                Err(IamError::Unauthenticated)
            ));
        }
    }
}
