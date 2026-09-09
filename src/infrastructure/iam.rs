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
            models::ApplicationAuthorizationActorType::Carbon => ActorKind::Carbon,
            models::ApplicationAuthorizationActorType::Silicon => ActorKind::Silicon,
            models::ApplicationAuthorizationActorType::Other(_) => {
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
            public_id: Some(snapshot.public_id.clone()),
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
