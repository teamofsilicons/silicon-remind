-- A conservative local hint only: the normal IAM/lifecycle admission and the
-- repository's current lease/dispatch predicates remain authoritative.
SELECT
    EXISTS (
        SELECT 1 FROM schedules s
        LEFT JOIN organization_lifecycle o ON o.org_id = s.org_id
        LEFT JOIN silicon_identities i
            ON i.org_id = s.org_id AND i.principal_id = s.owner_principal_id
        WHERE (s.status = 'active' AND s.deleted_at IS NULL AND s.next_run_at <= $1)
           OR s.purge_after <= $1
           OR (s.deleted_at IS NULL AND s.status <> 'completed'
               AND (o.state IS DISTINCT FROM 'active' OR i.state IS DISTINCT FROM 'active'))
    )
    OR EXISTS (
        SELECT 1 FROM executions e
        JOIN schedules s ON s.id = e.schedule_id
        LEFT JOIN organization_lifecycle o ON o.org_id = s.org_id
        LEFT JOIN silicon_identities i
            ON i.org_id = s.org_id AND i.principal_id = s.owner_principal_id
        WHERE e.status IN ('pending', 'retrying')
          AND ((e.next_attempt_at <= $1
                AND (e.lease_expires_at IS NULL OR e.lease_expires_at <= $1)
                AND s.deleted_at IS NULL AND (s.purge_after IS NULL OR s.purge_after > $1))
               OR o.state IS DISTINCT FROM 'active' OR i.state IS DISTINCT FROM 'active')
    )
    OR EXISTS (
        SELECT 1 FROM hook_destinations d
        LEFT JOIN organization_lifecycle o ON o.org_id = d.org_id
        LEFT JOIN silicon_identities i
            ON i.org_id = d.org_id AND i.principal_id = d.owner_principal_id
        WHERE d.purge_after <= $1
           OR (d.disabled_at IS NULL AND
               (d.encryption_key_version <> $2
                OR o.state IS DISTINCT FROM 'active' OR i.state IS DISTINCT FROM 'active'))
    )
    OR EXISTS (SELECT 1 FROM idempotency_records WHERE expires_at <= $1)
    OR EXISTS (SELECT 1 FROM deleted_reminders OFFSET 100000 LIMIT 1)
