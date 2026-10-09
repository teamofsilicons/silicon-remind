//! End-to-end tests of the HTTP API against a stub Silicon Accounts.
//!
//! The stub serves a JWKS for an Ed25519 key generated here, so the tests sign
//! their own access tokens; it also answers introspection, lookups and proof
//! verification. Each test gets its own production and testing databases.

use axum::{Router, body::Body};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{SigningKey, pkcs8::EncodePrivateKey as _};
use http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt as _;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use crate::{
    config::{DatabaseSettings, Settings},
    domain::ActorKind,
    infrastructure::testing::TestEnvironments,
};

mod authentication;
mod proofs;
mod routes;
mod webhook;

/// Signs access tokens with one Ed25519 key published under `kid`.
pub(super) struct Signer {
    key: EncodingKey,
    kid: String,
    x: String,
}

impl Signer {
    pub(super) fn new(seed: u8, kid: &str) -> anyhow::Result<Self> {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let der = signing
            .to_pkcs8_der()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(Self {
            key: EncodingKey::from_ed_der(der.as_bytes()),
            kid: kid.to_owned(),
            x: URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes()),
        })
    }

    pub(super) fn jwk(&self) -> Value {
        json!({"kty": "OKP", "crv": "Ed25519", "x": self.x, "kid": self.kid, "alg": "EdDSA", "use": "sig"})
    }

    pub(super) fn sign(&self, claims: &Value) -> anyhow::Result<String> {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(self.kid.clone());
        Ok(jsonwebtoken::encode(&header, claims, &self.key)?)
    }
}

pub(super) struct Harness {
    _database: crate::test_support::TestPostgres,
    _testing: crate::test_support::TestPostgres,
    pub(super) pool: PgPool,
    pub(super) accounts: MockServer,
    pub(super) signer: Signer,
    pub(super) issuer: String,
    pub(super) app: Router,
}

impl Harness {
    pub(super) async fn new() -> anyhow::Result<Self> {
        let database = crate::test_support::TestPostgres::start().await?;
        let pool = database.pool(5).await?;
        crate::infrastructure::postgres::migrate(&pool).await?;
        let testing = crate::test_support::TestPostgres::start().await?;
        let control = testing.pool(2).await?;
        TestEnvironments::migrate(&control).await?;
        control.close().await;

        let accounts = MockServer::start().await;
        let signer = Signer::new(7, "k1")?;
        Mock::given(method("GET"))
            .and(path("/.well-known/jwks.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"keys": [signer.jwk()]})))
            .with_priority(10)
            .mount(&accounts)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/introspect"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"active": true, "client_id": "remind"})),
            )
            .with_priority(10)
            .mount(&accounts)
            .await;

        let settings = Settings::for_tests(&accounts.uri(), &database.url)?;
        let mut state = super::ApiState::new(&settings, pool.clone())?;
        state.tests = Some(
            TestEnvironments::connect(
                &DatabaseSettings {
                    url: secrecy::SecretString::from(testing.url.clone()),
                    max_connections: 4.try_into()?,
                    min_connections: 0,
                    acquire_timeout: std::time::Duration::from_secs(5),
                    statement_timeout: Some(std::time::Duration::from_secs(5)),
                },
                state.encryption.clone(),
            )
            .await?,
        );
        Ok(Self {
            issuer: accounts.uri(),
            app: super::router(state, &settings),
            pool,
            accounts,
            signer,
            _database: database,
            _testing: testing,
        })
    }

    pub(super) fn claims(&self, uuid: &str, kind: ActorKind, id: &str) -> Value {
        let now = chrono::Utc::now().timestamp();
        json!({
            "iss": self.issuer, "sub": uuid, "aud": "remind", "exp": now + 1800, "iat": now,
            "nbf": now, "jti": uuid::Uuid::now_v7().to_string(), "kind": kind.as_str(), "id": id,
            "mid": format!("remind:{uuid}"), "fid": "family-1", "scope": "profile"
        })
    }

    /// `Authorization` value with a valid access token for the account.
    pub(super) fn bearer(&self, uuid: &str, kind: ActorKind, id: &str) -> anyhow::Result<String> {
        Ok(format!(
            "Bearer {}",
            self.signer.sign(&self.claims(uuid, kind, id))?
        ))
    }

    /// Seeds an active, freshly looked-up account with its storage key.
    pub(super) async fn seed(
        &self,
        uuid: &str,
        kind: ActorKind,
        id: &str,
        custodian: Option<(&str, &str)>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO accounts (uuid, kind, public_id, custodian_uuid, custodian_id, status, looked_up_at) \
             VALUES ($1, $2, $3, $4, $5, 'active', clock_timestamp())",
        )
        .bind(uuid)
        .bind(kind.as_str())
        .bind(id)
        .bind(custodian.map(|(uuid, _)| uuid))
        .bind(custodian.map(|(_, id)| id))
        .execute(&self.pool)
        .await?;
        sqlx::query("INSERT INTO account_keys (storage_id, account_uuid, origin) VALUES ($1, $2, 'accounts')")
            .bind(uuid::Uuid::now_v7())
            .bind(uuid)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Makes Silicon Accounts answer a lookup by id for an account.
    pub(super) async fn lookup(
        &self,
        uuid: &str,
        kind: ActorKind,
        id: &str,
        custodian: Option<(&str, &str)>,
    ) {
        let mut summary = json!({"uuid": uuid, "kind": kind.as_str(), "id": id, "display_name": id, "pfp_url": "", "status": "active"});
        if let Some((custodian_uuid, custodian_id)) = custodian {
            summary["custodian"] = json!({"uuid": custodian_uuid, "id": custodian_id});
        }
        for route in [
            format!("/v1/accounts/by-id/{id}"),
            format!("/v1/accounts/{uuid}"),
        ] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(&summary))
                .mount(&self.accounts)
                .await;
        }
    }

    pub(super) async fn send(
        &self,
        verb: &str,
        uri: &str,
        authorization: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> anyhow::Result<(StatusCode, Value)> {
        let mut request = Request::builder().method(verb).uri(uri);
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))?,
            None => request.body(Body::empty())?,
        };
        let response = self.app.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = response.into_body().collect().await?.to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&bytes)}))
        };
        Ok((status, value))
    }
}

/// The error code of a response body.
pub(super) fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}
