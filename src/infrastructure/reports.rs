//! Durable Postmark delivery for production bug reports only.
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
use sqlx::{FromRow, PgPool};
use std::time::Duration;
use uuid::Uuid;

const RECIPIENTS: &str = "saketdev12@gmail.com,shubhastro2@gmails.com,bugs@teamofsilicons.com";
#[derive(FromRow)]
struct PendingReport {
    id: Uuid,
    actor_id: String,
    reporter_id: Option<String>,
    reporter_kind: Option<String>,
    message: String,
    pr: Option<String>,
    attempts: i32,
}

pub(crate) async fn run(pool: PgPool, token: SecretString) -> anyhow::Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if let Err(error) =
            deliver_one(&pool, &http, &token, "https://api.postmarkapp.com/email").await
        {
            tracing::error!(error=%error,"bug report queue unavailable");
        }
    }
}

async fn deliver_one(
    pool: &PgPool,
    http: &reqwest::Client,
    token: &SecretString,
    endpoint: &str,
) -> anyhow::Result<()> {
    // Claim before external I/O, with a bounded lease recovered after worker failure.
    let report: Option<PendingReport> = sqlx::query_as("UPDATE bug_reports SET status='sending', attempts=attempts+1, next_attempt_at=now()+interval '60 seconds' WHERE id=(SELECT id FROM bug_reports WHERE status IN ('queued','sending') AND next_attempt_at<=now() ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING id,actor_id,(SELECT NULLIF(a.public_id,'') FROM accounts a WHERE a.uuid=bug_reports.actor_id) AS reporter_id,(SELECT a.kind FROM accounts a WHERE a.uuid=bug_reports.actor_id) AS reporter_kind,message,pr,attempts").fetch_optional(pool).await?;
    let Some(report) = report else {
        return Ok(());
    };
    let response = http
        .post(endpoint)
        .header("X-Postmark-Server-Token", token.expose_secret())
        .json(&payload(&report))
        .send()
        .await;
    let (status, reason) = match response {
        Ok(response) if response.status().is_success() => ("sent", None),
        Ok(response)
            if response.status().is_client_error() && response.status().as_u16() != 429 =>
        {
            (
                "failed",
                Some(format!("postmark_http_{}", response.status().as_u16())),
            )
        }
        Ok(response) => (
            if report.attempts >= 8 {
                "failed"
            } else {
                "queued"
            },
            Some(format!("postmark_http_{}", response.status().as_u16())),
        ),
        Err(_) => (
            if report.attempts >= 8 {
                "failed"
            } else {
                "queued"
            },
            Some("postmark_transport_unavailable".into()),
        ),
    };
    sqlx::query("UPDATE bug_reports SET status=$2,failure_reason=$3,next_attempt_at=now()+interval '5 minutes' WHERE id=$1 AND status='sending' AND attempts=$4")
        .bind(report.id).bind(status).bind(reason).bind(report.attempts).execute(pool).await?;
    Ok(())
}
fn payload(report: &PendingReport) -> serde_json::Value {
    json!({"From":"remind@teamofsilicons.com","To":RECIPIENTS,
        "Subject":format!("Remind bug report {}",report.id),
        "TextBody":format!("Report: {}\nReporter: {} ({}, Silicon Accounts uuid {})\n\n{}\n\nProposed fix: {}",report.id,report.reporter_id.as_deref().unwrap_or("unknown id"),report.reporter_kind.as_deref().unwrap_or("unknown kind"),report.actor_id,report.message,report.pr.as_deref().unwrap_or("none")),
        "MessageStream":"outbound","TrackOpens":false,"TrackLinks":"None",
        "Headers":[{"Name":"Message-ID","Value":format!("<{}@remind.teamofsilicons.com>",report.id)}]})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_bug_report_recipients_and_plain_text_are_used() {
        let value = payload(&PendingReport {
            id: Uuid::nil(),
            actor_id: "zQo".into(),
            reporter_id: Some("si:scout".into()),
            reporter_kind: Some("silicon".into()),
            message: "<html> stays text".into(),
            pr: None,
            attempts: 1,
        });
        assert_eq!(value["From"], "remind@teamofsilicons.com");
        assert_eq!(value["To"], RECIPIENTS);
        assert!(value.get("HtmlBody").is_none());
        assert_eq!(value["TrackOpens"], false);
        let body = value["TextBody"].as_str().unwrap_or_default();
        assert!(body.contains("Reporter: si:scout (silicon, Silicon Accounts uuid zQo)"));
        assert!(!body.contains("Organization"));
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };
    #[tokio::test]
    async fn retries_real_queue_but_never_sends_simulated_reports() -> anyhow::Result<()> {
        let container = crate::test_support::TestPostgres::start().await?;
        let pool = PgPool::connect(&container.url).await?;
        crate::infrastructure::postgres::migrate(&pool).await?;
        let id = Uuid::now_v7();
        for (report_id, status) in [(id, "queued"), (Uuid::now_v7(), "simulated")] {
            sqlx::query("INSERT INTO bug_reports(id,org_id,actor_id,idempotency_key,request_hash,message,status) VALUES($1,NULL,'zQo',$2,'hash','example reproduction',$3)")
                .bind(report_id).bind(report_id.to_string()).bind(status).execute(&pool).await?;
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/email"))
            .and(header("X-Postmark-Server-Token", "test-token"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let token = SecretString::from("test-token");
        deliver_one(&pool, &client, &token, &format!("{}/email", server.uri())).await?;
        let state: (String, i32) =
            sqlx::query_as("SELECT status,attempts FROM bug_reports WHERE id=$1")
                .bind(id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(state, ("queued".into(), 1));
        server.verify().await;
        server.reset().await;
        sqlx::query("UPDATE bug_reports SET next_attempt_at=now() WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await?;
        Mock::given(method("POST"))
            .and(path("/email"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ErrorCode":0})))
            .expect(1)
            .mount(&server)
            .await;
        deliver_one(&pool, &client, &token, &format!("{}/email", server.uri())).await?;
        deliver_one(&pool, &client, &token, &format!("{}/email", server.uri())).await?;
        let state: (String, i32) =
            sqlx::query_as("SELECT status,attempts FROM bug_reports WHERE id=$1")
                .bind(id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(state, ("sent".into(), 2));
        let attempts: i32 =
            sqlx::query_scalar("SELECT attempts FROM bug_reports WHERE status='simulated'")
                .fetch_one(&pool)
                .await?;
        assert_eq!(attempts, 0);
        server.verify().await;
        Ok(())
    }
}
