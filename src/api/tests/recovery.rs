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
async fn sensitive_routes_introspect_even_with_a_valid_signed_token() -> anyhow::Result<()> {
    let h = Harness::new().await?;
    h.seed("One", ActorKind::Silicon, "si:one", None).await?;
    let token = h.bearer("One", ActorKind::Silicon, "si:one")?;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"active":false})))
        .with_priority(1)
        .mount(&h.accounts)
        .await;
    for (verb, pattern) in crate::api::middleware::INTROSPECTED_ROUTES {
        let uri = pattern
            .replace("{schedule_id}", "00000000-0000-0000-0000-000000000001")
            .replace("{subscription_id}", "00000000-0000-0000-0000-000000000001")
            .replace("{id}", "00000000-0000-0000-0000-000000000001")
            .replace("{viewer}", "c:ada")
            .replace("{account}", "c:ada");
        let (status, response) = h.send(verb, &uri, Some(&token), None, &[]).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{verb} {uri}: {response}");
        assert_eq!(code(&response), "token_revoked", "{verb} {uri}: {response}");
    }
    assert_eq!(
        h.send("GET", "/api/v2/schedules", Some(&token), None, &[])
            .await?
            .0,
        StatusCode::OK
    );
    Ok(())
}
