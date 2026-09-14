//! Silicon IAM application integration through the official Rust client.

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret as _, SecretString};
use silicon_iam_client::{Client, Credential, EnvironmentKey, Mutation, models};
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
            .with_credential(Credential::application(
                &settings.app_id,
                secret.expose_secret(),
            ))
            .with_testing_application(&settings.app_id, secret.expose_secret())
            .map_err(classify)?;
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
    ) -> Result<Actor, IamError> {
        let inspected = self.inspect(token, Some(org_id), now).await?;
        let snapshot = inspected
            .authorization
            .as_ref()
            .ok_or(IamError::Unauthenticated)?;
        if inspected.authorizations.is_some() || snapshot.org_id != org_id {
            return Err(IamError::Unauthenticated);
        }
        self.actor(&inspected, snapshot)
    }

    /// Returns only organizations currently authorized through IAM for this token.
    ///
    /// # Errors
    /// Rejects invalid, expired, wrong-audience, or cross-plane authority.
    pub async fn organizations(
        &self,
        token: &SecretString,
        now: DateTime<Utc>,
    ) -> Result<Vec<Actor>, IamError> {
        let inspected = self.inspect(token, None, now).await?;
        let snapshots = match (&inspected.authorization, &inspected.authorizations) {
            (Some(snapshot), None) => std::slice::from_ref(snapshot),
            (None, Some(snapshots)) if inspected.org_id.is_none() => snapshots.as_slice(),
            _ => return Err(IamError::Unauthenticated),
        };
        let mut seen = std::collections::HashSet::new();
        snapshots
            .iter()
            .map(|snapshot| {
                if !seen.insert(&snapshot.org_id) {
                    return Err(IamError::Unauthenticated);
                }
                self.actor(&inspected, snapshot)
            })
            .collect()
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
            || inspected.principal_id.is_none()
        {
            return Err(IamError::Unauthenticated);
        }
        Ok(inspected)
    }

    fn actor(
        &self,
        inspected: &models::TokenIntrospection,
        snapshot: &models::ApplicationAuthorization,
    ) -> Result<Actor, IamError> {
        if snapshot.audience != self.app_id
            || inspected
                .org_id
                .as_deref()
                .is_some_and(|org| org != snapshot.org_id)
            || inspected.principal_id != Some(snapshot.principal_id)
            || inspected
                .membership_id
                .is_some_and(|id| id != snapshot.membership_id)
            || snapshot.testing_environment_id != self.testing_environment_id
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
        Ok(Actor {
            kind,
            id: snapshot.principal_id.to_string(),
            org_id: snapshot.org_id.clone(),
            membership_id: snapshot.membership_id,
            authorization_epoch: epoch,
            organization_iam_id: Some(snapshot.organization_id),
            public_id: Some(
                snapshot
                    .public_id
                    .clone()
                    .ok_or(IamError::Unauthenticated)?,
            ),
            org_role: snapshot.org_role.clone(),
            read_scope: ReminderReadScope::organization(),
        })
    }
}

fn classify(error: silicon_iam_client::Error) -> IamError {
    match &error {
        silicon_iam_client::Error::Api(api)
            if matches!(api.status, 400 | 401 | 403 | 404 | 422) =>
        {
            IamError::Unauthenticated
        }
        _ => IamError::Unavailable(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn discovery_fixture(id: Uuid) -> serde_json::Value {
        json!({
            "environment_id":id,
            "application":{"app_id":"tos>remind","base_url":"https://remind.teamofsilicons.com","app_scope":{"iam":[],"external":[]},"webhook_scope":[],"testing_idle_days":15},
            "environment":{"environment_id":id,"org_id":"tos","name":"Test world","version":1,"key_generation":1,"created_at":"2026-09-13T00:00:00Z","creator_type":"carbon","creator_id":"tester"}
        })
    }

    #[tokio::test]
    async fn discovery_and_login_use_the_imported_credential_for_both_headers() -> anyhow::Result<()>
    {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{basic_auth, header, method, path},
        };
        let server = MockServer::start().await;
        let settings = IamSettings {
            base_url: server.uri().parse()?,
            app_id: "tos>remind".into(),
            app_secret: SecretString::from(format!("ask_{}", "p".repeat(43))),
            request_timeout: std::time::Duration::from_secs(2),
            webhook_keys: std::collections::BTreeMap::new(),
        };
        let secret = SecretString::from(format!("ask_{}", "t".repeat(43)));
        let selector = format!(
            "Basic {}",
            STANDARD.encode(format!("tos>remind:{}", secret.expose_secret()))
        );
        let id = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/api/v1/application/testing-context"))
            .and(basic_auth("tos>remind", secret.expose_secret()))
            .and(header("x-testing-application", selector.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(discovery_fixture(id)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/api/v1/app-auth/tokens"))
            .and(basic_auth("tos>remind", secret.expose_secret()))
            .and(header("x-testing-application", selector.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token":"oat_fixture", "refresh_token":"ort_fixture", "token_type":"Bearer", "expires_in":1800,
                "scope":"self.identity.read", "actor":null,"org_id":null
            }))).expect(1).mount(&server).await;
        let (iam, context) = IamClient::discover(&settings, &secret).await?;
        assert_eq!(context.environment_id, id);
        assert_eq!(iam.public_info()["iam_environment_id"], json!(id));
        iam.login(
            &SecretString::from("existing-test-carbon"),
            &Mutation::new(),
        )
        .await?;
        let requests = server
            .received_requests()
            .await
            .ok_or_else(|| anyhow::anyhow!("requests unavailable"))?;
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| !request.headers.contains_key("x-testing-environment-key"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn discovery_rejects_mismatched_or_missing_environment_authority() -> anyhow::Result<()> {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let server = MockServer::start().await;
        let settings = IamSettings {
            base_url: server.uri().parse()?,
            app_id: "tos>remind".into(),
            app_secret: SecretString::from(format!("ask_{}", "p".repeat(43))),
            request_timeout: std::time::Duration::from_secs(2),
            webhook_keys: std::collections::BTreeMap::new(),
        };
        let secret = SecretString::from(format!("ask_{}", "t".repeat(43)));
        for change in ["application", "world", "missing", "version", "creator"] {
            server.reset().await;
            let mut context = discovery_fixture(Uuid::now_v7());
            match change {
                "application" => context["application"]["app_id"] = json!("tos>other"),
                "world" => context["environment"]["environment_id"] = json!(Uuid::now_v7()),
                "missing" => context["environment"] = serde_json::Value::Null,
                "version" => context["environment"]["version"] = json!(0),
                _ => context["environment"]["creator_type"] = json!("application"),
            }
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200).set_body_json(context))
                .mount(&server)
                .await;
            assert!(
                matches!(
                    IamClient::discover(&settings, &secret).await,
                    Err(IamError::Unauthenticated)
                ),
                "{change}"
            );
        }
        Ok(())
    }

    #[test]
    fn public_info_exposes_configuration_and_plane_without_secrets() -> anyhow::Result<()> {
        let client = IamClient::new(&IamSettings {
            base_url: url::Url::parse("http://127.0.0.1:8080")?,
            app_id: "custom>remind".into(),
            app_secret: SecretString::from("private-app-secret"),
            request_timeout: std::time::Duration::from_secs(5),
            webhook_keys: std::collections::BTreeMap::new(),
        })?;
        let mut expected = json!({"app_id":"custom>remind", "iam_url":"http://127.0.0.1:8080/", "iam_environment_id":null});
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
            app_id: "tos>remind".to_owned(),
            base_url: url::Url::parse("http://127.0.0.1:8080")?,
            testing_environment_id: None,
        };
        let principal = Uuid::now_v7();
        let membership = Uuid::now_v7();
        let inspected: models::TokenIntrospection = serde_json::from_value(json!({
            "active": true, "principal_id": principal, "client_id": "tos>remind",
            "org_id": null, "membership_id": null,
        }))?;
        let snapshot: models::ApplicationAuthorization = serde_json::from_value(json!({
            "principal_id": principal, "actor_type": "carbon", "public_id": "person",
            "organization_id": Uuid::now_v7(), "org_id": "alpha", "membership_id": membership,
            "membership_version": 1, "authorization_epoch": 1, "audience": "tos>remind",
            "testing_environment_id": null, "scopes": [], "org_role": "member", "tags": null,
        }))?;
        assert_eq!(client.actor(&inspected, &snapshot)?.org_id, "alpha");
        let mut scoped = inspected.clone();
        scoped.org_id = Some("alpha".to_owned());
        scoped.membership_id = Some(membership);
        assert!(client.actor(&scoped, &snapshot).is_ok());
        scoped.org_id = Some("other".to_owned());
        assert!(matches!(
            client.actor(&scoped, &snapshot),
            Err(IamError::Unauthenticated)
        ));
        scoped.org_id = None;
        scoped.membership_id = Some(Uuid::now_v7());
        assert!(matches!(
            client.actor(&scoped, &snapshot),
            Err(IamError::Unauthenticated)
        ));
        for change in ["principal", "audience", "plane", "epoch"] {
            let mut invalid = snapshot.clone();
            match change {
                "principal" => invalid.principal_id = Uuid::now_v7(),
                "audience" => invalid.audience = "tos>other".to_owned(),
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
