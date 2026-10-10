//! `remind-migrate link-identities`: re-keys rows written before the move to
//! Silicon Accounts to the accounts their IAM principals became.
//!
//! The mapping file has one `iam_principal_id,accounts_uuid` pair per line (an
//! optional header line is skipped). `iam_principal_id` is the Remind storage
//! key IAM-era rows carry (a UUID) or the IAM public id (`si:…`, `c:…`) that
//! `iam_identity_bindings` maps to it. An empty `accounts_uuid` (or `-`)
//! removes an earlier link.
//!
//! Nothing is rewritten: a link adds an `account_keys` row pointing the legacy
//! key at the account, and records the pairing in `identity_links`. Running it
//! again with a corrected file re-points the same rows, so the mapping can be
//! fixed until cutover. Everything happens in one transaction per database;
//! `--dry-run` rolls both back and only reports.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::accounts::AccountsGateway;
use crate::domain::{is_valid_account_uuid, is_valid_public_id};

/// One parsed line of the mapping file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkEntry {
    /// 1-based line number, for messages.
    pub line: usize,
    /// The legacy principal as written (UUID or public id).
    pub principal: String,
    /// The account to link it to; `None` removes the link.
    pub accounts_uuid: Option<String>,
}

/// Parses the mapping file.
///
/// # Errors
///
/// Returns every malformed line, so the operator fixes the file in one pass.
pub fn parse_mapping(text: &str) -> Result<Vec<LinkEntry>, Vec<String>> {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut fields = trimmed.split(',').map(str::trim);
        let (Some(principal), Some(accounts), None) = (fields.next(), fields.next(), fields.next())
        else {
            errors.push(format!(
                "line {line}: expected `iam_principal_id,accounts_uuid`"
            ));
            continue;
        };
        if line == 1 && principal == "iam_principal_id" {
            continue;
        }
        if Uuid::parse_str(principal).is_err() && !is_valid_public_id(principal) {
            errors.push(format!(
                "line {line}: `{principal}` is neither a Remind storage key (UUID) nor an IAM id (si:… or c:…)"
            ));
            continue;
        }
        let accounts_uuid = match accounts {
            "" | "-" => None,
            uuid if is_valid_account_uuid(uuid) => Some(uuid.to_owned()),
            other => {
                errors.push(format!(
                    "line {line}: `{other}` is not a Silicon Accounts uuid (1 to 64 letters and digits)"
                ));
                continue;
            }
        };
        entries.push(LinkEntry {
            line,
            principal: principal.to_owned(),
            accounts_uuid,
        });
    }
    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

/// What a run did (or, with `--dry-run`, would do).
#[derive(Debug, Default, Serialize)]
pub struct LinkReport {
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether the changes were committed (not for dry runs, nor when any
    /// entry was refused: the run is all or nothing).
    pub committed: bool,
    /// The `source` label recorded on new links.
    pub source: String,
    /// Links created or re-pointed.
    pub linked: Vec<LinkedPrincipal>,
    /// Links that already pointed where the file says.
    pub unchanged: usize,
    /// Links removed by empty `accounts_uuid` entries.
    pub unlinked: Vec<String>,
    /// File entries Remind could not apply, with the reason.
    pub refused: Vec<String>,
    /// Legacy principals that still own live data and have no link: their
    /// reminders keep firing but nobody sees them until they are linked.
    pub unmatched: Vec<UnmatchedPrincipal>,
    /// Legacy test environments now owned by an account.
    pub test_environments_linked: u64,
}

/// One applied link.
#[derive(Debug, Serialize)]
pub struct LinkedPrincipal {
    /// The legacy storage key.
    pub iam_principal_id: Uuid,
    /// The IAM public id, when known.
    pub iam_public_id: Option<String>,
    /// The account it now belongs to.
    pub accounts_uuid: String,
    /// The account's current id, when Silicon Accounts was consulted.
    pub accounts_id: Option<String>,
    /// Live schedules stored under the key.
    pub schedules: i64,
    /// Active subscriptions stored under the key.
    pub destinations: i64,
}

/// A legacy principal with live data and no link.
#[derive(Debug, Serialize)]
pub struct UnmatchedPrincipal {
    /// The legacy storage key.
    pub iam_principal_id: Uuid,
    /// The IAM public id, when known.
    pub iam_public_id: Option<String>,
    /// Live schedules stored under the key.
    pub schedules: i64,
    /// Active subscriptions stored under the key.
    pub destinations: i64,
}

/// Applies the mapping. `gateway`, when given, verifies each account with
/// Silicon Accounts and stores its current id and custodian; without it the
/// kind comes from the IAM binding (or from owning reminders, which only
/// Silicons do) and the rest is filled at the account's next sign-in.
///
/// # Errors
///
/// Returns database errors; nothing is committed then.
pub async fn link_identities(
    pool: &PgPool,
    testing: Option<&PgPool>,
    gateway: Option<&AccountsGateway>,
    entries: &[LinkEntry],
    source: &str,
    dry_run: bool,
) -> anyhow::Result<LinkReport> {
    let mut report = LinkReport {
        dry_run,
        source: source.to_owned(),
        ..LinkReport::default()
    };
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('remind.identity_links'))")
        .execute(&mut *transaction)
        .await?;
    report.refused = validate_mappings(&mut transaction, entries).await?;
    if !report.refused.is_empty() {
        transaction.rollback().await?;
        return Ok(report);
    }
    let mut applied: BTreeMap<Uuid, String> = BTreeMap::new();
    for entry in entries {
        match apply_entry(&mut transaction, gateway, entry, source).await? {
            Outcome::Linked(linked) => {
                applied.insert(linked.iam_principal_id, linked.accounts_uuid.clone());
                report.linked.push(linked);
            }
            Outcome::Unchanged(key, accounts_uuid) => {
                applied.insert(key, accounts_uuid);
                report.unchanged += 1;
            }
            Outcome::Unlinked(Some(key)) => report.unlinked.push(key.to_string()),
            Outcome::Unlinked(None) => {}
            Outcome::Refused(reason) => report.refused.push(reason),
        }
    }
    report.unmatched = unmatched(&mut transaction, &applied.keys().copied().collect()).await?;
    // All or nothing: a refused entry rolls everything back. Production
    // commits first; if the testing database then fails, running the same
    // file again completes it (every step is idempotent).
    let apply = !dry_run && report.refused.is_empty();
    if apply {
        transaction.commit().await?;
    } else {
        transaction.rollback().await?;
    }
    report.committed = apply;
    if let Some(testing) = testing {
        report.test_environments_linked = link_test_environments(testing, &applied, !apply).await?;
    }
    Ok(report)
}

/// Resolve aliases before writing: two spellings of one principal must never
/// let a later line silently replace the destination from an earlier line.
async fn validate_mappings(
    transaction: &mut Transaction<'_, Postgres>,
    entries: &[LinkEntry],
) -> anyhow::Result<Vec<String>> {
    let mut errors = Vec::new();
    let mut keys = BTreeMap::new();
    let mut destinations = BTreeMap::new();
    for entry in entries {
        let Some((key, _, _)) = resolve_principal(transaction, &entry.principal).await? else {
            errors.push(format!(
                "line {}: {} is not a principal Remind knows",
                entry.line, entry.principal
            ));
            continue;
        };
        if let Some(first) = keys.insert(key, entry.line) {
            errors.push(format!(
                "line {}: principal {key} already appears on line {first}; remove the duplicate",
                entry.line
            ));
        }
        if let Some(uuid) = &entry.accounts_uuid {
            if let Some((first, other)) = destinations.insert(uuid.clone(), (entry.line, key)) {
                errors.push(format!("line {}: {uuid} is also assigned to {other} on line {first}; different principals cannot be merged", entry.line));
            }
            let existing: Option<Uuid> = sqlx::query_scalar(
                "SELECT iam_principal_id FROM identity_links WHERE accounts_uuid = $1 AND iam_principal_id <> $2 LIMIT 1",
            ).bind(uuid).bind(key).fetch_optional(&mut **transaction).await?;
            if let Some(other) = existing {
                errors.push(format!("line {}: {uuid} is already linked to {other}; different principals cannot be merged", entry.line));
            }
        }
    }
    Ok(errors)
}

enum Outcome {
    Linked(LinkedPrincipal),
    Unchanged(Uuid, String),
    Unlinked(Option<Uuid>),
    Refused(String),
}

/// Applies one mapping line inside the production transaction.
async fn apply_entry(
    transaction: &mut Transaction<'_, Postgres>,
    gateway: Option<&AccountsGateway>,
    entry: &LinkEntry,
    source: &str,
) -> anyhow::Result<Outcome> {
    let Some((key, iam_kind, iam_public_id)) =
        resolve_principal(transaction, &entry.principal).await?
    else {
        return Ok(Outcome::Refused(format!(
            "line {}: `{}` is not a principal Remind knows (no binding and no rows under that key)",
            entry.line, entry.principal
        )));
    };
    let Some(accounts_uuid) = &entry.accounts_uuid else {
        let removed = sqlx::query(
            "DELETE FROM account_keys WHERE storage_id = $1 AND origin = 'identity_link'",
        )
        .bind(key)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
        sqlx::query("DELETE FROM identity_links WHERE iam_principal_id = $1")
            .bind(key)
            .execute(&mut **transaction)
            .await?;
        return Ok(Outcome::Unlinked((removed > 0).then_some(key)));
    };
    let primary: Option<String> = sqlx::query_scalar(
        "SELECT account_uuid FROM account_keys WHERE storage_id = $1 AND origin = 'accounts'",
    )
    .bind(key)
    .fetch_optional(&mut **transaction)
    .await?;
    if primary.is_some() {
        return Ok(Outcome::Refused(format!(
            "line {}: {key} is a key Remind created for a signed-in account, not an IAM principal",
            entry.line
        )));
    }
    let (kind, accounts_id, custodian) = match account_facts(
        transaction,
        gateway,
        key,
        iam_kind.as_deref(),
        accounts_uuid,
    )
    .await?
    {
        Ok(facts) => facts,
        Err(reason) => return Ok(Outcome::Refused(format!("line {}: {reason}", entry.line))),
    };
    if let Some(iam_kind) = iam_kind.as_deref().filter(|iam_kind| *iam_kind != kind) {
        return Ok(Outcome::Refused(format!(
            "line {}: {} was an IAM {iam_kind} but account {accounts_uuid} is a {kind}",
            entry.line,
            iam_public_id.as_deref().unwrap_or(&entry.principal),
        )));
    }
    upsert_account(
        transaction,
        accounts_uuid,
        &kind,
        accounts_id.as_deref(),
        custodian.as_ref(),
    )
    .await?;
    let previous = write_link(
        transaction,
        key,
        iam_kind.as_deref(),
        iam_public_id.as_deref(),
        accounts_uuid,
        source,
    )
    .await?;
    if previous.as_deref() == Some(accounts_uuid.as_str()) {
        return Ok(Outcome::Unchanged(key, accounts_uuid.clone()));
    }
    let (schedules, destinations) = live_counts(transaction, key).await?;
    Ok(Outcome::Linked(LinkedPrincipal {
        iam_principal_id: key,
        iam_public_id,
        accounts_uuid: accounts_uuid.clone(),
        accounts_id,
        schedules,
        destinations,
    }))
}

type AccountFacts = (String, Option<String>, Option<(String, String)>);

/// The account's kind, current id and custodian: from Silicon Accounts when a
/// gateway is given, else from the IAM binding or the rows the key owns.
/// `Err` is a refusal reason.
async fn account_facts(
    transaction: &mut Transaction<'_, Postgres>,
    gateway: Option<&AccountsGateway>,
    key: Uuid,
    iam_kind: Option<&str>,
    accounts_uuid: &str,
) -> anyhow::Result<Result<AccountFacts, String>> {
    let Some(gateway) = gateway else {
        return Ok(Ok((
            offline_kind(transaction, key, iam_kind).await?,
            None,
            None,
        )));
    };
    match gateway.lookup(accounts_uuid).await {
        Ok(Some(summary)) if summary.status != "deleted" => {
            let kind = match summary.kind {
                silicon_accounts_client::AccountKind::Carbon => "carbon",
                silicon_accounts_client::AccountKind::Silicon => "silicon",
            };
            let custodian = summary
                .custodian
                .map(|custodian| (custodian.uuid, custodian.id));
            Ok(Ok((kind.to_owned(), Some(summary.id), custodian)))
        }
        Ok(_) => Ok(Err(format!(
            "Silicon Accounts has no active account {accounts_uuid}"
        ))),
        Err(error) => anyhow::bail!("Silicon Accounts lookup of {accounts_uuid} failed: {error}"),
    }
}

/// Records the link and points the legacy key at the account. Returns the
/// account the key pointed at before.
async fn write_link(
    transaction: &mut Transaction<'_, Postgres>,
    key: Uuid,
    iam_kind: Option<&str>,
    iam_public_id: Option<&str>,
    accounts_uuid: &str,
    source: &str,
) -> anyhow::Result<Option<String>> {
    let previous: Option<String> = sqlx::query_scalar(
        "SELECT account_uuid FROM account_keys WHERE storage_id = $1 AND origin = 'identity_link'",
    )
    .bind(key)
    .fetch_optional(&mut **transaction)
    .await?;
    sqlx::query(
        "INSERT INTO identity_links (iam_principal_id, iam_kind, iam_public_id, accounts_uuid, source) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (iam_principal_id) DO UPDATE SET accounts_uuid = EXCLUDED.accounts_uuid, \
             iam_kind = EXCLUDED.iam_kind, iam_public_id = EXCLUDED.iam_public_id, \
             source = EXCLUDED.source, linked_at = clock_timestamp() \
         WHERE identity_links.accounts_uuid <> EXCLUDED.accounts_uuid",
    )
    .bind(key)
    .bind(iam_kind)
    .bind(iam_public_id)
    .bind(accounts_uuid)
    .bind(source)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        "INSERT INTO account_keys (storage_id, account_uuid, origin) VALUES ($1, $2, 'identity_link') \
         ON CONFLICT (storage_id) DO UPDATE SET account_uuid = EXCLUDED.account_uuid \
         WHERE account_keys.origin = 'identity_link'",
    )
    .bind(key)
    .bind(accounts_uuid)
    .execute(&mut **transaction)
    .await?;
    Ok(previous)
}

/// Resolves a file entry to the legacy storage key, with the IAM kind and public id when bound.
async fn resolve_principal(
    transaction: &mut Transaction<'_, Postgres>,
    principal: &str,
) -> anyhow::Result<Option<(Uuid, Option<String>, Option<String>)>> {
    if let Ok(key) = Uuid::parse_str(principal) {
        let binding: Option<(String, String)> = sqlx::query_as(
            "SELECT identity_kind, public_id FROM iam_identity_bindings \
             WHERE local_id = $1 AND identity_kind IN ('carbon', 'silicon')",
        )
        .bind(key)
        .fetch_optional(&mut **transaction)
        .await?;
        let referenced: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM schedules WHERE owner_principal_id = $1) \
                 OR EXISTS (SELECT 1 FROM hook_destinations WHERE owner_principal_id = $1) \
                 OR EXISTS (SELECT 1 FROM deleted_reminders WHERE owner_principal_id = $1) \
                 OR EXISTS (SELECT 1 FROM silicon_identities WHERE principal_id = $1)",
        )
        .bind(key)
        .fetch_one(&mut **transaction)
        .await?;
        return Ok((binding.is_some() || referenced).then(|| {
            let (kind, public_id) = binding.unzip();
            (key, kind, public_id)
        }));
    }
    let kind = if principal.starts_with("si:") {
        "silicon"
    } else {
        "carbon"
    };
    let key: Option<Uuid> = sqlx::query_scalar(
        "SELECT local_id FROM iam_identity_bindings WHERE identity_kind = $1 AND public_id = $2",
    )
    .bind(kind)
    .bind(principal)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(key.map(|key| (key, Some(kind.to_owned()), Some(principal.to_owned()))))
}

async fn offline_kind(
    transaction: &mut Transaction<'_, Postgres>,
    key: Uuid,
    iam_kind: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(kind) = iam_kind {
        return Ok(kind.to_owned());
    }
    let owns: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM schedules WHERE owner_principal_id = $1) \
             OR EXISTS (SELECT 1 FROM hook_destinations WHERE owner_principal_id = $1)",
    )
    .bind(key)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(if owns { "silicon" } else { "carbon" }.to_owned())
}

async fn upsert_account(
    transaction: &mut Transaction<'_, Postgres>,
    uuid: &str,
    kind: &str,
    public_id: Option<&str>,
    custodian: Option<&(String, String)>,
) -> anyhow::Result<()> {
    let public_id = public_id.filter(|id| is_valid_public_id(id)).unwrap_or("");
    let custodian = custodian
        .filter(|(custodian_uuid, _)| kind == "silicon" && is_valid_account_uuid(custodian_uuid));
    sqlx::query(
        "INSERT INTO accounts (uuid, kind, public_id, custodian_uuid, custodian_id, looked_up_at) \
         VALUES ($1, $2, $3, $4, $5, CASE WHEN $3 = '' THEN NULL ELSE clock_timestamp() END) \
         ON CONFLICT (uuid) DO UPDATE SET \
             kind = COALESCE(accounts.kind, EXCLUDED.kind), \
             public_id = CASE WHEN EXCLUDED.public_id = '' THEN accounts.public_id \
                              ELSE EXCLUDED.public_id END, \
             custodian_uuid = COALESCE(EXCLUDED.custodian_uuid, accounts.custodian_uuid), \
             custodian_id = COALESCE(EXCLUDED.custodian_id, accounts.custodian_id), \
             updated_at = clock_timestamp()",
    )
    .bind(uuid)
    .bind(kind)
    .bind(public_id)
    .bind(custodian.map(|(custodian_uuid, _)| custodian_uuid.as_str()))
    .bind(custodian.map(|(_, custodian_id)| custodian_id.as_str()))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn live_counts(
    transaction: &mut Transaction<'_, Postgres>,
    key: Uuid,
) -> anyhow::Result<(i64, i64)> {
    Ok(sqlx::query_as(
        "SELECT (SELECT count(*) FROM schedules WHERE owner_principal_id = $1 \
                   AND (purge_after IS NULL OR purge_after > clock_timestamp())), \
                (SELECT count(*) FROM hook_destinations WHERE owner_principal_id = $1 \
                   AND disabled_at IS NULL)",
    )
    .bind(key)
    .fetch_one(&mut **transaction)
    .await?)
}

/// Legacy keys that own live data and no account.
async fn unmatched(
    transaction: &mut Transaction<'_, Postgres>,
    linked: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<UnmatchedPrincipal>> {
    let rows: Vec<(Uuid, Option<String>, i64, i64)> = sqlx::query_as(
        "WITH legacy AS (\
             SELECT owner_principal_id AS key FROM schedules \
             WHERE org_id IS NOT NULL AND (purge_after IS NULL OR purge_after > clock_timestamp()) \
             UNION SELECT owner_principal_id FROM hook_destinations \
             WHERE org_id IS NOT NULL AND disabled_at IS NULL) \
         SELECT l.key, \
                (SELECT b.public_id FROM iam_identity_bindings b \
                 WHERE b.local_id = l.key AND b.identity_kind IN ('carbon', 'silicon') LIMIT 1), \
                (SELECT count(*) FROM schedules s WHERE s.owner_principal_id = l.key \
                   AND (s.purge_after IS NULL OR s.purge_after > clock_timestamp())), \
                (SELECT count(*) FROM hook_destinations d WHERE d.owner_principal_id = l.key \
                   AND d.disabled_at IS NULL) \
         FROM legacy l \
         WHERE NOT EXISTS (SELECT 1 FROM account_keys k WHERE k.storage_id = l.key) \
         ORDER BY l.key",
    )
    .fetch_all(&mut **transaction)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(key, ..)| !linked.contains(key))
        .map(
            |(iam_principal_id, iam_public_id, schedules, destinations)| UnmatchedPrincipal {
                iam_principal_id,
                iam_public_id,
                schedules,
                destinations,
            },
        )
        .collect())
}

/// Sets the owner of legacy key-based test environments whose creator was linked.
async fn link_test_environments(
    testing: &PgPool,
    applied: &BTreeMap<Uuid, String>,
    roll_back: bool,
) -> anyhow::Result<u64> {
    let mut transaction = testing.begin().await?;
    let mut changed = 0;
    for (key, accounts_uuid) in applied {
        changed += sqlx::query(
            "UPDATE public.testing_environments SET owner_uuid = $2 \
             WHERE creator_id = $1 AND iam_control_version IS NULL \
               AND owner_uuid IS DISTINCT FROM $2",
        )
        .bind(key.to_string())
        .bind(accounts_uuid)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
    }
    if roll_back {
        transaction.rollback().await?;
    } else {
        transaction.commit().await?;
    }
    Ok(changed)
}
