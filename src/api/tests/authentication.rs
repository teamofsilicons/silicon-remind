//! Access-token verification and API contract selection.

use http::StatusCode;
use serde_json::json;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path},
};

use super::{Harness, Signer, code};
use crate::domain::ActorKind;

#[tokio::test]
async fn valid_tokens_pass_and_bad_ones_get_precise_401s() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    harness
        .seed("Ada", ActorKind::Carbon, "c:ada", None)
        .await?;
    let me = "/api/v2/auth/me";

    let ok = harness.bearer("Ada", ActorKind::Carbon, "c:ada")?;
    let (status, body) = harness.send("GET", me, Some(&ok), None, &[]).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            body["uuid"].as_str(),
            body["kind"].as_str(),
            body["id"].as_str()
        ),
        (Some("Ada"), Some("carbon"), Some("c:ada"))
    );
    assert_eq!(body["credential"], "access_token");

    let mut wrong_audience = harness.claims("Ada", ActorKind::Carbon, "c:ada");
    wrong_audience["aud"] = json!("briefcase");
    let mut wrong_issuer = harness.claims("Ada", ActorKind::Carbon, "c:ada");
    wrong_issuer["iss"] = json!("https://accounts.example");
    let mut expired = harness.claims("Ada", ActorKind::Carbon, "c:ada");
    expired["exp"] = json!(chrono::Utc::now().timestamp() - 3600);
    for (claims, expected) in [
        (wrong_audience, "token_wrong_audience"),
        (wrong_issuer, "token_wrong_issuer"),
        (expired, "token_expired"),
    ] {
        let token = format!("Bearer {}", harness.signer.sign(&claims)?);
        let (status, body) = harness.send("GET", me, Some(&token), None, &[]).await?;
        assert_eq!(
            (status, code(&body)),
            (StatusCode::UNAUTHORIZED, expected),
            "{body}"
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.len() > 20)
        );
    }

    for (authorization, expected) in [
        (None, "unauthenticated"),
        (Some("Bearer oat_legacy_iam_token"), "iam_token_rejected"),
        (Some("Bearer not-a-jwt"), "token_malformed"),
        (Some("Basic YWRhOnB3"), "unauthenticated"),
    ] {
        let (status, body) = harness.send("GET", me, authorization, None, &[]).await?;
        assert_eq!(
            (status, code(&body)),
            (StatusCode::UNAUTHORIZED, expected),
            "{body}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_unknown_key_refetches_the_jwks_once_and_accepts_a_rotated_key() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    harness
        .seed("Ada", ActorKind::Carbon, "c:ada", None)
        .await?;
    let me = "/api/v2/auth/me";
    // Warm the cache with the first key.
    let (status, _) = harness
        .send(
            "GET",
            me,
            Some(&harness.bearer("Ada", ActorKind::Carbon, "c:ada")?),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::OK);

    // Silicon Accounts rotates: the JWKS now also publishes k2.
    let rotated = Signer::new(9, "k2")?;
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"keys": [harness.signer.jwk(), rotated.jwk()]})),
        )
        .with_priority(1)
        .mount(&harness.accounts)
        .await;
    let token = format!(
        "Bearer {}",
        rotated.sign(&harness.claims("Ada", ActorKind::Carbon, "c:ada"))?
    );
    let (status, body) = harness.send("GET", me, Some(&token), None, &[]).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "a rotated key is fetched on first sight: {body}"
    );

    // A made-up key id right after does not trigger another fetch.
    let forged = Signer::new(11, "k3")?;
    let token = format!(
        "Bearer {}",
        forged.sign(&harness.claims("Ada", ActorKind::Carbon, "c:ada"))?
    );
    let fetches_before = jwks_fetches(&harness).await;
    let (status, body) = harness.send("GET", me, Some(&token), None, &[]).await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNAUTHORIZED, "token_unknown_key")
    );
    assert_eq!(
        jwks_fetches(&harness).await,
        fetches_before,
        "refetching is rate limited"
    );
    Ok(())
}

async fn jwks_fetches(harness: &Harness) -> usize {
    harness
        .accounts
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == "/.well-known/jwks.json")
        .count()
}

#[tokio::test]
async fn api_v1_is_retired_and_v2_is_the_only_contract() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    harness
        .seed("Ada", ActorKind::Carbon, "c:ada", None)
        .await?;
    let token = harness.bearer("Ada", ActorKind::Carbon, "c:ada")?;
    let (status, body) = harness
        .send("GET", "/api/v1/schedules", Some(&token), None, &[])
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::GONE, "api_version_retired")
    );
    let (status, body) = harness
        .send(
            "GET",
            "/api/v2/schedules",
            Some(&token),
            None,
            &[("x-remind-api-version", "1")],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::NOT_ACCEPTABLE, "unsupported_api_version")
    );
    let (status, body) = harness
        .send(
            "GET",
            "/api/v2/schedules",
            Some(&token),
            None,
            &[("x-remind-api-version", "2")],
        )
        .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .send("GET", "/api/versions", None, None, &[])
        .await?;
    assert_eq!(
        (status, body["current"].as_i64()),
        (StatusCode::OK, Some(2))
    );
    for retired in [
        "/api/v1/auth/login",
        "/internal/v1/iam/events",
        "/internal/honeycomb/organizations/tos/testing-environments/x/operations/prepare",
    ] {
        let (status, _) = harness
            .send("POST", retired, None, Some(json!({})), &[])
            .await?;
        assert!(
            matches!(status, StatusCode::GONE | StatusCode::NOT_FOUND),
            "{retired} answered {status}"
        );
    }
    Ok(())
}
