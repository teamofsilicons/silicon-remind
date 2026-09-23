//! Silicon IAM application integration through the official Rust client.

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret as _, SecretString};
use silicon_iam_client::{Client, Credential, EnvironmentKey, Mutation, models};
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::IamSettings,
    domain::{Actor, ActorKind, ReminderReadScope},
};

/// Authentication failure distinguished from an unavailable authority.
#[derive(Debug, Error)]
pub enum IamError {
    /// No current authority in the requested organization.
    #[error("IAM token is not authorized for the requested organization")]
    Unauthenticated,
    /// IAM could not produce a trustworthy response.
    #[error("IAM is temporarily unavailable")]
    Unavailable(#[source] anyhow::Error),
}

/// Stateless application client. Secrets remain on the Remind server.
#[derive(Clone, Debug)]
pub struct IamClient {
    client: Client,
    app_id: String,
    base_url: url::Url,
    testing_environment_id: Option<Uuid>,
}

struct CanonicalActor {
    kind: ActorKind,
    public_id: String,
    org_id: String,
    membership_id: String,
    authorization_epoch: u64,
    organization_iam_id: Uuid,
    org_role: Option<String>,
}

impl CanonicalActor {
    async fn resolve(self, pool: &PgPool) -> Result<Actor, IamError> {
        let kind = match self.kind {
            ActorKind::Carbon => "carbon",
            ActorKind::Silicon => "silicon",
        };
        let id = super::identity_keys::resolve(pool, kind, &self.public_id)
            .await
            .map_err(|error| IamError::Unavailable(error.into()))?;
        let membership_id = super::identity_keys::resolve(pool, "membership", &self.membership_id)
            .await
            .map_err(|error| IamError::Unavailable(error.into()))?;
        Ok(Actor {
            kind: self.kind,
            id: id.to_string(),
            org_id: self.org_id,
            membership_id,
            authorization_epoch: self.authorization_epoch,
            organization_iam_id: Some(self.organization_iam_id),
            public_id: Some(self.public_id),
            org_role: self.org_role,
            read_scope: ReminderReadScope::organization(),
        })
    }
}

impl IamClient {
    /// Constructs the official client with bounded requests and deployment-owned updates.
    ///
    /// # Errors
    ///
    /// Returns an error if the service URL or HTTP configuration is invalid.
    pub fn new(settings: &IamSettings) -> anyhow::Result<Self> {
        let client = Client::builder(settings.base_url.as_str())?
            .timeout(settings.request_timeout)
            .auto_update(false)
            .credential(Credential::application(
                &settings.app_id,
                settings.app_secret.expose_secret(),
            ))
            .build()?;
        Ok(Self {
            client,
            app_id: settings.app_id.clone(),
            base_url: settings.base_url.clone(),
            testing_environment_id: None,
        })
    }

    /// Binds every IAM request to one test environment and its own app secret.
    #[must_use]
    pub fn in_environment(&self, id: Uuid, key: EnvironmentKey, app_secret: &SecretString) -> Self {
        Self {
            client: self
                .client
                .with_environment(key)
                .with_credential(Credential::application(
                    &self.app_id,
                    app_secret.expose_secret(),
                )),
            app_id: self.app_id.clone(),
            base_url: self.base_url.clone(),
            testing_environment_id: Some(id),
        }
    }

    /// Resolves the imported test application without an IAM root credential.
    /// # Errors
    /// Rejects production secrets, unavailable worlds and mismatched applications.
    pub async fn discover(
        settings: &IamSettings,
        secret: &SecretString,
    ) -> Result<(Self, models::ApplicationTestingContext), IamError> {
        let mut iam = Self::new(settings).map_err(IamError::Unavailable)?;
        iam.client = iam
            .client
            .with_testing_application(&settings.app_id, secret.expose_secret())
            .map_err(classify)?
            .with_credential(Credential::application(
                &settings.app_id,
                secret.expose_secret(),
            ));
        let context = iam
            .client
            .applications()
            .testing_context()
            .await
            .map_err(classify)?;
        let meta = context
            .environment
            .as_ref()
            .ok_or(IamError::Unauthenticated)?;
        if context.application.app_id != settings.app_id
            || context.environment_id.is_nil()
            || meta.environment_id != context.environment_id
            || meta.version < 1
            || !matches!(meta.creator_type.as_str(), "carbon" | "silicon")
            || meta.creator_id.is_empty()
        {
            return Err(IamError::Unauthenticated);
        }
        iam.testing_environment_id = Some(context.environment_id);
        Ok((iam, context))
    }

    /// Public application configuration for SLT discovery; excludes all credentials.
    #[must_use]
    pub fn public_info(&self) -> serde_json::Value {
        serde_json::json!({
            "app_id": self.app_id,
            "iam_url": self.base_url,
            "iam_environment_id": self.testing_environment_id,
        })
    }

    /// Exchanges a user-supplied SLT, without receiving IAM login credentials.
    ///
    /// # Errors
    ///
    /// Returns unauthorized for rejected SLTs or unavailable for an untrustworthy IAM response.
    pub async fn login(
        &self,
        slt: &SecretString,
        mutation: &Mutation,
    ) -> Result<models::OAuthTokenResponse, IamError> {
        if self.testing_environment_id.is_none() && !slt.expose_secret().starts_with("oac_") {
            return Err(IamError::Unauthenticated);
        }
        self.client
            .oauth()
            .login(&self.app_id, slt.expose_secret(), mutation)
            .await
            .map_err(classify)
    }

    /// Rotates an existing application's refresh token.
    ///
    /// # Errors
    ///
    /// Returns unauthorized for rejected refresh tokens or unavailable when IAM cannot answer.
    pub async fn refresh(
        &self,
        token: &SecretString,
        mutation: &Mutation,
    ) -> Result<models::OAuthTokenResponse, IamError> {
        self.client
            .oauth()
            .refresh(&self.app_id, token.expose_secret(), mutation)
            .await
            .map_err(classify)
    }

    /// Revokes the refresh-token family or supplied access token.
    ///
    /// # Errors
    ///
    /// Returns an IAM rejection or availability failure.
    pub async fn logout(&self, token: &SecretString, mutation: &Mutation) -> Result<(), IamError> {
        self.client
            .oauth()
            .revoke(
                &models::OAuthRevocationRequest {
                    token: token.expose_secret().to_owned(),
                    token_type_hint: None,
                },
                mutation,
            )
            .await
            .map_err(classify)
    }

    /// Fetches current authority on every request, including revocation and membership changes.
    ///
    /// # Errors
    ///
    /// Returns unauthorized unless the live token, audience, organization and testing plane all match.
    pub async fn authenticate(
        &self,
        token: &SecretString,
        org_id: &str,
        now: DateTime<Utc>,
        pool: &PgPool,
    ) -> Result<Actor, IamError> {
        let inspected = self.inspect(token, Some(org_id), now).await?;
        let snapshot = inspected
            .authorization
            .as_ref()
            .ok_or(IamError::Unauthenticated)?;
        if inspected.authorizations.is_some() || snapshot.org_id != org_id {
            return Err(IamError::Unauthenticated);
        }
        self.actor(&inspected, snapshot)?.resolve(pool).await
    }

    /// Returns only organizations currently authorized through IAM for this token.
    ///
    /// # Errors
    /// Rejects invalid, expired, wrong-audience, or cross-plane authority.
    pub async fn organizations(
        &self,
        token: &SecretString,
        now: DateTime<Utc>,
        pool: &PgPool,
    ) -> Result<Vec<Actor>, IamError> {
        let inspected = self.inspect(token, None, now).await?;
        let snapshots = match (&inspected.authorization, &inspected.authorizations) {
            (Some(snapshot), None) => std::slice::from_ref(snapshot),
            (None, Some(snapshots)) if inspected.org_id.is_none() => snapshots.as_slice(),
            _ => return Err(IamError::Unauthenticated),
        };
        let mut seen = std::collections::HashSet::new();
        let mut actors = Vec::with_capacity(snapshots.len());
        let mut identity = None;
        for snapshot in snapshots {
            if !seen.insert(&snapshot.org_id) {
                return Err(IamError::Unauthenticated);
            }
            let actor = self.actor(&inspected, snapshot)?;
            let current = (actor.kind, actor.public_id.clone());
            if identity.as_ref().is_some_and(|prior| prior != &current) {
                return Err(IamError::Unauthenticated);
            }
            identity = Some(current);
            actors.push(actor);
        }
        let mut resolved = Vec::with_capacity(actors.len());
        for actor in actors {
            resolved.push(actor.resolve(pool).await?);
        }
        Ok(resolved)
    }

    async fn inspect(
        &self,
        token: &SecretString,
        org_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<models::TokenIntrospection, IamError> {
        // Refresh tokens never authorize requests or organization discovery.
        if !token.expose_secret().starts_with("oat_") {
            return Err(IamError::Unauthenticated);
        }
        let inspected = self
            .client
            .oauth()
            .introspect(
                &models::TokenIntrospectionRequest {
                    token: token.expose_secret().to_owned(),
                    token_type_hint: Some(
                        models::TokenIntrospectionRequestTokenTypeHint::AccessToken,
                    ),
                },
                org_id,
            )
            .await
            .map_err(classify)?;
        if !inspected.active
            || inspected
                .expires_at
                .is_none_or(|expiry| expiry <= now.timestamp())
            || inspected.client_id.as_deref() != Some(&self.app_id)
            || inspected
                .audience
                .as_deref()
                .is_some_and(|audience| audience != self.app_id)
        {
            return Err(IamError::Unauthenticated);
        }
        Ok(inspected)
    }

    fn actor(
        &self,
        inspected: &models::TokenIntrospection,
        snapshot: &models::ApplicationAuthorization,
    ) -> Result<CanonicalActor, IamError> {
        if snapshot.audience != self.app_id
            || inspected
                .org_id
                .as_deref()
                .is_some_and(|org| org != snapshot.org_id)
            || inspected
                .public_id
                .as_ref()
                .is_some_and(|id| Some(id) != snapshot.public_id.as_ref())
            || inspected
                .membership_id
                .as_ref()
                .is_some_and(|id| id != &snapshot.membership_id)
            || inspected
                .authorization_epoch
                .is_some_and(|epoch| epoch != snapshot.authorization_epoch)
            || snapshot.testing_environment_id != self.testing_environment_id
            || snapshot.organization_id.is_nil()
            || snapshot.membership_version < 1
            || snapshot.authorization_epoch < 1
            || !crate::domain::is_valid_iam_label(&snapshot.org_id)
        {
            return Err(IamError::Unauthenticated);
        }
        let kind = match snapshot.actor_type {
            Some(models::ApplicationAuthorizationActorType::Carbon) => ActorKind::Carbon,
            Some(models::ApplicationAuthorizationActorType::Silicon) => ActorKind::Silicon,
            _ => {
                return Err(IamError::Unauthenticated);
            }
        };
        let epoch =
            u64::try_from(snapshot.authorization_epoch).map_err(|_| IamError::Unauthenticated)?;
        let public_id = snapshot
            .public_id
            .as_ref()
            .ok_or(IamError::Unauthenticated)?;
        let valid = match kind {
            ActorKind::Carbon => crate::domain::is_valid_carbon_id(public_id),
            ActorKind::Silicon => crate::domain::is_valid_global_silicon_id(public_id),
        };
        let inspected_kind = match inspected.actor_type.as_ref() {
            Some(models::TokenIntrospectionActorType::Carbon) => Some(ActorKind::Carbon),
            Some(models::TokenIntrospectionActorType::Silicon) => Some(ActorKind::Silicon),
            Some(_) => return Err(IamError::Unauthenticated),
            None => None,
        };
        if !valid
            || inspected_kind.is_some_and(|value| value != kind)
            || snapshot.membership_id != format!("{public_id}[{}]", snapshot.org_id)
        {
            return Err(IamError::Unauthenticated);
        }
        Ok(CanonicalActor {
            kind,
            public_id: public_id.clone(),
            org_id: snapshot.org_id.clone(),
            membership_id: snapshot.membership_id.clone(),
            authorization_epoch: epoch,
            organization_iam_id: snapshot.organization_id,
            org_role: snapshot.org_role.clone(),
        })
    }
}

fn classify(error: silicon_iam_client::Error) -> IamError {
    match &error {
        silicon_iam_client::Error::Api(api)
            if matches!(api.status, 400 | 401)
                && matches!(
                    api.code.as_str(),
                    "invalid_grant" | "refresh_token_reuse" | "unauthenticated" | "invalid_token"
                ) =>
        {
            IamError::Unauthenticated
        }
        // IAM authenticates Remind itself using deployment-owned app credentials.
        // An invalid_client or other unknown 4xx is not proof that the user's
        // refresh family is invalid. Returning 401 would make clients erase it.
        _ => IamError::Unavailable(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    #[test]
    fn provider_failures_do_not_invalidate_a_saved_user_session() {
        for (status, code) in [
            (401, "invalid_client"),
            (400, "invalid_request"),
            (401, "unknown_auth_failure"),
            (403, "forbidden"),
            (404, "not_found"),
            (422, "validation_error"),
            (503, "service_unavailable"),
        ] {
            let error = silicon_iam_client::ApiError {
                status,
                code: code.to_owned(),
                message: "provider response".to_owned(),
                details: None,
                request_id: None,
            };
            assert!(matches!(classify(error.into()), IamError::Unavailable(_)));
        }
    }

    #[test]
    fn explicit_expiry_or_reuse_still_ends_the_user_session() {
        for (status, code) in [
            (400, "invalid_grant"),
            (401, "invalid_grant"),
            (400, "refresh_token_reuse"),
            (401, "unauthenticated"),
            (401, "invalid_token"),
        ] {
            let error = silicon_iam_client::ApiError {
                status,
                code: code.to_owned(),
                message: "credential rejected".to_owned(),
                details: None,
                request_id: None,
            };
            assert!(matches!(classify(error.into()), IamError::Unauthenticated));
        }
    }

    #[tokio::test]
    async fn discovery_and_subsequent_requests_use_only_the_test_application_credential()
    -> anyhow::Result<()> {
        let server = MockServer::start().await;
        let id = Uuid::now_v7();
        let app_id = "remind";
        let secret = format!("ask_{}", "t".repeat(43));
        let authorization = format!("Basic {}", STANDARD.encode(format!("{app_id}:{secret}")));
        Mock::given(method("GET"))
            .and(path("/api/v1/application/testing-context"))
            .and(header("authorization", authorization.as_str()))
            .and(header("x-testing-application", authorization.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "environment_id": id,
                "application": {
                    "app_id": app_id, "base_url": server.uri(),
                    "app_scope": {"iam": [], "external": []},
                    "webhook_scope": [], "testing_idle_days": 15,
                },
                "environment": {
                    "environment_id": id, "org_id": "tos", "name": "Timezone test",
                    "version": 1, "key_generation": 1,
                    "created_at": "2026-09-20T00:00:00Z",
                    "creator_type": "carbon", "creator_id": "tester",
                },
                "webhook_key_digest": "test-root-digest",
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/oauth/introspect"))
            .and(header("authorization", authorization.as_str()))
            .and(header("x-testing-application", authorization.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"active": false})))
            .expect(1)
            .mount(&server)
            .await;
        let settings = IamSettings {
            base_url: url::Url::parse(&server.uri())?,
            app_id: app_id.into(),
            app_secret: SecretString::from(format!("ask_{}", "p".repeat(43))),
            request_timeout: std::time::Duration::from_secs(5),
            webhook_keys: std::collections::BTreeMap::new(),
        };
        let (client, context) = IamClient::discover(&settings, &SecretString::from(secret)).await?;
        assert_eq!(context.environment_id, id);
        assert_eq!(client.testing_environment_id, Some(id));
        let introspection = client
            .client
            .oauth()
            .introspect(
                &models::TokenIntrospectionRequest {
                    token: "oat_test_probe".into(),
                    token_type_hint: None,
                },
                None,
            )
            .await?;
        assert!(!introspection.active);
        Ok(())
    }

    #[test]
    fn public_info_exposes_configuration_and_plane_without_secrets() -> anyhow::Result<()> {
        let client = IamClient::new(&IamSettings {
            base_url: url::Url::parse("http://127.0.0.1:8080")?,
            app_id: "custom-remind".into(),
            app_secret: SecretString::from("private-app-secret"),
            request_timeout: std::time::Duration::from_secs(5),
            webhook_keys: std::collections::BTreeMap::new(),
        })?;
        let mut expected = json!({"app_id":"custom-remind", "iam_url":"http://127.0.0.1:8080/", "iam_environment_id":null});
        assert_eq!(client.public_info(), expected);
        let id = Uuid::now_v7();
        let sandbox = client.in_environment(
            id,
            EnvironmentKey::new("12345678901234567890123456789012")?,
            &SecretString::from("private-test-secret"),
        );
        expected["iam_environment_id"] = json!(id);
        assert_eq!(sandbox.public_info(), expected);
        assert!(client.public_info()["iam_environment_id"].is_null());
        Ok(())
    }

    #[test]
    fn unscoped_authority_preserves_identity_audience_and_plane_boundaries() -> anyhow::Result<()> {
        let client = IamClient {
            client: Client::builder("http://127.0.0.1:8080")?
                .auto_update(false)
                .build()?,
            app_id: "remind".to_owned(),
            base_url: url::Url::parse("http://127.0.0.1:8080")?,
            testing_environment_id: None,
        };
        let principal = Uuid::now_v7();
        let membership = "c:person[alpha]".to_owned();
        let inspected: models::TokenIntrospection = serde_json::from_value(json!({
            "active": true, "public_id": "c:person", "actor_type": "carbon", "principal_id": principal, "client_id": "remind",
            "org_id": null, "membership_id": null,
        }))?;
        let snapshot: models::ApplicationAuthorization = serde_json::from_value(json!({
            "principal_id": principal, "actor_type": "carbon", "public_id": "c:person",
            "organization_id": Uuid::now_v7(), "org_id": "alpha", "membership_id": membership,
            "membership_version": 1, "authorization_epoch": 1, "audience": "remind",
            "testing_environment_id": null, "scopes": [], "org_role": "member", "tags": null,
        }))?;
        assert_eq!(client.actor(&inspected, &snapshot)?.org_id, "alpha");
        // Deployed IAM may omit the top-level public ID; its scoped snapshot
        // still supplies the canonical identity. Private UUID fields are ignored.
        let mut legacy = inspected.clone();
        legacy.public_id = None;
        assert_eq!(client.actor(&legacy, &snapshot)?.public_id, "c:person");
        let mut missing = snapshot.clone();
        missing.public_id = None;
        assert!(matches!(
            client.actor(&legacy, &missing),
            Err(IamError::Unauthenticated)
        ));
        let mut scoped = inspected.clone();
        scoped.org_id = Some("alpha".to_owned());
        scoped.membership_id = Some(membership.clone());
        assert!(client.actor(&scoped, &snapshot).is_ok());
        scoped.org_id = Some("other".to_owned());
        assert!(matches!(
            client.actor(&scoped, &snapshot),
            Err(IamError::Unauthenticated)
        ));
        scoped.org_id = None;
        scoped.membership_id = Some("other[alpha]".to_owned());
        assert!(matches!(
            client.actor(&scoped, &snapshot),
            Err(IamError::Unauthenticated)
        ));
        for change in ["principal", "audience", "plane", "epoch"] {
            let mut invalid = snapshot.clone();
            match change {
                "principal" => invalid.public_id = Some("other".to_owned()),
                "audience" => invalid.audience = "other".to_owned(),
                "plane" => invalid.testing_environment_id = Some(Uuid::now_v7()),
                _ => invalid.authorization_epoch = -1,
            }
            assert!(matches!(
                client.actor(&inspected, &invalid),
                Err(IamError::Unauthenticated)
            ));
        }
        Ok(())
    }
}
