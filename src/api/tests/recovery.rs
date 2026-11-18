//! Regression cases recovered from the interrupted migration review.
use super::{Harness, code};
use crate::domain::ActorKind;
use http::StatusCode;
use serde_json::json;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn stale_custody_is_checked_before_reading_or_managing() -> anyhow::Result<()> {
    let h = Harness::new().await?;
    h.seed("Ada", ActorKind::Carbon, "c:ada", None).await?;
    h.seed("Ian", ActorKind::Carbon, "c:ian", None).await?;
    h.seed("One", ActorKind::Silicon, "si:one", Some(("Ada", "c:ada")))
        .await?;
    h.lookup("One", ActorKind::Silicon, "si:one", Some(("Ian", "c:ian")))
        .await;
    sqlx::query("UPDATE accounts SET looked_up_at = now() - interval '1 day' WHERE uuid = 'One'")
        .execute(&h.pool)
        .await?;
    let ada = h.bearer("Ada", ActorKind::Carbon, "c:ada")?;
    let (status, list) = h
        .send("GET", "/api/v2/silicons", Some(&ada), None, &[])
        .await?;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(
        list["items"].as_array().is_some_and(Vec::is_empty),
        "{list}"
    );
    let (status, _) = h
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&ada),
            Some(json!({"id":"c:ada","silicon_id":"si:one"})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn custody_changes_revoke_old_custodian_delegation() -> anyhow::Result<()> {
    let h = Harness::new().await?;
    h.seed("Ada", ActorKind::Carbon, "c:ada", None).await?;
    h.seed("One", ActorKind::Silicon, "si:one", Some(("Ada", "c:ada")))
        .await?;
    sqlx::query("INSERT INTO reminder_viewers(id,owner_uuid,viewer_uuid,granted_by_uuid) VALUES (gen_random_uuid(),'One','Ada','Ada')")
        .execute(&h.pool).await?;
    sqlx::query("INSERT INTO silicon_allowances(id,silicon_uuid,allowed_uuid,created_by_uuid) VALUES (gen_random_uuid(),'One','Ada','Ada')")
        .execute(&h.pool).await?;
    sqlx::query("UPDATE accounts SET custodian_uuid = 'Ian' WHERE uuid = 'One'")
        .execute(&h.pool)
        .await?;
    for table in ["reminder_viewers", "silicon_allowances"] {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE revoked_at IS NULL"
        )))
        .fetch_one(&h.pool)
        .await?;
        assert_eq!(count, 0);
    }
    Ok(())
}

#[tokio::test]
async fn every_bearer_request_checks_live_type_audience_and_revocation() -> anyhow::Result<()> {
    let h = Harness::new().await?;
    h.seed("One", ActorKind::Silicon, "si:one", None).await?;
    let token = h.bearer("One", ActorKind::Silicon, "si:one")?;
    assert_eq!(
        h.send("GET", "/api/v2/schedules", Some(&token), None, &[])
            .await?
            .0,
        StatusCode::OK
    );
    let response = std::sync::Arc::new(std::sync::Mutex::new(ResponseTemplate::new(503)));
    let active_response = response.clone();
    Mock::given(method("POST"))
        .and(path("/v1/oauth/introspect"))
        .respond_with(move |_: &wiremock::Request| {
            active_response
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
        .with_priority(1)
        .mount(&h.accounts)
        .await;
    for reply in [
        json!({"active": false}),
        json!({"active": true, "client_id": "remind", "token_type": "refresh_token"}),
        json!({"active": true, "client_id": "other", "token_type": "access_token"}),
        json!({"active": true, "token_type": "access_token"}),
    ] {
        *response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            ResponseTemplate::new(200).set_body_json(reply);
        for (verb, uri) in [
            ("GET", "/api/v2/schedules"),
            ("POST", "/api/v2/schedules"),
            (
                "DELETE",
                "/api/v2/schedules/00000000-0000-0000-0000-000000000001",
            ),
        ] {
            let (status, body) = h.send(verb, uri, Some(&token), None, &[]).await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{verb} {uri}: {body}");
            assert_eq!(code(&body), "token_revoked");
        }
    }
    *response
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = ResponseTemplate::new(503);
    let (status, body) = h
        .send("GET", "/api/v2/auth/me", Some(&token), None, &[])
        .await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    Ok(())
}
