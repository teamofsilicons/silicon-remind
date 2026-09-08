//! Black-box interoperability checks for generic outbound webhooks.

use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac as _};
use secrecy::SecretString;
use serde_json::{Value, json};
use sha2::Sha256;
use silicon_remind::infrastructure::webhook::{
    ReminderEvent, WebhookClient, WebhookDeliveryError, WebhookDestination, WebhookReceipt,
};
use url::Url;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

const ENDPOINT_PATH: &str = "/custom/reminders/assistant";
struct Fixture {
    now: DateTime<Utc>,
    event: ReminderEvent,
}

impl Fixture {
    fn new() -> Result<Self> {
        let now = DateTime::parse_from_rfc3339("2026-08-31T09:00:00Z")?.with_timezone(&Utc);
        let scheduled_for =
            DateTime::parse_from_rfc3339("2026-08-31T08:59:00Z")?.with_timezone(&Utc);
        Ok(Self {
            now,
            event: ReminderEvent {
                execution_id: Uuid::from_u128(1),
                schedule_id: Uuid::from_u128(2),
                silicon_id: "assistant:tos".to_owned(),
                text: "Prepare the daily report".to_owned(),
                scheduled_for,
                timezone: "Asia/Kolkata".to_owned(),
            },
        })
    }
}

fn client() -> Result<WebhookClient> {
    WebhookClient::new(Duration::from_secs(1), Duration::from_secs(2), 64 * 1_024)
}

fn destination(server: &MockServer) -> Result<WebhookDestination> {
    Ok(WebhookDestination {
        endpoint_url: Url::parse(&format!("{}{ENDPOINT_PATH}", server.uri()))?,
        signing_secret: SecretString::from("shared-secret"),
    })
}

fn unsigned_destination(server: &MockServer) -> Result<WebhookDestination> {
    Ok(WebhookDestination {
        endpoint_url: Url::parse(&format!("{}{ENDPOINT_PATH}", server.uri()))?,
        signing_secret: SecretString::from(""),
    })
}

fn header<'a>(request: &'a Request, name: &str) -> Result<&'a str> {
    request
        .headers
        .get(name)
        .with_context(|| format!("request is missing {name}"))?
        .to_str()
        .with_context(|| format!("{name} is not valid header text"))
}

fn expected_signature(id: Uuid, timestamp: &str, body: &[u8]) -> Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"shared-secret")
        .map_err(|_| anyhow::anyhow!("test signing secret must be accepted by HMAC"))?;
    mac.update(id.to_string().as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(format!(
        "v1,{}",
        STANDARD.encode(mac.finalize().into_bytes())
    ))
}

async fn deliver_with(
    template: ResponseTemplate,
) -> Result<Result<WebhookReceipt, WebhookDeliveryError>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(ENDPOINT_PATH))
        .respond_with(template)
        .mount(&server)
        .await;
    let fixture = Fixture::new()?;
    let result = client()?
        .deliver(&destination(&server)?, &fixture.event, fixture.now)
        .await;
    Ok(result)
}

#[tokio::test]
async fn accepted_delivery_has_exact_envelope_idempotency_and_hmac_contract() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(ENDPOINT_PATH))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    let fixture = Fixture::new()?;
    let receipt = client()?
        .deliver(&destination(&server)?, &fixture.event, fixture.now)
        .await?;
    ensure!(
        receipt.event_id == fixture.event.execution_id,
        "webhook receipt marker must be the execution UUID"
    );

    let requests = server
        .received_requests()
        .await
        .context("wiremock request recording must be enabled")?;
    ensure!(
        requests.len() == 1,
        "expected one webhook request, got {}",
        requests.len()
    );
    let request = requests
        .first()
        .context("recorded request disappeared after length check")?;
    ensure!(
        request.method.as_str() == "POST",
        "webhook method must be POST"
    );
    ensure!(
        request.url.path() == ENDPOINT_PATH,
        "webhook endpoint path changed"
    );
    ensure!(
        header(request, "content-type")? == "application/json",
        "webhook body must be JSON"
    );
    ensure!(
        header(request, "idempotency-key")? == fixture.event.execution_id.to_string(),
        "execution UUID must be the webhook idempotency key"
    );

    let timestamp = fixture.now.timestamp().to_string();
    ensure!(
        header(request, "webhook-timestamp")? == timestamp,
        "webhook timestamp changed"
    );
    ensure!(
        header(request, "webhook-signature")?
            == expected_signature(fixture.event.execution_id, &timestamp, &request.body)?,
        "webhook HMAC must cover `<execution_id>.<timestamp>.<raw_body>`"
    );

    let body: Value = serde_json::from_slice(&request.body)?;
    let expected_body = json!({
        "type": "remind.schedule.triggered",
        "source": "silicon-remind",
        "subject": fixture.event.schedule_id.to_string(),
        "occurred_at": fixture.event.scheduled_for,
        "schema_version": "1.0",
        "payload": {
            "execution_id": fixture.event.execution_id,
            "schedule_id": fixture.event.schedule_id,
            "silicon_id": fixture.event.silicon_id,
            "text": fixture.event.text,
            "scheduled_for": fixture.event.scheduled_for,
            "timezone": fixture.event.timezone,
        },
    });
    ensure!(
        body == expected_body,
        "webhook event envelope changed: {body:#}"
    );
    Ok(())
}

#[tokio::test]
async fn retryable_http_statuses_include_explicit_transients_and_all_5xx() -> Result<()> {
    for status in [408_u16, 425, 429, 500, 501, 599] {
        let result =
            deliver_with(ResponseTemplate::new(status).set_body_string("unavailable")).await?;
        let Err(error) = result else {
            bail!("HTTP {status} must not be accepted as delivered");
        };
        ensure!(error.is_retryable(), "HTTP {status} must be retryable");
        ensure!(
            error.reason().contains(&status.to_string()),
            "HTTP {status} diagnostic must retain the status code"
        );
    }
    Ok(())
}

#[tokio::test]
async fn definitive_4xx_responses_are_terminal() -> Result<()> {
    for status in [400_u16, 401, 403, 404, 409, 422] {
        let result =
            deliver_with(ResponseTemplate::new(status).set_body_string("rejected")).await?;
        let Err(error) = result else {
            bail!("HTTP {status} must not be accepted as delivered");
        };
        ensure!(!error.is_retryable(), "HTTP {status} must be terminal");
        ensure!(
            error.reason().contains(&status.to_string()),
            "HTTP {status} diagnostic must retain the status code"
        );
    }
    Ok(())
}

#[tokio::test]
async fn any_success_status_and_body_are_accepted() -> Result<()> {
    for template in [
        ResponseTemplate::new(200).set_body_raw(b"not-json".to_vec(), "application/json"),
        ResponseTemplate::new(202).set_body_json(json!({"queued": true})),
        ResponseTemplate::new(204),
    ] {
        let result = deliver_with(template).await?;
        let receipt = result?;
        ensure!(receipt.event_id == Uuid::from_u128(1));
    }
    Ok(())
}

#[tokio::test]
async fn unsigned_destinations_omit_signature_headers() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(ENDPOINT_PATH))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let fixture = Fixture::new()?;
    client()?
        .deliver(&unsigned_destination(&server)?, &fixture.event, fixture.now)
        .await?;
    let request = server
        .received_requests()
        .await
        .context("wiremock request recording must be enabled")?
        .into_iter()
        .next()
        .context("unsigned webhook request was not recorded")?;
    ensure!(!request.headers.contains_key("webhook-signature"));
    ensure!(!request.headers.contains_key("webhook-timestamp"));
    Ok(())
}
