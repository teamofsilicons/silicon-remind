//! Authorization on every route family: owner, custodian, sibling, shared,
//! outsider, the Silicon allow-list, and test environments.

use http::StatusCode;
use serde_json::{Value, json};

use super::{Harness, code};
use crate::domain::ActorKind;

struct Cast {
    one: String,
    two: String,
    ada: String,
    zed: String,
    eve: String,
}

/// c:ada looks after si:one and si:two; c:zed is an outsider Carbon; si:eve is
/// another Carbon's Silicon.
async fn cast(harness: &Harness) -> anyhow::Result<Cast> {
    harness
        .seed("Ada", ActorKind::Carbon, "c:ada", None)
        .await?;
    harness
        .seed("One", ActorKind::Silicon, "si:one", Some(("Ada", "c:ada")))
        .await?;
    harness
        .seed("Two", ActorKind::Silicon, "si:two", Some(("Ada", "c:ada")))
        .await?;
    harness
        .seed("Zed", ActorKind::Carbon, "c:zed", None)
        .await?;
    harness
        .seed("Eve", ActorKind::Silicon, "si:eve", Some(("Ian", "c:ian")))
        .await?;
    harness
        .lookup("Zed", ActorKind::Carbon, "c:zed", None)
        .await;
    harness
        .lookup("Eve", ActorKind::Silicon, "si:eve", Some(("Ian", "c:ian")))
        .await;
    harness
        .lookup("One", ActorKind::Silicon, "si:one", Some(("Ada", "c:ada")))
        .await;
    Ok(Cast {
        one: harness.bearer("One", ActorKind::Silicon, "si:one")?,
        two: harness.bearer("Two", ActorKind::Silicon, "si:two")?,
        ada: harness.bearer("Ada", ActorKind::Carbon, "c:ada")?,
        zed: harness.bearer("Zed", ActorKind::Carbon, "c:zed")?,
        eve: harness.bearer("Eve", ActorKind::Silicon, "si:eve")?,
    })
}

fn reminder() -> Value {
    json!({"text": "Stand up", "kind": "recurring", "cron": "0 9 * * *", "timezone": "Asia/Kolkata"})
}

async fn listed(
    harness: &Harness,
    token: &str,
    extra: &[(&str, &str)],
) -> anyhow::Result<Vec<Value>> {
    let (status, body) = harness
        .send("GET", "/api/v2/schedules", Some(token), None, extra)
        .await?;
    anyhow::ensure!(status == StatusCode::OK, "list answered {status}: {body}");
    Ok(body["items"].as_array().cloned().unwrap_or_default())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One cast walks every reminder route family in order"
)]
async fn reminders_follow_owner_custodian_sibling_and_outsider_rules() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    let cast = cast(&harness).await?;
    let key = [("idempotency-key", "create-reminder-1")];

    let (status, created) = harness
        .send(
            "POST",
            "/api/v2/schedules",
            Some(&cast.one),
            Some(reminder()),
            &key,
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        (
            created["owner"]["uuid"].as_str(),
            created["silicon_id"].as_str()
        ),
        (Some("One"), Some("si:one"))
    );
    assert!(created.get("org_id").is_none());
    let id = created["id"].as_str().unwrap_or_default().to_owned();

    let (status, body) = harness
        .send(
            "POST",
            "/api/v2/schedules",
            Some(&cast.ada),
            Some(reminder()),
            &[("idempotency-key", "carbon-create-1")],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "silicon_only")
    );

    for (token, expected) in [
        (&cast.one, 1),
        (&cast.ada, 1),
        (&cast.two, 1),
        (&cast.zed, 0),
        (&cast.eve, 0),
    ] {
        assert_eq!(listed(&harness, token, &[]).await?.len(), expected);
    }
    let one_url = format!("/api/v2/schedules/{id}");
    let (status, _) = harness
        .send("GET", &one_url, Some(&cast.zed), None, &[])
        .await?;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "outsiders cannot tell it exists"
    );
    let (status, _) = harness
        .send(
            "GET",
            &format!("{one_url}/executions"),
            Some(&cast.ada),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::OK);
    let (status, filtered) = harness
        .send(
            "GET",
            "/api/v2/schedules?silicon_id=si:two",
            Some(&cast.ada),
            None,
            &[],
        )
        .await?;
    assert_eq!(
        (status, filtered["items"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(0))
    );

    let patch = json!({"text": "Stand up now"});
    let (status, body) = harness
        .send(
            "PATCH",
            &one_url,
            Some(&cast.two),
            Some(patch.clone()),
            &[("idempotency-key", "sibling-patch-1")],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "not_reminder_owner")
    );
    let (status, body) = harness
        .send(
            "PATCH",
            &one_url,
            Some(&cast.ada),
            Some(patch.clone()),
            &[("idempotency-key", "custodian-patch-1")],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "silicon_only")
    );
    let bulk = json!({"schedule_ids": [id], "status": "paused"});
    let (status, body) = harness
        .send(
            "PATCH",
            "/api/v2/schedules",
            Some(&cast.two),
            Some(bulk.clone()),
            &[("idempotency-key", "sibling-bulk-1")],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "not_reminder_owner")
    );
    let (status, body) = harness
        .send(
            "PATCH",
            "/api/v2/schedules",
            Some(&cast.one),
            Some(bulk),
            &[("idempotency-key", "owner-bulk-1")],
        )
        .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = harness
        .send(
            "PATCH",
            &one_url,
            Some(&cast.one),
            Some(patch),
            &[("idempotency-key", "owner-patch-1")],
        )
        .await?;
    assert_eq!(
        (status, body["text"].as_str()),
        (StatusCode::OK, Some("Stand up now"))
    );

    // Silicons the caller sees, with the relation that explains why.
    let (_, silicons) = harness
        .send("GET", "/api/v2/silicons", Some(&cast.ada), None, &[])
        .await?;
    let relations = silicons["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            (
                item["silicon_id"].as_str().unwrap_or_default().to_owned(),
                item["relation"].as_str().unwrap_or_default().to_owned(),
                item["reminder_count"].as_i64().unwrap_or(-1),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relations,
        vec![
            ("si:one".to_owned(), "custodian".to_owned(), 1),
            ("si:two".to_owned(), "custodian".to_owned(), 0)
        ]
    );
    let (_, mine) = harness
        .send("GET", "/api/v2/silicons", Some(&cast.two), None, &[])
        .await?;
    assert_eq!(mine["items"].as_array().map(Vec::len), Some(2));

    // Archive: the custodian may not; the owner may.
    let (status, body) = harness
        .send("DELETE", &one_url, Some(&cast.ada), None, &[])
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "silicon_only")
    );
    let (status, _) = harness
        .send("DELETE", &one_url, Some(&cast.one), None, &[])
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let archived = listed(&harness, &cast.ada, &[]).await?;
    assert!(archived.is_empty());
    Ok(())
}

#[tokio::test]
async fn subscriptions_are_managed_by_the_silicon_and_listed_by_its_custodian() -> anyhow::Result<()>
{
    let harness = Harness::new().await?;
    let cast = cast(&harness).await?;
    let hook = json!({"endpoint_url": "http://127.0.0.1:9/reminders", "signing_secret": "a-signing-secret"});
    let (status, created) = harness
        .send(
            "POST",
            "/api/v2/webhooks",
            Some(&cast.one),
            Some(hook.clone()),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        (
            created["silicon_id"].as_str(),
            created["silicon_uuid"].as_str()
        ),
        (Some("si:one"), Some("One"))
    );
    let (status, body) = harness
        .send("POST", "/api/v2/webhooks", Some(&cast.ada), Some(hook), &[])
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "silicon_only")
    );

    let (status, listed) = harness
        .send("GET", "/api/v2/webhooks", Some(&cast.ada), None, &[])
        .await?;
    assert_eq!(status, StatusCode::OK);
    let items = listed["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(items.len(), 1);
    assert_eq!(
        (
            items[0]["endpoint_url"].as_str(),
            items[0]["owner"]["uuid"].as_str()
        ),
        (Some("http://127.0.0.1:9/reminders"), Some("One"))
    );
    assert!(items[0].get("signing_secret").is_none());
    let (status, body) = harness
        .send(
            "GET",
            "/api/v2/webhooks?silicon_id=si:one",
            Some(&cast.zed),
            None,
            &[],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "subscriptions_not_visible")
    );

    let id = created["id"].as_str().unwrap_or_default();
    let (status, _) = harness
        .send(
            "DELETE",
            &format!("/api/v2/webhooks/{id}"),
            Some(&cast.two),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = harness
        .send(
            "DELETE",
            &format!("/api/v2/webhooks/{id}"),
            Some(&cast.one),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One sharing story from grant to allow-list removal"
)]
async fn sharing_reaches_carbons_freely_and_silicons_only_when_allowed() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    let cast = cast(&harness).await?;
    harness
        .send(
            "POST",
            "/api/v2/schedules",
            Some(&cast.one),
            Some(reminder()),
            &[("idempotency-key", "share-reminder-1")],
        )
        .await?;

    let (status, grant) = harness
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&cast.one),
            Some(json!({"id": "c:zed"})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED, "{grant}");
    assert_eq!(listed(&harness, &cast.zed, &[]).await?.len(), 1);
    let (status, body) = harness
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&cast.ada),
            Some(json!({"id": "c:zed"})),
            &[],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNPROCESSABLE_ENTITY, "silicon_id_required")
    );
    let (status, _) = harness
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&cast.ada),
            Some(json!({"id": "c:zed", "silicon_id": "si:one"})),
            &[],
        )
        .await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "the custodian manages its Silicon's grants; this one exists"
    );
    let (status, body) = harness
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&cast.zed),
            Some(json!({"id": "c:ada", "silicon_id": "si:one"})),
            &[],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "not_custodian")
    );

    // A Silicon outside the circle receives nothing it has not allowed.
    let (status, body) = harness
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&cast.one),
            Some(json!({"id": "si:eve"})),
            &[],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "silicon_not_open")
    );
    let (status, _) = harness
        .send(
            "POST",
            "/api/v2/allowed-accounts",
            Some(&cast.eve),
            Some(json!({"id": "si:one"})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = harness
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&cast.one),
            Some(json!({"id": "si:eve"})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(listed(&harness, &cast.eve, &[]).await?.len(), 1);
    let (_, viewers) = harness
        .send("GET", "/api/v2/viewers", Some(&cast.eve), None, &[])
        .await?;
    assert_eq!(viewers["received"].as_array().map(Vec::len), Some(1));

    // Removing the allowance ends the shares it made possible.
    let (status, _) = harness
        .send(
            "DELETE",
            "/api/v2/allowed-accounts/si:one",
            Some(&cast.eve),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(listed(&harness, &cast.eve, &[]).await?.is_empty());
    let (status, _) = harness
        .send(
            "DELETE",
            "/api/v2/viewers/c:zed",
            Some(&cast.one),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(listed(&harness, &cast.zed, &[]).await?.is_empty());
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One environment from creation to sandboxed reads"
)]
async fn test_environments_belong_to_accounts_and_open_with_their_key() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    let cast = cast(&harness).await?;
    let (status, created) = harness
        .send(
            "POST",
            "/api/v2/test-environments",
            Some(&cast.one),
            Some(json!({"name": "staging"})),
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["environment"]["owner"]["uuid"], "One");
    let id = created["environment"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let (_, listed_by_ada) = harness
        .send(
            "GET",
            "/api/v2/test-environments",
            Some(&cast.ada),
            None,
            &[],
        )
        .await?;
    assert_eq!(listed_by_ada["items"].as_array().map(Vec::len), Some(1));
    let (status, _) = harness
        .send(
            "GET",
            &format!("/api/v2/test-environments/{id}/key"),
            Some(&cast.two),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::OK, "the owner's circle may use it");
    let (status, body) = harness
        .send(
            "POST",
            &format!("/api/v2/test-environments/{id}/key-rotations"),
            Some(&cast.two),
            None,
            &[],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "not_environment_manager")
    );
    let (status, rotated) = harness
        .send(
            "POST",
            &format!("/api/v2/test-environments/{id}/key-rotations"),
            Some(&cast.ada),
            None,
            &[],
        )
        .await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "the custodian manages its Silicon's environment"
    );
    let (status, _) = harness
        .send(
            "GET",
            &format!("/api/v2/test-environments/{id}"),
            Some(&cast.zed),
            None,
            &[],
        )
        .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let key = rotated["key"].as_str().unwrap_or_default().to_owned();
    let in_test = [
        ("x-remind-test-key", key.as_str()),
        ("idempotency-key", "sandbox-create-1"),
    ];
    let (status, sandboxed) = harness
        .send(
            "POST",
            "/api/v2/schedules",
            Some(&cast.one),
            Some(reminder()),
            &in_test,
        )
        .await?;
    assert_eq!(status, StatusCode::CREATED, "{sandboxed}");
    // Whoever holds the key reads everything inside, and acts as themselves.
    assert_eq!(
        listed(&harness, &cast.zed, &[("x-remind-test-key", key.as_str())])
            .await?
            .len(),
        1
    );
    assert!(
        listed(&harness, &cast.one, &[]).await?.is_empty(),
        "test data stays out of production"
    );
    let sandbox_url = format!(
        "/api/v2/schedules/{}",
        sandboxed["id"].as_str().unwrap_or_default()
    );
    let (status, body) = harness
        .send(
            "PATCH",
            &sandbox_url,
            Some(&cast.eve),
            Some(json!({"text": "x"})),
            &[
                ("x-remind-test-key", key.as_str()),
                ("idempotency-key", "sandbox-patch-1"),
            ],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "not_reminder_owner")
    );
    let (status, body) = harness
        .send(
            "POST",
            "/api/v2/viewers",
            Some(&cast.one),
            Some(json!({"id": "c:zed"})),
            &[("x-remind-test-key", key.as_str())],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::FORBIDDEN, "not_in_test_environment")
    );
    let (status, current) = harness
        .send(
            "GET",
            "/api/v2/testing-environment",
            None,
            None,
            &[("x-remind-test-key", key.as_str())],
        )
        .await?;
    assert_eq!(
        (status, current["id"].as_str()),
        (StatusCode::OK, Some(id.as_str()))
    );
    let (status, body) = harness
        .send(
            "GET",
            "/api/v2/testing-environment",
            Some(&cast.one),
            None,
            &[],
        )
        .await?;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::BAD_REQUEST, "test_environment_required")
    );
    Ok(())
}
