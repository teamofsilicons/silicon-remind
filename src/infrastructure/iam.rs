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
            testing_environment_id: Some(id),
        }
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
        // Introspection can also describe refresh tokens. Those never authorize actions.
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
                Some(org_id),
            )
            .await
            .map_err(classify)?;
        let snapshot = inspected.authorization.ok_or(IamError::Unauthenticated)?;
        if !inspected.active
            || inspected
                .expires_at
                .is_none_or(|expiry| expiry <= now.timestamp())
            || inspected.client_id.as_deref() != Some(&self.app_id)
            || snapshot.audience != self.app_id
            || snapshot.org_id != org_id
            || inspected.org_id.as_deref() != Some(org_id)
            || inspected.principal_id != Some(snapshot.principal_id)
            || inspected.membership_id != Some(snapshot.membership_id)
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
            org_id: org_id.to_owned(),
            membership_id: snapshot.membership_id,
            authorization_epoch: epoch,
            organization_iam_id: Some(snapshot.organization_id),
            public_id: Some(snapshot.public_id),
            org_role: snapshot.org_role,
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
