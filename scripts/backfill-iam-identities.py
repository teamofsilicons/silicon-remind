#!/usr/bin/env python3
"""Bind trusted IAM IDs to retained Remind keys; no remote discovery or secret output.

Run against the application database with DATABASE_URL and an owner connection.
The default is a rollback-only completeness check. Use --apply before IAM cutover.
Every test world must be imported separately; production is never a fallback.
"""
import argparse
import hashlib
import json
import os
import subprocess
import uuid
from pathlib import Path
from urllib.parse import urlsplit, unquote, parse_qsl


def mapping_rows(document, environment):
    rows = set()
    for actor in document["testing" if environment else "production"]:
        if actor.get("testing_environment_id") != environment:
            continue
        if actor["kind"] not in ("carbon", "silicon"):
            continue
        rows.add((actor["kind"], actor["public_id"], str(uuid.UUID(actor["legacy_id"]))))
        for member in actor["memberships"]:
            expected = f'{actor["public_id"]}[{member["org_id"]}]'
            if member["membership_public_id"] != expected:
                raise ValueError("inconsistent trusted membership mapping")
            rows.add(("membership", expected, str(uuid.UUID(member["membership_id"]))))
    if not rows:
        raise ValueError("no identities for the explicitly selected environment")
    return sorted(rows)


def schema_upgrade():
    migrations = sorted((Path(__file__).resolve().parent.parent / "migrations").glob("*.sql"))
    expected = []
    for path in migrations:
        version = int(path.name.split("_", 1)[0])
        if version > 8:
            raise ValueError("this cutover utility supports migrations through version 8 only")
        digest = hashlib.sha384(path.read_bytes()).hexdigest()
        expected.append((version, digest))
    checks = ",".join(f"({version},decode('{digest}','hex'))" for version, digest in expected)
    migration = next(path for path in migrations if path.name.startswith("0008_"))
    digest = hashlib.sha384(migration.read_bytes()).hexdigest()
    return rf"""LOCK TABLE _sqlx_migrations IN EXCLUSIVE MODE;
DO $guard$ BEGIN
 IF EXISTS(SELECT 1 FROM (VALUES {checks}) AS expected(version,checksum)
 LEFT JOIN _sqlx_migrations actual USING(version)
 WHERE (expected.version < 8 AND actual.version IS NULL)
 OR (actual.version IS NOT NULL AND (NOT actual.success OR actual.checksum<>expected.checksum)))
 OR EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version>8) THEN
 RAISE EXCEPTION 'retained schema migration history differs from release'; END IF;
END $guard$;
SELECT NOT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=8) AS needs_upgrade \gset
\if :needs_upgrade
{migration.read_text()}
INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time)
VALUES(8,'canonical iam identity bindings',true,decode('{digest}','hex'),0);
\endif
"""


def statement(rows, schema, apply, migrate=False, environment=None, local_environment=None):
    public_keys = {(kind, public) for kind, public, _ in rows}
    local_keys = {(kind, local) for kind, _, local in rows}
    if len(public_keys) != len(rows) or len(local_keys) != len(rows):
        raise ValueError("trusted export contains conflicting identity bindings")
    payload = json.dumps([dict(identity_kind=kind, public_id=public, local_id=local)
        for kind, public, local in rows], separators=(",", ":")).replace("'", "''")
    relation = f"SELECT * FROM jsonb_to_recordset('{payload}'::jsonb) AS imported(identity_kind text,public_id text,local_id uuid)"
    binding = ""
    if environment:
        # Control metadata lives in the same shared test database. Validate the
        # registered local/upstream pair under a lock before selecting its schema.
        local = str(uuid.UUID(local_environment or environment))
        upstream = str(uuid.UUID(environment))
        if schema != f"remind_test_{uuid.UUID(local).hex}":
            raise ValueError("schema does not match selected local environment")
        binding = f"""LOCK TABLE public.testing_environments IN SHARE MODE;
DO $binding$ BEGIN
 IF NOT EXISTS(SELECT 1 FROM public.testing_environments
 WHERE id='{local}' AND iam_environment_id='{upstream}') THEN
 RAISE EXCEPTION 'local testing environment is not bound to selected IAM environment'; END IF;
END $binding$;
"""
    elif schema != "public":
        raise ValueError("testing schema requires an explicit IAM environment")
    return f'''BEGIN;
SET LOCAL standard_conforming_strings = on;
{binding}
SET LOCAL search_path TO "{schema}";
DO $$ BEGIN IF current_schema() IS DISTINCT FROM '{schema}' THEN
 RAISE EXCEPTION 'selected schema does not exist'; END IF; END $$;
{schema_upgrade() if migrate else ''}
LOCK TABLE iam_identity_bindings IN EXCLUSIVE MODE;
DO $$ BEGIN
 IF EXISTS(WITH identity_import AS ({relation})
 SELECT 1 FROM identity_import i JOIN iam_identity_bindings b
 ON b.identity_kind=i.identity_kind AND (b.public_id=i.public_id OR b.local_id=i.local_id)
 WHERE b.public_id<>i.public_id OR b.local_id<>i.local_id) THEN
 RAISE EXCEPTION 'existing identity binding conflicts with trusted export'; END IF;
END $$;
WITH identity_import AS ({relation})
INSERT INTO iam_identity_bindings SELECT * FROM identity_import
 ON CONFLICT(identity_kind,public_id) DO NOTHING;
DO $$ BEGIN IF EXISTS(SELECT 1 FROM iam_unmapped_identity_keys()) THEN
 RAISE EXCEPTION 'retained identity references are absent from trusted export'; END IF; END $$;
SELECT {len(rows)} AS verified_bindings;
{'COMMIT' if apply else 'ROLLBACK'};
'''


def database_environment():
    env = dict(os.environ)
    parsed = urlsplit(env["DATABASE_URL"])
    if parsed.scheme not in ("postgres", "postgresql") or not parsed.path.lstrip("/"):
        raise ValueError("DATABASE_URL must be a PostgreSQL connection URL")
    fields = {"PGHOST": parsed.hostname, "PGPORT": str(parsed.port or 5432),
        "PGUSER": unquote(parsed.username or ""), "PGPASSWORD": unquote(parsed.password or ""),
        "PGDATABASE": unquote(parsed.path.lstrip("/"))}
    parameters = {"sslmode": "PGSSLMODE", "sslrootcert": "PGSSLROOTCERT",
        "sslcert": "PGSSLCERT", "sslkey": "PGSSLKEY", "connect_timeout": "PGCONNECT_TIMEOUT",
        "application_name": "PGAPPNAME", "options": "PGOPTIONS", "host": "PGHOST"}
    for name, value in parse_qsl(parsed.query, keep_blank_values=True):
        if name not in parameters:
            raise ValueError("unsupported DATABASE_URL parameter")
        fields[parameters[name]] = value
    for name, value in fields.items():
        if value:
            env[name] = value
        else:
            env.pop(name, None)
    env.pop("PGSERVICE", None)
    env.pop("PGSERVICEFILE", None)
    return env


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mapping", type=Path)
    plane = parser.add_mutually_exclusive_group(required=True)
    plane.add_argument("--production", action="store_true")
    plane.add_argument("--testing-environment-id", type=uuid.UUID)
    parser.add_argument("--local-environment-id", type=uuid.UUID,
        help="retained Remind world ID when it differs from the selected IAM environment; the registered pair is verified")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--migrate-retained-schema", action="store_true",
        help="apply only migration 0008 in an existing testing schema, verifying all earlier checksums")
    args = parser.parse_args()
    environment = str(args.testing_environment_id) if args.testing_environment_id else None
    if args.local_environment_id and not environment:
        parser.error("--local-environment-id requires --testing-environment-id")
    local = args.local_environment_id or args.testing_environment_id
    schema = f"remind_test_{local.hex}" if environment else "public"
    if args.migrate_retained_schema and not environment:
        parser.error("--migrate-retained-schema requires --testing-environment-id")
    rows = mapping_rows(json.loads(args.mapping.read_text()), environment)
    # Database connection is read from the process environment, never printed.
    env = database_environment()
    result = subprocess.run(["psql", "-X", "--set", "ON_ERROR_STOP=1", "--quiet"],
        input=statement(rows, schema, args.apply, args.migrate_retained_schema,
            environment, str(local) if local else None), text=True, env=env, capture_output=True)
    if result.returncode:
        # psql COPY errors may contain imported identifiers; do not echo them.
        raise SystemExit("Identity binding validation failed; transaction rolled back. Check schema, export completeness, and existing binding conflicts.")
    print(f"{'Applied' if args.apply else 'Validated (rolled back)'} {len(rows)} bindings in {schema}.")


if __name__ == "__main__":
    main()
