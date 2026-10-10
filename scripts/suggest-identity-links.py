#!/usr/bin/env python3
"""Suggest a mapping file for `remind-migrate link-identities` from a dry run's report.

    remind-migrate link-identities --file /dev/null --dry-run > report.json
    REMIND_APP_SECRET=... python3 scripts/suggest-identity-links.py report.json > mapping.csv

For every principal in the report's `unmatched` list (an account of the previous identity service that still owns
reminders or subscriptions and has no link yet), it looks the principal's `si:`/`c:` id up at Silicon Accounts
(`GET /v1/accounts/by-id/{id}` with Remind's app credentials) and writes `id,accounts_uuid` when exactly that id
belongs to an active account of the same kind. Everything else becomes a `# REVIEW` comment saying what to decide:
an id nobody holds now, a deleted account, another kind, a principal without an id. Comments are ignored by
link-identities, so the file can be dry-run as it is; a Carbon reviews it before the real run.

Settings: REMIND_APP_SECRET (required), REMIND_APP_ID (default remind), ACCOUNTS_URL (default
https://accounts.teamofsilicons.com) and ACCOUNTS_API_URL (where to call the API, default ACCOUNTS_URL). It only
reads from Silicon Accounts and prints nothing secret. Silicon Accounts allows 600 lookups a minute per app; the
script waits when asked to slow down.
"""

import argparse
import base64
from datetime import datetime, timezone
import json
import os
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

KINDS = {"si:": "silicon", "c:": "carbon"}


class Lookup:
    def __init__(self, base, app_id, secret, timeout=10.0):
        self.base = base.rstrip("/")
        token = base64.b64encode(f"{app_id}:{secret}".encode()).decode()
        self.headers = {"Authorization": "Basic " + token, "Accept": "application/json",
                        "User-Agent": "remind-suggest-identity-links"}
        self.timeout = timeout

    def by_id(self, public_id):
        """(status, body) for GET /v1/accounts/by-id/{id}; retries after 429 and transient failures."""
        url = f"{self.base}/v1/accounts/by-id/{quote(public_id, safe='')}"
        for attempt in range(6):
            try:
                with urlopen(Request(url, headers=self.headers), timeout=self.timeout) as response:
                    return response.status, json.load(response)
            except HTTPError as error:
                body = error.read()
                if error.code == 429 or error.code >= 500:
                    wait = float(error.headers.get("Retry-After") or 2 ** attempt)
                    time.sleep(min(wait, 60))
                    continue
                try:
                    return error.code, json.loads(body or b"{}")
                except json.JSONDecodeError:
                    return error.code, {}
            except URLError as error:
                if attempt == 5:
                    raise SystemExit(f"suggest-identity-links: cannot reach Silicon Accounts at {self.base}: {error.reason}")
                time.sleep(2 ** attempt)
        raise SystemExit(f"suggest-identity-links: Silicon Accounts kept refusing lookups at {self.base}; try again later")


def find_report(text):
    """The link-identities report in the text, even when log lines (plain or JSON) were captured with it."""
    decoder = json.JSONDecoder()
    position = 0
    while position < len(text):
        start = text.find("{", position)
        if start < 0:
            break
        try:
            value, end = decoder.raw_decode(text, start)
        except json.JSONDecodeError:
            position = start + 1
            continue
        if isinstance(value, dict) and "unmatched" in value:
            return value
        position = end
    raise SystemExit("suggest-identity-links: the input holds no link-identities report (no object with `unmatched`)")


def owned(entry):
    return f"{entry.get('schedules', 0)} reminders, {entry.get('destinations', 0)} subscriptions"


def suggest(report, lookup, out):
    unmatched = report.get("unmatched")
    if not isinstance(unmatched, list):
        raise SystemExit("suggest-identity-links: the report's `unmatched` is not a list")
    suggested = review = 0
    out.write(f"# Suggested by scripts/suggest-identity-links.py at {datetime.now(timezone.utc):%Y-%m-%dT%H:%M:%SZ}"
              f" for {len(unmatched)} unlinked principals.\n"
              "# Lines are iam_principal_id,accounts_uuid. Confirm every identity and custodian before uncommenting a suggestion; handles are not identity proof.\n")
    for entry in unmatched:
        principal = entry.get("iam_principal_id")
        public_id = entry.get("iam_public_id")
        if not public_id:
            out.write(f"# REVIEW {principal}: owns {owned(entry)} but has no si:/c: id; find its account and add "
                      f"'{principal},<uuid>'\n")
            review += 1
            continue
        expected = next((kind for prefix, kind in KINDS.items() if public_id.startswith(prefix)), None)
        status, body = lookup.by_id(public_id)
        code = (body.get("error") or {}).get("code") if isinstance(body.get("error"), dict) else body.get("code")
        if status == 200 and body.get("uuid"):
            kind, state, current = body.get("kind"), body.get("status"), body.get("id")
            custodian = (body.get("custodian") or {}).get("id")
            if current != public_id:
                out.write(f"# REVIEW {public_id}: Silicon Accounts answered for {current}; check by hand\n")
                review += 1
            elif expected and kind != expected:
                out.write(f"# REVIEW {public_id}: now a {kind} account ({body['uuid']}), but it was a {expected}; "
                          "a different account took the id\n")
                review += 1
            elif state != "active":
                out.write(f"# REVIEW {public_id}: account {body['uuid']} is {state}; link it only if its data should "
                          "follow it\n")
                review += 1
            else:
                detail = f"{kind}" + (f", custodian {custodian}" if custodian else "")
                out.write(f"# REVIEW {public_id}: {detail}; owns {owned(entry)}\n# {public_id},{body['uuid']}\n")
                suggested += 1
        elif status == 404:
            why = "the account was deleted" if code == "account_deleted" else (
                "no account holds this id now (it changed, was released, or was never claimed)")
            out.write(f"# REVIEW {public_id}: {why}; owns {owned(entry)}. Find the account and add "
                      f"'{public_id},<uuid>', or leave it unlinked\n")
            review += 1
        elif status in (401, 403):
            raise SystemExit("suggest-identity-links: Silicon Accounts refused Remind's app credentials; check "
                             "REMIND_APP_ID and REMIND_APP_SECRET")
        else:
            out.write(f"# REVIEW {public_id}: lookup answered {status} {code or ''}; check by hand\n")
            review += 1
    return suggested, review


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("report", help="the JSON report of `remind-migrate link-identities --dry-run` ('-' for stdin)")
    parser.add_argument("--output", help="write the mapping here instead of standard output")
    args = parser.parse_args(argv)
    secret = os.environ.get("REMIND_APP_SECRET")
    if not secret:
        raise SystemExit("suggest-identity-links: set REMIND_APP_SECRET (Remind's app secret at Silicon Accounts)")
    base = os.environ.get("ACCOUNTS_API_URL") or os.environ.get("ACCOUNTS_URL") or "https://accounts.teamofsilicons.com"
    source = sys.stdin if args.report == "-" else open(args.report, encoding="utf-8")
    with source:
        text = source.read()
    report = find_report(text)
    lookup = Lookup(base, os.environ.get("REMIND_APP_ID") or "remind", secret)
    out = open(args.output, "w", encoding="utf-8", newline="\n") if args.output else sys.stdout
    try:
        suggested, review = suggest(report, lookup, out)
    finally:
        if args.output:
            out.close()
    print(f"suggest-identity-links: {suggested} links suggested, {review} to review", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
