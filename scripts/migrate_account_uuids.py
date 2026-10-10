#!/usr/bin/env python3
"""Backfill Remind production or its separate testing database using Accounts' CSV.

Use once for each database with APIs, workers and maintenance stopped. All registered
active and retained test schemas migrate together; secrets, private storage keys,
immutable schedule snapshots and execution payloads retain their original bytes.
Dependencies: accounts_uuid128.requirements.txt. No writes persist without --apply.
"""
import argparse
import json
import os
import sys
from accounts_uuid128 import read_mapping, migrate


def manifest_for(connection, testing=False):
    with connection.cursor() as cur:
        if testing:
            cur.execute("SELECT id FROM public.testing_environments ORDER BY id")
            schemas=["remind_test_"+str(row[0]).replace("-","") for row in cur.fetchall()]
            cur.execute("SELECT nspname FROM pg_namespace WHERE nspname LIKE 'remind_test_%'")
            if set(schemas) != {row[0] for row in cur.fetchall()}:
                raise ValueError("registered and stored test schemas differ; reconcile them before UUID migration")
        else:
            schemas=["public"]
        for schema in schemas:
            cur.execute("SELECT to_regclass(%s)",(schema+".accounts_uuid128_map",))
            if cur.fetchone()[0] is None:
                raise ValueError("run remind-migrate (including every testing schema) before UUID backfill")
    scalar=["accounts.uuid","accounts.custodian_uuid","account_keys.account_uuid","identity_links.accounts_uuid",
            "reminder_viewers.owner_uuid","reminder_viewers.viewer_uuid","reminder_viewers.granted_by_uuid","reminder_viewers.revoked_by_uuid",
            "silicon_allowances.silicon_uuid","silicon_allowances.allowed_uuid","silicon_allowances.created_by_uuid","silicon_allowances.revoked_by_uuid"]
    structured=["idempotency_records.response_body","audit_records.metadata"]
    conditional=[("audit_records.actor_id","t.actor_type IN ('carbon','silicon')"),
                 ("idempotency_records.actor_id","t.actor_type IN ('carbon','silicon')"),
                 ("bug_reports.actor_id","t.org_id IS NULL"),
                 ("internal_event_receipts.subject_id","t.source='silicon-accounts'")]
    manifest={"app":"remind","schema":"public","schemas":sorted(set(schemas+["public"])),
              "allow_unknown_kind":True,"account_tables":[f"{schema}.accounts" for schema in schemas],
              "ledger_tables":[f"{schema}.accounts_uuid128_map" for schema in schemas],
              "columns":[f"{schema}.{column}" for schema in schemas for column in scalar],
              "json_columns":[f"{schema}.{column}" for schema in schemas for column in structured],
              "conditional_columns":[{"column":f"{schema}.{column}","where":where} for schema in schemas for column,where in conditional]}
    if testing:
        manifest["columns"] += ["public.testing_environments.owner_uuid","public.testing_environments.creator_id"]
    return manifest


def main():
    import psycopg
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--file",required=True)
    parser.add_argument("--database-env",default="REMIND_DATABASE_URL")
    parser.add_argument("--testing",action="store_true",help="Target the separate testing database and all its environments")
    parser.add_argument("--apply",action="store_true")
    parser.add_argument("--writers-stopped",action="store_true")
    args=parser.parse_args()
    if args.apply and not args.writers_stopped:
        parser.error("--apply requires --writers-stopped")
    if args.database_env not in os.environ:
        parser.error(f"{args.database_env} is not set")
    mapping,kinds,digest=read_mapping(args.file)
    url=os.environ[args.database_env]
    with psycopg.connect(url) as conn:
        manifest=manifest_for(conn,args.testing)
    report=migrate(url,manifest,mapping,kinds,digest,args.apply)
    print(json.dumps(report,sort_keys=True,indent=2))

if __name__=="__main__":
    try:
        main()
    except Exception as error:
        message=str(error) if isinstance(error,ValueError) else f"{type(error).__name__}: migration rolled back; inspect local schema and configuration"
        print(message,file=sys.stderr)
        sys.exit(1)
