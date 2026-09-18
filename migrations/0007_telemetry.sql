-- Sandbox telemetry stays in its schema. Production records go to Space Station.
CREATE TABLE telemetry_events (
 id uuid PRIMARY KEY,
 recorded_at timestamptz NOT NULL DEFAULT now(),
 event jsonb NOT NULL
);
CREATE INDEX telemetry_events_time ON telemetry_events(recorded_at);
