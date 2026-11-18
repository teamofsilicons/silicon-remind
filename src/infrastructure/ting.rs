//! Account-addressed reminder notifications and delegated recipient registration.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use silicon_accounts_client::{IssueAppVerification, IssueUserVerification};
use tokio::sync::Mutex;
use url::Url;

use super::{
    accounts::AccountsGateway,
    webhook::{ReminderEvent, WebhookDeliveryError},
};
use crate::{config::Settings, error::AppError};

#[derive(Clone, Debug)]
pub(crate) struct Ting {
    url: Url,
    http: reqwest::Client,
    accounts: AccountsGateway,
    send_proof: Arc<Mutex<Option<(SecretString, Instant)>>>,
}

impl Ting {
    pub(crate) fn configured(
        settings: &Settings,
        accounts: AccountsGateway,
    ) -> anyhow::Result<Option<Self>> {
        let Some(url) = settings.ting_url.clone() else {
            return Ok(None);
        };
        Ok(Some(Self {
            url,
            accounts,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            send_proof: Arc::default(),
        }))
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut url = self.url.clone();
        url.set_path(path);
        url
    }

    fn kind(&self) -> String {
        format!("{}.reminderDue", self.accounts.app_id())
    }

    async fn proof(&self, scope: &str) -> Result<SecretString, AppError> {
        let issued = self
            .accounts
            .app_client()
            .issue_app_verification(
                &IssueAppVerification {
                    receiving_app: "ting".into(),
                    scopes: vec![scope.into()],
                    access_ttl_seconds: Some(300),
                },
                None,
            )
            .await
            .map_err(|_| unavailable())?;
        Ok(SecretString::from(issued.proof_token.expose()))
    }

    /// An explicit release operation. An incompatible existing definition is an error.
    pub(crate) async fn register_type(&self) -> anyhow::Result<()> {
        let kind = self.kind();
        let body = json!({
            "type": kind,
            "description": "A scheduled Silicon Remind reminder is due.",
            "schema": {
                "type":"object","additionalProperties":false,
                "required":["execution_id","schedule_id","text","scheduled_for","timezone"],
                "properties":{
                    "execution_id":{"type":"string"},"schedule_id":{"type":"string"},
                    "text":{"type":"string"},"scheduled_for":{"type":"string"},"timezone":{"type":"string"}
                }
            }
        });
        let proof = self.proof("types.manage").await?;
        let response = self
            .http
            .post(self.endpoint(&format!("/v1/apps/{}/types", self.accounts.app_id())))
            .bearer_auth(proof.expose_secret())
            .json(&body)
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "Ting type registration returned {}",
            response.status()
        );
        let receipt: Value = response.json().await?;
        anyhow::ensure!(
            receipt["type"] == kind && receipt["schema"] == body["schema"],
            "unexpected Ting type receipt"
        );
        Ok(())
    }

    pub(crate) async fn enrol(
        &self,
        recipient: &str,
        token: &SecretString,
    ) -> Result<(), AppError> {
        let issued = self
            .accounts
            .app_client()
            .issue_user_verification(
                &IssueUserVerification {
                    receiving_app: "ting".into(),
                    scopes: vec!["subscriptions.register".into()],
                    subject_token: token.expose_secret().to_owned(),
                    access_ttl_seconds: Some(120),
                },
                None,
            )
            .await
            .map_err(|_| unavailable())?;
        let response = self
            .http
            .post(self.endpoint("/v1/subscriptions"))
            .bearer_auth(issued.proof_token.expose())
            .json(&json!({"app_id":self.accounts.app_id(),"for":recipient}))
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(unavailable());
        }
        let receipt: Value = response.json().await.map_err(|_| unavailable())?;
        if receipt["active"] != true
            || receipt["for"] != recipient
            || receipt["app_id"] != self.accounts.app_id()
        {
            return Err(unavailable());
        }
        Ok(())
    }

    async fn sender(&self) -> Result<SecretString, WebhookDeliveryError> {
        let mut proof = self.send_proof.lock().await;
        if let Some((secret, created)) = &*proof
            && created.elapsed() < Duration::from_secs(240)
        {
            return Ok(secret.clone());
        }
        let secret = self
            .proof("tings.send")
            .await
            .map_err(|_| retry("Ting sender proof unavailable"))?;
        *proof = Some((secret.clone(), Instant::now()));
        Ok(secret)
    }

    pub(crate) async fn send(
        &self,
        recipient: &str,
        event: &ReminderEvent,
    ) -> Result<&'static str, WebhookDeliveryError> {
        let proof = self.sender().await?;
        let response = self.http.post(self.endpoint("/v1/tings")).bearer_auth(proof.expose_secret())
            .json(&json!({"type":self.kind(), "for":recipient, "key":event.execution_id, "metadata":{},
                "data":{"execution_id":event.execution_id,"schedule_id":event.schedule_id,"text":event.text,
                        "scheduled_for":event.scheduled_for,"timezone":event.timezone}}))
            .send().await.map_err(|_| retry("Ting did not confirm delivery"))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let mut cached = self.send_proof.lock().await;
            if cached
                .as_ref()
                .is_some_and(|(secret, _)| secret.expose_secret() == proof.expose_secret())
            {
                *cached = None;
            }
            return Err(retry("Ting rejected the sender proof"));
        }
        if status.is_server_error() || status.as_u16() == 429 || status.as_u16() == 408 {
            return Err(retry("Ting is temporarily unavailable"));
        }
        let receipt: Value = response
            .json()
            .await
            .map_err(|_| retry("Ting returned an unreadable receipt"))?;
        if !status.is_success() {
            if receipt["error"]["code"] == "recipient_not_registered"
                || receipt["code"] == "recipient_not_registered"
            {
                return Ok("unsubscribed");
            }
            return Err(WebhookDeliveryError::Terminal {
                reason: format!("Ting rejected delivery (HTTP {})", status.as_u16()),
            });
        }
        match receipt["status"].as_str() {
            Some("accepted") if receipt["id"].as_str().is_some_and(|id| !id.is_empty()) => {
                Ok("accepted")
            }
            Some("blocked") => Ok("blocked"),
            _ => Err(retry("Ting returned an unrecognized receipt")),
        }
    }
}

fn retry(reason: &str) -> WebhookDeliveryError {
    WebhookDeliveryError::Retryable {
        reason: reason.to_owned(),
    }
}
fn unavailable() -> AppError {
    AppError::DependencyUnavailable {
        dependency: "Ting registration",
    }
}
