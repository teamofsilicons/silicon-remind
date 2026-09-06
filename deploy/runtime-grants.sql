-- Execute as the production migration owner. Runtime may operate data but may
-- not change schemas, migration history, or the append-only audit guards.
GRANT USAGE ON SCHEMA public TO remind_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO remind_runtime;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO remind_runtime;
REVOKE ALL ON public._sqlx_migrations FROM remind_runtime;
GRANT SELECT ON public._sqlx_migrations TO remind_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO remind_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO remind_runtime;
