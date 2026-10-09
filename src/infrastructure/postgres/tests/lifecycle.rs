//! Silicon Accounts lifecycle events applied to Remind's data, and rows from
//! before the move to Silicon Accounts.

use std::sync::Arc;

use chrono::{Duration, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{
    FixedClock, actor, claims, due_ids, event, fixture_now, seed_account, seed_due_schedule,
    seed_execution, test_database,
};
use crate::{
    application::schedules::ScheduleService,
    domain::{ActorKind, CreateScheduleCommand, ScheduleKind},
    infrastructure::{
        account_events::{EventOutcome, apply},
        postgres::{NewHookDestination, RepositoryError},
    },
};

fn destination(owner_key: Uuid) -> NewHookDestination {
    NewHookDestination {
        id: Uuid::now_v7(),
        owner_key,
        silicon_id: "si:doomed".to_owned(),
        endpoint_url_ciphertext: vec![1],
        endpoint_url_nonce: [2; 12],
        signing_secret_ciphertext: vec![3],
        signing_secret_nonce: [4; 12],
        encryption_key_version: 1,
    }
}

#[tokio::test]
async fn account_deletion_archives_disables_revokes_and_is_replay_safe() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    let (key, schedule) =
        seed_due_schedule(&database.pool, "Doom", "si:doomed", now, "recurring").await?;
    let pending = seed_execution(&database.pool, schedule, "si:doomed", now).await?;
    database
        .repository
        .upsert_hook_destination(&destination(key), &super::service_audit())
        .await?;
    seed_account(&database.pool, "Ada", ActorKind::Carbon, "c:ada", None).await?;
    crate::infrastructure::sharing::grant(&database.pool, "Doom", "Ada", "Doom").await?;

    let (deleted, body) = event(
        "evt-delete",
        "account.deleted",
        now,
        &json!({"uuid": "Doom", "membership_id": "remind:Doom"}),
    )?;
    assert_eq!(
        apply(&database.identity, None, &deleted, &body).await?,
        EventOutcome::Processed
    );
    assert_eq!(
        apply(&database.identity, None, &deleted, &body).await?,
        EventOutcome::Duplicate
    );

    let (status, public_id, cutoff): (String, String, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as(
            "SELECT status, public_id, revoked_before FROM accounts WHERE uuid = 'Doom'",
        )
        .fetch_one(&database.pool)
        .await?;
    assert_eq!((status.as_str(), public_id.as_str()), ("deleted", ""));
    assert!(cutoff.is_some());
    let deleted_at: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM schedules WHERE id = $1")
            .bind(schedule)
            .fetch_one(&database.pool)
            .await?;
    assert!(
        deleted_at.is_some(),
        "the reminder is archived (45-day retention, then the ledger)"
    );
    let (execution_status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, failure_reason FROM executions WHERE id = $1")
            .bind(pending)
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(
        (execution_status.as_str(), reason.as_deref()),
        ("failed", Some("stopped after the account was deleted"))
    );
    assert!(
        database
            .repository
            .get_hook_destinations(&[key])
            .await?
            .is_empty()
    );
    assert!(
        crate::infrastructure::sharing::grants_to(&database.pool, "Ada")
            .await?
            .is_empty()
    );
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM internal_event_receipts WHERE source = 'silicon-accounts'",
    )
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(receipts, 1);

    // Its tokens are refused from now on, and nothing more can be created.
    let error = database
        .identity
        .resolve_bearer(&claims(
            "Doom",
            ActorKind::Silicon,
            "si:doomed",
            Utc::now().timestamp() + 5,
        )?)
        .await
        .err()
        .map(|error| error.code());
    assert_eq!(error, Some("account_deleted".into()));
    Ok(())
}

#[tokio::test]
async fn access_removal_suspends_until_the_next_sign_in() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    let (_, schedule) =
        seed_due_schedule(&database.pool, "Away", "si:away", now, "recurring").await?;
    let pending = seed_execution(
        &database.pool,
        schedule,
        "si:away",
        now - Duration::minutes(5),
    )
    .await?;
    // The last token the account used was issued before the removal.
    sqlx::query("UPDATE accounts SET last_token_iat = $1 WHERE uuid = 'Away'")
        .bind(now - Duration::minutes(10))
        .execute(&database.pool)
        .await?;
    let (removed, body) = event(
        "evt-removed",
        "membership.access_removed",
        now,
        &json!({"uuid": "Away", "membership_id": "remind:Away"}),
    )?;
    assert_eq!(
        apply(&database.identity, None, &removed, &body).await?,
        EventOutcome::Processed
    );

    assert!(
        !due_ids(&database.repository, now)
            .await?
            .contains(&schedule),
        "suspended reminders do not fire"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM executions WHERE id = $1")
        .bind(pending)
        .fetch_one(&database.pool)
        .await?;
    assert_eq!(status, "failed");
    let old_token = claims(
        "Away",
        ActorKind::Silicon,
        "si:away",
        (now - Duration::minutes(1)).timestamp(),
    )?;
    assert_eq!(
        database
            .identity
            .resolve_bearer(&old_token)
            .await
            .err()
            .map(|e| e.code()),
        Some("token_revoked".into())
    );

    // Signing in again (a token issued after the removal) ends the suspension.
    let fresh = claims(
        "Away",
        ActorKind::Silicon,
        "si:away",
        (now + Duration::seconds(2)).timestamp(),
    )?;
    let actor = database.identity.resolve_bearer(&fresh).await?;
    assert_eq!(actor.uuid, "Away");
    assert!(
        due_ids(&database.repository, now)
            .await?
            .contains(&schedule)
    );
    Ok(())
}

#[tokio::test]
async fn sign_outs_cut_off_older_tokens_except_remind_revoking_one_sign_in() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    seed_account(&database.pool, "Out", ActorKind::Silicon, "si:out", None).await?;
    let (own, body) = event(
        "evt-own",
        "membership.signed_out",
        now,
        &json!({"uuid": "Out", "membership_id": "remind:Out", "reason": "app_revoked"}),
    )?;
    assert_eq!(
        apply(&database.identity, None, &own, &body).await?,
        EventOutcome::Ignored
    );
    let old = claims(
        "Out",
        ActorKind::Silicon,
        "si:out",
        (now - Duration::minutes(1)).timestamp(),
    )?;
    assert!(
        database.identity.resolve_bearer(&old).await.is_ok(),
        "app_revoked ends one sign-in only"
    );

    let (rotated, body) = event(
        "evt-stk",
        "membership.signed_out",
        now,
        &json!({"uuid": "Out", "membership_id": "remind:Out", "reason": "stk_rotated"}),
    )?;
    assert_eq!(
        apply(&database.identity, None, &rotated, &body).await?,
        EventOutcome::Processed
    );
    assert_eq!(
        database
            .identity
            .resolve_bearer(&old)
            .await
            .err()
            .map(|e| e.code()),
        Some("token_revoked".into())
    );
    let same_second = claims("Out", ActorKind::Silicon, "si:out", now.timestamp())?;
    assert!(
        database
            .identity
            .resolve_bearer(&same_second)
            .await
            .is_err(),
        "a token from the cutoff second is refused"
    );
    let after = claims("Out", ActorKind::Silicon, "si:out", now.timestamp() + 1)?;
    assert!(database.identity.resolve_bearer(&after).await.is_ok());

    // An event for an account Remind never saw still records the cutoff.
    let (unknown, body) = event(
        "evt-unknown",
        "membership.signed_out",
        now,
        &json!({"uuid": "Never", "membership_id": "remind:Never", "reason": "session_revoked"}),
    )?;
    assert_eq!(
        apply(&database.identity, None, &unknown, &body).await?,
        EventOutcome::Processed
    );
    let never = claims("Never", ActorKind::Carbon, "c:never", now.timestamp() - 30)?;
    assert_eq!(
        database
            .identity
            .resolve_bearer(&never)
            .await
            .err()
            .map(|e| e.code()),
        Some("token_revoked".into())
    );
    Ok(())
}

#[tokio::test]
async fn id_and_custodian_changes_apply_in_occurrence_order() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    seed_account(&database.pool, "Ada", ActorKind::Carbon, "c:ada", None).await?;
    seed_account(&database.pool, "Bob", ActorKind::Carbon, "c:bob", None).await?;
    seed_account(
        &database.pool,
        "Scout",
        ActorKind::Silicon,
        "si:scout",
        Some(("Ada", "c:ada")),
    )
    .await?;
    sqlx::query("UPDATE accounts SET id_observed_at = NULL, custodian_observed_at = NULL")
        .execute(&database.pool)
        .await?;
    // Silicon Accounts is unreachable here, so the events' own data is applied.
    let (renamed, body) = event(
        "evt-id-2",
        "account.id_changed",
        now,
        &json!({"uuid": "Scout", "membership_id": "remind:Scout", "kind": "silicon", "old_id": "si:scout", "new_id": "si:scout-two"}),
    )?;
    apply(&database.identity, None, &renamed, &body).await?;
    let (stale, body) = event(
        "evt-id-1",
        "account.id_changed",
        now - Duration::seconds(5),
        &json!({"uuid": "Scout", "membership_id": "remind:Scout", "kind": "silicon", "old_id": "si:old", "new_id": "si:scout"}),
    )?;
    apply(&database.identity, None, &stale, &body).await?;
    let id: String = sqlx::query_scalar("SELECT public_id FROM accounts WHERE uuid = 'Scout'")
        .fetch_one(&database.pool)
        .await?;
    assert_eq!(
        id, "si:scout-two",
        "a delayed older id change does not undo a newer one"
    );

    let (moved, body) = event(
        "evt-cust",
        "silicon.custodian_changed",
        now,
        &json!({"uuid": "Scout", "membership_id": "remind:Scout", "from": {"uuid": "Ada", "id": "c:ada"}, "to": {"uuid": "Bob", "id": "c:bob"}}),
    )?;
    apply(&database.identity, None, &moved, &body).await?;
    let bob = actor(&database.identity, "Bob", ActorKind::Carbon, "c:bob").await?;
    let ada = actor(&database.identity, "Ada", ActorKind::Carbon, "c:ada").await?;
    assert!(bob.looks_after("Scout") && !ada.looks_after("Scout"));
    Ok(())
}

#[tokio::test]
async fn first_reminder_needs_no_webhook_and_a_deleted_owner_cannot_create() -> anyhow::Result<()> {
    let database = test_database().await?;
    let now = fixture_now();
    seed_account(
        &database.pool,
        "First",
        ActorKind::Silicon,
        "si:first",
        None,
    )
    .await?;
    let service = ScheduleService::new(
        database.repository.clone(),
        database.identity.clone(),
        Arc::new(FixedClock(now)),
        std::time::Duration::from_hours(24),
    );
    let first = actor(&database.identity, "First", ActorKind::Silicon, "si:first").await?;
    let command = || CreateScheduleCommand {
        text: "No subscribers required".to_owned(),
        timezone: "UTC".to_owned(),
        kind: ScheduleKind::OneTime,
        cron: "* * * * *".to_owned(),
    };
    let created = service
        .create(&first, command(), "first-create".to_owned(), [91; 32])
        .await?;
    assert_eq!(created.status_code, 201);
    assert_eq!(created.body["owner"]["uuid"], "First");
    assert_eq!(created.body["silicon_id"], "si:first");
    assert!(created.body.get("org_id").is_none());
    let replay = service
        .create(&first, command(), "first-create".to_owned(), [91; 32])
        .await?;
    assert_eq!(replay.body, created.body);

    // A deletion that lands between authentication and the write is refused by the write fence.
    sqlx::query("UPDATE accounts SET status = 'deleted' WHERE uuid = 'First'")
        .execute(&database.pool)
        .await?;
    let refused = service
        .create(&first, command(), "after-delete".to_owned(), [92; 32])
        .await;
    assert!(matches!(refused, Err(error) if error.code() == "silicon_unavailable"));
    assert!(matches!(
        database
            .repository
            .upsert_hook_destination(&destination(first.storage_key), &super::service_audit())
            .await,
        Err(RepositoryError::SiliconUnavailable)
    ));
    Ok(())
}

#[tokio::test]
async fn legacy_rows_keep_firing_unseen_until_linked_then_follow_the_account() -> anyhow::Result<()>
{
    let database = test_database().await?;
    let now = fixture_now();
    let legacy_key = Uuid::now_v7();
    sqlx::query("INSERT INTO organization_lifecycle (org_id, state) VALUES ('tos', 'active')")
        .execute(&database.pool)
        .await?;
    sqlx::query("INSERT INTO silicon_identities (org_id, principal_id, silicon_id, state) VALUES ('tos', $1, 'si:legacy', 'active')")
        .bind(legacy_key)
        .execute(&database.pool)
        .await?;
    let schedule = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO schedules (id, org_id, owner_principal_id, silicon_id, reminder_text, timezone, \
             schedule_kind, cron_expression, status, next_run_at) \
         VALUES ($1, 'tos', $2, 'si:legacy', 'legacy', 'UTC', 'recurring', '* * * * *', 'active', $3)",
    )
    .bind(schedule)
    .bind(legacy_key)
    .bind(now - Duration::minutes(1))
    .execute(&database.pool)
    .await?;
    assert!(
        due_ids(&database.repository, now)
            .await?
            .contains(&schedule),
        "unmapped legacy reminders keep firing"
    );
    seed_account(&database.pool, "Ada", ActorKind::Carbon, "c:ada", None).await?;
    let new_key = seed_account(
        &database.pool,
        "Legacy",
        ActorKind::Silicon,
        "si:legacy",
        Some(("Ada", "c:ada")),
    )
    .await?;
    let ada = actor(&database.identity, "Ada", ActorKind::Carbon, "c:ada").await?;
    assert!(
        database
            .repository
            .get_schedule(ada.read_keys().as_deref(), schedule)
            .await?
            .is_none(),
        "unseen until linked"
    );

    sqlx::query("INSERT INTO identity_links (iam_principal_id, iam_kind, iam_public_id, accounts_uuid, source) VALUES ($1, 'silicon', 'si:legacy', 'Legacy', 'test')")
        .bind(legacy_key)
        .execute(&database.pool)
        .await?;
    sqlx::query("INSERT INTO account_keys (storage_id, account_uuid, origin) VALUES ($1, 'Legacy', 'identity_link')")
        .bind(legacy_key)
        .execute(&database.pool)
        .await?;
    let ada = actor(&database.identity, "Ada", ActorKind::Carbon, "c:ada").await?;
    assert!(
        database
            .repository
            .get_schedule(ada.read_keys().as_deref(), schedule)
            .await?
            .is_some()
    );
    let owner = actor(
        &database.identity,
        "Legacy",
        ActorKind::Silicon,
        "si:legacy",
    )
    .await?;
    assert!(owner.owns_key(legacy_key) && owner.owns_key(new_key) && owner.storage_key == new_key);

    sqlx::query("UPDATE accounts SET status = 'access_removed' WHERE uuid = 'Legacy'")
        .execute(&database.pool)
        .await?;
    assert!(
        !due_ids(&database.repository, now)
            .await?
            .contains(&schedule),
        "linked rows follow the account"
    );
    Ok(())
}
