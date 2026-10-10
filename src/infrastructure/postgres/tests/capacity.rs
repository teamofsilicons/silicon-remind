//! Capacity applies atomically to an account, including migrated storage keys.
use super::{FixedClock, actor, fixture_now, seed_account, service_audit, test_database};
use crate::{
    application::schedules::ScheduleService,
    domain::{ActorKind, CreateScheduleCommand, ScheduleKind},
    infrastructure::postgres::{NewHookDestination, RepositoryError},
};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[tokio::test]
async fn concurrent_creates_share_capacity_across_legacy_keys_and_replays_still_work()
-> anyhow::Result<()> {
    let database = test_database().await?;
    let key = seed_account(
        &database.pool,
        "Capacity",
        ActorKind::Silicon,
        "si:capacity",
        None,
    )
    .await?;
    let legacy = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO account_keys(storage_id,account_uuid,origin) VALUES($1,'Capacity','identity_link')",
    )
    .bind(legacy)
    .execute(&database.pool)
    .await?;
    // Archived reminders consume retained storage too.
    sqlx::query("INSERT INTO schedules(id,org_id,owner_principal_id,silicon_id,reminder_text,timezone,schedule_kind,cron_expression,status,created_at,deleted_at,purge_after) SELECT gen_random_uuid(),NULL,$1,'si:capacity','retained','UTC','one_time','* * * * *','paused',clock_timestamp()-interval '1 hour',clock_timestamp(),clock_timestamp()+interval '45 days' FROM generate_series(1,999)")
        .bind(legacy).execute(&database.pool).await?;
    let service = ScheduleService::new(
        database.repository.clone(),
        database.identity.clone(),
        Arc::new(FixedClock(fixture_now())),
        Duration::from_hours(24),
    );
    let owner = actor(
        &database.identity,
        "Capacity",
        ActorKind::Silicon,
        "si:capacity",
    )
    .await?;
    let command = || CreateScheduleCommand {
        text: "last slot".into(),
        timezone: "UTC".into(),
        kind: ScheduleKind::OneTime,
        cron: "* * * * *".into(),
    };
    let (first, second) = tokio::join!(
        service.create(&owner, command(), "capacity-a".into(), [11; 32]),
        service.create(&owner, command(), "capacity-b".into(), [12; 32]),
    );
    let (replay_key, replay_hash, created, refused) = match (first, second) {
        (Ok(created), Err(refused)) => ("capacity-a", [11; 32], created, refused),
        (Err(refused), Ok(created)) => ("capacity-b", [12; 32], created, refused),
        _ => anyhow::bail!("exactly one concurrent creation must get the last slot"),
    };
    assert_eq!(refused.code(), "account_reminder_limit");
    let replay = service
        .create(&owner, command(), replay_key.into(), replay_hash)
        .await?;
    assert_eq!(replay.body, created.body);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM schedules WHERE owner_principal_id=ANY($1)")
            .bind(vec![key, legacy])
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(count, 1000);
    seed_account(
        &database.pool,
        "Other",
        ActorKind::Silicon,
        "si:other",
        None,
    )
    .await?;
    let other = actor(&database.identity, "Other", ActorKind::Silicon, "si:other").await?;
    service
        .create(&other, command(), "other-key".into(), [13; 32])
        .await?;
    Ok(())
}

fn subscription(owner_key: Uuid) -> NewHookDestination {
    NewHookDestination {
        id: Uuid::now_v7(),
        owner_key,
        silicon_id: "si:capacity".into(),
        endpoint_url_ciphertext: vec![1],
        endpoint_url_nonce: [2; 12],
        signing_secret_ciphertext: vec![3],
        signing_secret_nonce: [4; 12],
        encryption_key_version: 1,
    }
}

#[tokio::test]
async fn concurrent_subscriptions_obey_capacity_and_disabled_rows_release_it() -> anyhow::Result<()>
{
    let database = test_database().await?;
    let key = seed_account(
        &database.pool,
        "Capacity",
        ActorKind::Silicon,
        "si:capacity",
        None,
    )
    .await?;
    let audit = service_audit();
    for _ in 0..19 {
        database
            .repository
            .upsert_hook_destination(&subscription(key), &audit)
            .await?;
    }
    let a = subscription(key);
    let b = subscription(key);
    let (first, second) = tokio::join!(
        database.repository.upsert_hook_destination(&a, &audit),
        database.repository.upsert_hook_destination(&b, &audit)
    );
    assert!(matches!(
        (&first, &second),
        (Ok(_), Err(RepositoryError::ResourceLimit("subscriptions")))
            | (Err(RepositoryError::ResourceLimit("subscriptions")), Ok(_))
    ));
    sqlx::query("UPDATE hook_destinations SET disabled_at=clock_timestamp() WHERE id=(SELECT id FROM hook_destinations WHERE owner_principal_id=$1 AND disabled_at IS NULL LIMIT 1)")
        .bind(key).execute(&database.pool).await?;
    database
        .repository
        .upsert_hook_destination(&subscription(key), &audit)
        .await?;
    Ok(())
}
