-- A conservative local hint only: admission and the repository's lease and
-- dispatch predicates remain authoritative. The IAM-era clauses apply only to
-- rows that still carry an organization from before Silicon Accounts.
SELECT
    EXISTS (
        SELECT 1 FROM schedules s
        WHERE (s.status = 'active' AND s.deleted_at IS NULL AND s.next_run_at <= $1)
           OR s.purge_after <= $1
           OR (s.org_id IS NOT NULL AND s.deleted_at IS NULL AND s.status <> 'completed'
               AND NOT EXISTS (SELECT 1 FROM silicon_identities i
                   JOIN organization_lifecycle o ON o.org_id = i.org_id
                   WHERE i.org_id = s.org_id AND i.principal_id = s.owner_principal_id
                     AND i.state = 'active' AND o.state = 'active'))
    )
    OR EXISTS (
        SELECT 1 FROM executions e
        JOIN schedules s ON s.id = e.schedule_id
        WHERE e.status IN ('pending', 'retrying')
          AND e.next_attempt_at <= $1
          AND (e.lease_expires_at IS NULL OR e.lease_expires_at <= $1)
    )
    OR EXISTS (
        SELECT 1 FROM hook_destinations d
        WHERE d.purge_after <= $1
           OR (d.disabled_at IS NULL AND d.encryption_key_version <> $2)
    )
    OR EXISTS (SELECT 1 FROM idempotency_records WHERE expires_at <= $1)
    OR EXISTS (SELECT 1 FROM deleted_reminders OFFSET 100000 LIMIT 1)
