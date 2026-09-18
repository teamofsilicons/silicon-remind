-- Reports share the selected production or sandbox schema; only production is mailed.
CREATE TABLE bug_reports (
 id uuid PRIMARY KEY,
 org_id text NOT NULL,
 actor_id text NOT NULL,
 idempotency_key text NOT NULL,
 request_hash text NOT NULL,
 message text NOT NULL,
 pr text,
 status text NOT NULL CHECK (status IN ('queued','sending','sent','simulated','failed')),
 attempts integer NOT NULL DEFAULT 0,
 next_attempt_at timestamptz NOT NULL DEFAULT now(),
 created_at timestamptz NOT NULL DEFAULT now(),
 failure_reason text,
 UNIQUE (org_id, actor_id, idempotency_key)
);
CREATE INDEX bug_reports_pending ON bug_reports(next_attempt_at) WHERE status IN ('queued','sending');
