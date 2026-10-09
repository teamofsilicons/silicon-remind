//! Who reads which reminders: the custodian circle, viewer grants, outsiders,
//! suspended owners, and test environments (everything readable).

use chrono::Duration;
use uuid::Uuid;

use super::{actor, fixture_now, seed_account, seed_execution, seed_schedule, test_database};
use crate::{
    domain::{ActorKind, Relation, ScheduleSection},
    infrastructure::postgres::{ListSchedules, RepositoryError, ScheduleCursor},
};

fn filters(
    read_keys: Option<Vec<Uuid>>,
    cursor: Option<ScheduleCursor>,
    limit: u32,
) -> ListSchedules {
    ListSchedules {
        read_keys,
        owner_keys: None,
        silicon_snapshot: None,
        section: ScheduleSection::Current,
        status: None,
        cursor,
        limit,
    }
}

struct Circle {
    one: Uuid,
    two: Uuid,
    bob_silicon: Uuid,
    one_execution: Uuid,
    bob_execution: Uuid,
}

/// c:ada looks after si:one and si:two; c:bob looks after si:bob-bot; c:zed has nothing.
async fn seed_circle(pool: &sqlx::PgPool) -> anyhow::Result<Circle> {
    let now = fixture_now();
    seed_account(pool, "Ada", ActorKind::Carbon, "c:ada", None).await?;
    seed_account(pool, "Bob", ActorKind::Carbon, "c:bob", None).await?;
    seed_account(pool, "Zed", ActorKind::Carbon, "c:zed", None).await?;
    let one_key = seed_account(
        pool,
        "One",
        ActorKind::Silicon,
        "si:one",
        Some(("Ada", "c:ada")),
    )
    .await?;
    let two_key = seed_account(
        pool,
        "Two",
        ActorKind::Silicon,
        "si:two",
        Some(("Ada", "c:ada")),
    )
    .await?;
    let bob_key = seed_account(
        pool,
        "BobBot",
        ActorKind::Silicon,
        "si:bob-bot",
        Some(("Bob", "c:bob")),
    )
    .await?;
    let one = seed_schedule(
        pool,
        one_key,
        "si:one",
        "one",
        "recurring",
        "active",
        Some(now + Duration::hours(1)),
        now - Duration::minutes(2),
    )
    .await?;
    let two = seed_schedule(
        pool,
        two_key,
        "si:two",
        "two",
        "recurring",
        "active",
        Some(now + Duration::hours(1)),
        now - Duration::minutes(3),
    )
    .await?;
    let bob_silicon = seed_schedule(
        pool,
        bob_key,
        "si:bob-bot",
        "bob",
        "recurring",
        "active",
        Some(now + Duration::hours(1)),
        now - Duration::minutes(1),
    )
    .await?;
    Ok(Circle {
        one,
        two,
        bob_silicon,
        one_execution: seed_execution(pool, one, "si:one", now).await?,
        bob_execution: seed_execution(pool, bob_silicon, "si:bob-bot", now).await?,
    })
}

#[tokio::test]
async fn the_circle_reads_its_silicons_reminders_and_outsiders_read_nothing() -> anyhow::Result<()>
{
    let database = test_database().await?;
    let circle = seed_circle(&database.pool).await?;
    let repository = &database.repository;

    // The custodian reads both of its Silicons, newest first, paged before the keyset.
    let ada = actor(&database.identity, "Ada", ActorKind::Carbon, "c:ada").await?;
    assert!(ada.looks_after("One") && ada.looks_after("Two"));
    let first = repository
        .list_schedules(&filters(ada.read_keys(), None, 1))
        .await?;
    assert_eq!(
        first.items.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![circle.one]
    );
    assert!(first.has_more);
    let cursor = ScheduleCursor {
        created_at: first.items[0].created_at,
        id: first.items[0].id,
    };
    let second = repository
        .list_schedules(&filters(ada.read_keys(), Some(cursor), 1))
        .await?;
    assert_eq!(
        second.items.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![circle.two]
    );
    assert!(!second.has_more);
    let keys = ada.read_keys();
    assert!(
        repository
            .get_schedule(keys.as_deref(), circle.bob_silicon)
            .await?
            .is_none()
    );
    assert!(
        repository
            .get_execution(keys.as_deref(), circle.one_execution)
            .await?
            .is_some()
    );
    assert!(
        repository
            .get_execution(keys.as_deref(), circle.bob_execution)
            .await?
            .is_none()
    );
    assert!(matches!(
        repository
            .list_executions(keys.as_deref(), circle.bob_silicon, None, 20)
            .await,
        Err(RepositoryError::NotFound)
    ));

    // A Silicon reads itself and its custodian's other Silicons, not other circles.
    let one = actor(&database.identity, "One", ActorKind::Silicon, "si:one").await?;
    let relations = one
        .visible
        .iter()
        .map(|owner| (owner.account.uuid.as_str(), owner.relation))
        .collect::<Vec<_>>();
    assert!(relations.contains(&("One", Relation::Own)));
    assert!(relations.contains(&("Two", Relation::Sibling)));
    let page = repository
        .list_schedules(&filters(one.read_keys(), None, 20))
        .await?;
    let mut ids = page.items.iter().map(|s| s.id).collect::<Vec<_>>();
    ids.sort_unstable();
    let mut expected = vec![circle.one, circle.two];
    expected.sort_unstable();
    assert_eq!(ids, expected);

    // An outsider reads nothing; a test-environment key holder reads everything.
    let zed = actor(&database.identity, "Zed", ActorKind::Carbon, "c:zed").await?;
    assert!(
        repository
            .list_schedules(&filters(zed.read_keys(), None, 20))
            .await?
            .items
            .is_empty()
    );
    assert_eq!(
        repository
            .list_schedules(&filters(None, None, 20))
            .await?
            .items
            .len(),
        3
    );
    Ok(())
}

#[tokio::test]
async fn grants_extend_reading_and_suspended_owners_disappear() -> anyhow::Result<()> {
    let database = test_database().await?;
    let circle = seed_circle(&database.pool).await?;
    crate::infrastructure::sharing::grant(&database.pool, "One", "Bob", "One").await?;

    let bob = actor(&database.identity, "Bob", ActorKind::Carbon, "c:bob").await?;
    assert!(
        bob.visible
            .iter()
            .any(|owner| owner.account.uuid == "One" && owner.relation == Relation::Shared)
    );
    let mut ids = database
        .repository
        .list_schedules(&filters(bob.read_keys(), None, 20))
        .await?
        .items
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    let mut expected = vec![circle.one, circle.bob_silicon];
    expected.sort_unstable();
    assert_eq!(ids, expected);

    // A suspended Silicon's reminders are hidden from its custodian until it signs in again.
    sqlx::query("UPDATE accounts SET status = 'access_removed' WHERE uuid = 'One'")
        .execute(&database.pool)
        .await?;
    let ada = actor(&database.identity, "Ada", ActorKind::Carbon, "c:ada").await?;
    let visible = database
        .repository
        .list_schedules(&filters(ada.read_keys(), None, 20))
        .await?;
    assert_eq!(
        visible.items.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![circle.two]
    );

    crate::infrastructure::sharing::revoke_grant(&database.pool, "One", "Bob", "One").await?;
    let bob = actor(&database.identity, "Bob", ActorKind::Carbon, "c:bob").await?;
    assert!(!bob.visible.iter().any(|owner| owner.account.uuid == "One"));
    Ok(())
}

#[tokio::test]
async fn a_custodian_change_moves_visibility_to_the_new_custodian() -> anyhow::Result<()> {
    let database = test_database().await?;
    let circle = seed_circle(&database.pool).await?;
    sqlx::query(
        "UPDATE accounts SET custodian_uuid = 'Bob', custodian_id = 'c:bob' WHERE uuid = 'Two'",
    )
    .execute(&database.pool)
    .await?;
    let ada = actor(&database.identity, "Ada", ActorKind::Carbon, "c:ada").await?;
    let bob = actor(&database.identity, "Bob", ActorKind::Carbon, "c:bob").await?;
    let ada_ids = database
        .repository
        .list_schedules(&filters(ada.read_keys(), None, 20))
        .await?;
    let bob_ids = database
        .repository
        .list_schedules(&filters(bob.read_keys(), None, 20))
        .await?;
    assert!(ada_ids.items.iter().all(|s| s.id != circle.two));
    assert!(bob_ids.items.iter().any(|s| s.id == circle.two));
    assert!(bob.looks_after("Two") && !ada.looks_after("Two"));
    Ok(())
}
