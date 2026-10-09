//! `remind-migrate link-identities` over IAM-era rows.

use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::{actor, due_ids, fixture_now, test_database};
use crate::{
    domain::ActorKind,
    infrastructure::{
        accounts::AccountsGateway,
        identity_links::{link_identities, parse_mapping},
        testing::TestEnvironments,
    },
};

struct Legacy {
    scout: Uuid,
    ada: Uuid,
    orphan: Uuid,
    schedule: Uuid,
}

async fn seed_legacy(pool: &sqlx::PgPool) -> anyhow::Result<Legacy> {
    let legacy = Legacy {
        scout: Uuid::now_v7(),
        ada: Uuid::now_v7(),
        orphan: Uuid::now_v7(),
        schedule: Uuid::now_v7(),
    };
    let due = fixture_now() - chrono::Duration::minutes(1);
    sqlx::query("INSERT INTO organization_lifecycle (org_id, state) VALUES ('tos', 'active')")
        .execute(pool)
        .await?;
    for (key, id) in [(legacy.scout, "si:scout"), (legacy.orphan, "si:orphan")] {
        sqlx::query("INSERT INTO silicon_identities (org_id, principal_id, silicon_id, state) VALUES ('tos', $1, $2, 'active')")
            .bind(key).bind(id).execute(pool).await?;
        sqlx::query("INSERT INTO iam_identity_bindings (identity_kind, public_id, local_id) VALUES ('silicon', $1, $2)")
            .bind(id).bind(key).execute(pool).await?;
    }
    sqlx::query("INSERT INTO iam_identity_bindings (identity_kind, public_id, local_id) VALUES ('carbon', 'c:ada', $1)")
        .bind(legacy.ada).execute(pool).await?;
    for (schedule, key, id) in [
        (legacy.schedule, legacy.scout, "si:scout"),
        (Uuid::now_v7(), legacy.orphan, "si:orphan"),
    ] {
        sqlx::query(
            "INSERT INTO schedules (id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
                 schedule_kind, cron_expression, status, next_run_at) \
             VALUES ($1, 'tos', $2, $3, 'legacy', 'UTC', 'recurring', '* * * * *', 'active', $4)",
        )
        .bind(schedule).bind(key).bind(id).bind(due).execute(pool).await?;
    }
    sqlx::query(
        "INSERT INTO hook_destinations (id, org_id, owner_principal_id, silicon_id, endpoint_url_ciphertext, \
             endpoint_url_nonce, signing_secret_ciphertext, signing_secret_nonce, encryption_key_version) \
         VALUES ($1, 'tos', $2, 'si:scout', '\\x01', '\\x000000000000000000000000', '\\x02', '\\x000000000000000000000000', 1)",
    )
    .bind(Uuid::now_v7()).bind(legacy.scout).execute(pool).await?;
    Ok(legacy)
}

#[test]
fn the_mapping_file_is_parsed_strictly() {
    let parsed =
        parse_mapping("iam_principal_id,accounts_uuid\n# comment\nsi:scout,Scout\nc:ada,-\n\n");
    assert!(parsed.as_ref().is_ok_and(|entries| entries.len() == 2
        && entries[0].accounts_uuid.as_deref() == Some("Scout")
        && entries[1].accounts_uuid.is_none()));
    let errors = parse_mapping("si:scout\nnot-an-id,Scout\nsi:scout,zQ-o\nsi:a,b,c")
        .err()
        .unwrap_or_default();
    assert_eq!(errors.len(), 4, "{errors:?}");
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One mapping's whole life: dry run, apply, re-run, correct, unlink, refuse"
)]
async fn linking_rekeys_legacy_rows_without_rewriting_them() -> anyhow::Result<()> {
    let database = test_database().await?;
    let legacy = seed_legacy(&database.pool).await?;
    let testing_database = crate::test_support::TestPostgres::start().await?;
    let testing = testing_database.pool(2).await?;
    TestEnvironments::migrate(&testing).await?;
    let environment = Uuid::now_v7();
    sqlx::query("INSERT INTO public.testing_environments (id, org_id, creator_id, name, iam_environment_id, key_hash, secrets) VALUES ($1, 'tos', $2, 'legacy env', $3, $4, '{}'::jsonb)")
        .bind(environment).bind(legacy.scout.to_string()).bind(Uuid::now_v7()).bind(vec![1_u8; 32])
        .execute(&testing).await?;

    let mapping = parse_mapping(&format!(
        "iam_principal_id,accounts_uuid\nsi:scout,Scout\n{},Ada\n",
        legacy.ada
    ))
    .map_err(|errors| anyhow::anyhow!("{errors:?}"))?;
    let dry = link_identities(&database.pool, Some(&testing), None, &mapping, "test", true).await?;
    assert!(dry.dry_run && !dry.committed);
    assert_eq!(dry.linked.len(), 2);
    assert_eq!(dry.linked[0].schedules + dry.linked[1].schedules, 1);
    let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM account_keys")
        .fetch_one(&database.pool)
        .await?;
    assert_eq!(keys, 0, "a dry run changes nothing");

    let applied = link_identities(
        &database.pool,
        Some(&testing),
        None,
        &mapping,
        "test",
        false,
    )
    .await?;
    assert!(applied.committed);
    assert_eq!(
        applied
            .unmatched
            .iter()
            .map(|u| u.iam_principal_id)
            .collect::<Vec<_>>(),
        vec![legacy.orphan]
    );
    assert_eq!(applied.test_environments_linked, 1);
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_uuid FROM public.testing_environments WHERE id = $1")
            .bind(environment)
            .fetch_one(&testing)
            .await?;
    assert_eq!(owner.as_deref(), Some("Scout"));
    let kinds: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT uuid, kind FROM accounts ORDER BY uuid")
            .fetch_all(&database.pool)
            .await?;
    assert_eq!(
        kinds,
        vec![
            ("Ada".to_owned(), Some("carbon".to_owned())),
            ("Scout".to_owned(), Some("silicon".to_owned()))
        ]
    );
    let scout = actor(&database.identity, "Scout", ActorKind::Silicon, "si:scout").await?;
    assert!(scout.owns_key(legacy.scout));
    assert!(
        database
            .repository
            .get_schedule(scout.read_keys().as_deref(), legacy.schedule)
            .await?
            .is_some()
    );
    let untouched: (Option<String>, Uuid, String) = sqlx::query_as(
        "SELECT org_id, owner_principal_id, silicon_id FROM schedules WHERE id = $1",
    )
    .bind(legacy.schedule)
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(
        untouched,
        (Some("tos".to_owned()), legacy.scout, "si:scout".to_owned()),
        "rows are never rewritten"
    );
    assert!(
        due_ids(&database.repository, fixture_now())
            .await?
            .contains(&legacy.schedule)
    );

    let again = link_identities(
        &database.pool,
        Some(&testing),
        None,
        &mapping,
        "test",
        false,
    )
    .await?;
    assert_eq!(
        (again.linked.len(), again.unchanged),
        (0, 2),
        "re-running is idempotent"
    );

    // A corrected mapping re-points the same key; an empty account unlinks it.
    let corrected =
        parse_mapping("si:scout,Scout2\n").map_err(|errors| anyhow::anyhow!("{errors:?}"))?;
    link_identities(
        &database.pool,
        Some(&testing),
        None,
        &corrected,
        "test",
        false,
    )
    .await?;
    let now_owner: String =
        sqlx::query_scalar("SELECT account_uuid FROM account_keys WHERE storage_id = $1")
            .bind(legacy.scout)
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(now_owner, "Scout2");
    let unlink = parse_mapping("si:scout,-\n").map_err(|errors| anyhow::anyhow!("{errors:?}"))?;
    let removed =
        link_identities(&database.pool, Some(&testing), None, &unlink, "test", false).await?;
    assert_eq!(removed.unlinked, vec![legacy.scout.to_string()]);

    // Any refused line rolls the whole run back.
    let refused = parse_mapping(&format!("si:orphan,Orphan\n{},Nobody\n", Uuid::now_v7()))
        .map_err(|errors| anyhow::anyhow!("{errors:?}"))?;
    let report = link_identities(
        &database.pool,
        Some(&testing),
        None,
        &refused,
        "test",
        false,
    )
    .await?;
    assert!(
        !report.committed && report.refused.len() == 1,
        "{:?}",
        report.refused
    );
    let orphan_keys: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_keys WHERE storage_id = $1")
            .bind(legacy.orphan)
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(orphan_keys, 0);
    Ok(())
}

#[tokio::test]
async fn online_linking_checks_each_account_with_silicon_accounts() -> anyhow::Result<()> {
    let database = test_database().await?;
    let legacy = seed_legacy(&database.pool).await?;
    let accounts = MockServer::start().await;
    for (uuid, kind, id) in [
        ("Scout", "silicon", "si:scout"),
        ("Carb", "carbon", "c:carb"),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/v1/accounts/{uuid}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uuid": uuid, "kind": kind, "id": id, "display_name": id, "pfp_url": "", "status": "active",
                "custodian": if kind == "silicon" { serde_json::json!({"uuid": "Ada", "id": "c:ada"}) } else { serde_json::Value::Null }
            })))
            .mount(&accounts)
            .await;
    }
    let gateway = AccountsGateway::new(&crate::test_support::accounts_settings(&accounts.uri())?)?;
    let mismatched =
        parse_mapping("si:orphan,Carb\n").map_err(|errors| anyhow::anyhow!("{errors:?}"))?;
    let report = link_identities(
        &database.pool,
        None,
        Some(&gateway),
        &mismatched,
        "test",
        false,
    )
    .await?;
    assert!(
        report
            .refused
            .iter()
            .any(|reason| reason.contains("was an IAM silicon")),
        "{:?}",
        report.refused
    );
    let mapping =
        parse_mapping("si:scout,Scout\n").map_err(|errors| anyhow::anyhow!("{errors:?}"))?;
    let report = link_identities(
        &database.pool,
        None,
        Some(&gateway),
        &mapping,
        "test",
        false,
    )
    .await?;
    assert!(report.committed);
    assert_eq!(report.linked[0].accounts_id.as_deref(), Some("si:scout"));
    let custodian: Option<String> =
        sqlx::query_scalar("SELECT custodian_uuid FROM accounts WHERE uuid = 'Scout'")
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(custodian.as_deref(), Some("Ada"));
    let _ = legacy;
    Ok(())
}
