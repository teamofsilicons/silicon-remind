//! Account registration from tokens, storage keys, circles and subscriptions.

use chrono::Utc;
use uuid::Uuid;

use super::{claims, fixture_now, seed_account, service_audit, test_database};
use crate::{
    domain::ActorKind,
    infrastructure::postgres::{NewHookDestination, RepositoryError},
};

#[tokio::test]
async fn a_first_token_registers_the_account_with_one_storage_key() -> anyhow::Result<()> {
    let database = test_database().await?;
    let token = claims(
        "Fresh",
        ActorKind::Silicon,
        "si:fresh",
        Utc::now().timestamp(),
    )?;
    let first = database.identity.resolve_bearer(&token).await?;
    let second = database.identity.resolve_bearer(&token).await?;
    assert_eq!(first.uuid, "Fresh");
    assert_eq!(first.public_id, "si:fresh");
    assert_eq!(first.storage_key, second.storage_key);
    assert_eq!(first.own_keys, vec![first.storage_key]);
    let keys: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_keys WHERE account_uuid = 'Fresh'")
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(keys, 1);
    // A token claiming another kind for the same uuid is refused.
    let wrong = claims(
        "Fresh",
        ActorKind::Carbon,
        "c:fresh",
        Utc::now().timestamp() + 1,
    )?;
    assert_eq!(
        database
            .identity
            .resolve_bearer(&wrong)
            .await
            .err()
            .map(|error| error.code()),
        Some("token_kind_mismatch".into())
    );
    Ok(())
}

#[tokio::test]
async fn circles_follow_custodians() -> anyhow::Result<()> {
    let database = test_database().await?;
    seed_account(&database.pool, "Ada", ActorKind::Carbon, "c:ada", None).await?;
    seed_account(
        &database.pool,
        "One",
        ActorKind::Silicon,
        "si:one",
        Some(("Ada", "c:ada")),
    )
    .await?;
    seed_account(
        &database.pool,
        "Two",
        ActorKind::Silicon,
        "si:two",
        Some(("Ada", "c:ada")),
    )
    .await?;
    seed_account(
        &database.pool,
        "Lone",
        ActorKind::Silicon,
        "si:lone",
        Some(("Bob", "c:bob")),
    )
    .await?;
    assert_eq!(
        database.identity.circle_of("One").await?,
        vec!["Ada", "One", "Two"]
    );
    assert_eq!(
        database.identity.circle_of("Ada").await?,
        vec!["Ada", "One", "Two"]
    );
    assert_eq!(database.identity.circle_of("Lone").await?, vec!["Lone"]);
    Ok(())
}

#[tokio::test]
async fn deliveries_reach_every_subscription_of_the_owner_account() -> anyhow::Result<()> {
    let database = test_database().await?;
    let primary = seed_account(
        &database.pool,
        "Multi",
        ActorKind::Silicon,
        "si:multi",
        None,
    )
    .await?;
    let legacy = Uuid::now_v7();
    sqlx::query("INSERT INTO account_keys (storage_id, account_uuid, origin) VALUES ($1, 'Multi', 'identity_link')")
        .bind(legacy)
        .execute(&database.pool)
        .await?;
    let audit = service_audit();
    let mut ids = Vec::new();
    for key in [primary, legacy] {
        let row = database
            .repository
            .upsert_hook_destination(
                &NewHookDestination {
                    id: Uuid::now_v7(),
                    owner_key: key,
                    silicon_id: "si:multi".to_owned(),
                    endpoint_url_ciphertext: vec![1],
                    endpoint_url_nonce: [2; 12],
                    signing_secret_ciphertext: vec![3],
                    signing_secret_nonce: [4; 12],
                    encryption_key_version: 1,
                },
                &audit,
            )
            .await?;
        assert_eq!((row.aad_version, row.org_id.as_deref()), (2, None));
        ids.push(row.id);
    }
    for key in [primary, legacy] {
        let reached = database
            .repository
            .destinations_for_owner_account(key)
            .await?;
        assert_eq!(reached.iter().map(|row| row.id).collect::<Vec<_>>(), ids);
    }
    // Someone else's keys cannot end a subscription; the owner's can.
    let other = seed_account(
        &database.pool,
        "Other",
        ActorKind::Silicon,
        "si:other",
        None,
    )
    .await?;
    assert!(matches!(
        database
            .repository
            .disable_hook_destination_by_id(&[other], ids[0], fixture_now(), &audit)
            .await,
        Err(RepositoryError::NotFound)
    ));
    assert!(
        database
            .repository
            .disable_hook_destination_by_id(&[primary, legacy], ids[0], fixture_now(), &audit)
            .await?
    );
    assert_eq!(
        database
            .repository
            .destinations_for_owner_account(primary)
            .await?
            .len(),
        1
    );
    Ok(())
}
