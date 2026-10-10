#!/usr/bin/env python3
"""End-to-end checks of Remind against a local Silicon Accounts stack: real sign-ins, tokens, proofs and webhooks.

    REMIND_TEST_STACK=/path/to/test-stack.json scripts/e2e-accounts.sh [--only 1,4,5] [--keep-running]

Starts the development stack (dev_accounts.py up) when it is not running, and stops it again at the end unless
--keep-running or it was already running. Every run makes its own test accounts with the stack's mint helper
(emails remind-e2e-<role>-<run>@example.test, Silicons si:remind-e2e-<role>-<run>), then runs:

  1  Carbon on the API    a Carbon signed in through the hosted pages (code + PKCE, exchanged with the app secret)
                          creates, lists, reads, rotates the key of (updates), cleans and retires a test
                          environment; it cannot create reminders (Silicons write, Carbons read)
  2  Silicon on the CLI   a new Silicon signs in with a short-lived token in an empty SILICON_HOME; subscribe,
                          create, list, get, edit, pause, resume, archive; logout ends that sign-in only
  3  Device flow          `remind login` as the Carbon, approved the way the account site approves it, then commands
  4  Circle and sharing   custodian and sibling read; an unrelated Carbon sees nothing until shared by c: id; a
                          Silicon outside the circle only after it allowed the owner; unsharing removes access
  5  Webhooks             the custodian changes the Silicon's id -> Remind shows it; a real replay is ignored; forged,
                          unsigned, stale and repeated deliveries; the Silicon removes Remind -> its tokens are
                          refused until it signs in again
  6  Proofs               User verification proofs from `interface` read for the Carbon; writes, a missing scope,
                          another receiving app, a revoked proof and an issuer not allowed are refused
  7  Discovery            the archive scripts/package-apps.sh makes answers the three discovery commands in an
                          empty home and writes nothing there
  8  Restart safety       API and worker restart: sign-ins keep working (stateless tokens), dedupe persists

Prints PASS/FAIL per check; the transcript (tokens masked) and results go to .mig/e2e/<run>/. Exit 1 when any check
fails. Configuration (environment):
  REMIND_TEST_STACK        the stack file (accounts URLs, app secrets, `cli`); required
  REMIND_E2E_MINT          the mint helper (default: mint.mts beside the stack file)
  REMIND_E2E_TSX           tsx to run it (default: <silicon-accounts>/testkit/node_modules/.bin/tsx, found from
                           the stack file's `cli` path, else tsx on PATH)
  REMIND_E2E_ACCOUNTS_CLI  the local stack's silicon-accounts CLI (default: the stack file's `cli`)
  REMIND_E2E_RUN           run suffix (default: time-based); REMIND_BIN_DIR, CARGO_TARGET_DIR as for dev_accounts.py
  SILICON_APPS             silicon-apps for scenario 7 (default: PATH, then ~/.apps/bin/silicon-apps; only its
                           local validate and pack are used, with an empty home)
"""
import argparse
import base64
import datetime
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
import dev_accounts  # noqa: E402  (the same directory)

ROOT = dev_accounts.ROOT
MASK = re.compile(r"\b(slt|sar|sap|sapr|stk|whsec|oac)[_-][A-Za-z0-9_-]{8,}|eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_.-]+")


TEST_KEY = re.compile(r'("key":\s*")[A-Za-z0-9]{32}"')


def mask(text):
    text = MASK.sub(lambda m: (m.group(1) + "_<masked>") if m.group(1) else "<jwt>", text)
    return TEST_KEY.sub(r'\1<test key>"', text)


class Run:
    """Configuration, transcript and results of one run."""

    def __init__(self, cfg, stack, out):
        self.cfg, self.stack, self.out = cfg, stack, out
        self.failed, self.passed = [], []
        self.transcript = open(out / "transcript.txt", "a")
        cli = (stack.get("cli") or "").split()
        accounts_repo = cli[0].split("/target/")[0] if cli and "/target/" in cli[0] else None
        stack_dir = Path(os.environ["REMIND_TEST_STACK"]).resolve().parent
        self.mint = os.environ.get("REMIND_E2E_MINT") or str(stack_dir / "mint.mts")
        self.tsx = os.environ.get("REMIND_E2E_TSX") or (
            f"{accounts_repo}/testkit/node_modules/.bin/tsx" if accounts_repo else shutil.which("tsx") or "tsx")
        self.accounts_cli = os.environ.get("REMIND_E2E_ACCOUNTS_CLI") or (cli[0] if cli else "silicon-accounts")
        self.remind = str(cfg.bin_dir / "remind")
        for what, path in (("mint helper", self.mint), ("tsx", self.tsx), ("silicon-accounts CLI", self.accounts_cli),
                           ("remind CLI", self.remind)):
            if not Path(path).exists():
                raise dev_accounts.Failure(f"the {what} is not at {path} (see the configuration in --help)")

    def log(self, text):
        self.transcript.write(mask(text) + "\n")
        self.transcript.flush()

    def section(self, title):
        print(f"\n== {title}")
        self.log(f"\n== {title}")

    def check(self, label, ok, detail=""):
        line = ("PASS " if ok else "FAIL ") + label + ("" if ok else f"  -> {mask(str(detail))[:600]}")
        print(line)
        self.log(line)
        (self.passed if ok else self.failed).append(label)
        return ok

    # --- HTTP ----------------------------------------------------------------------------------------------------

    def call(self, method, url, auth=None, body=None, headers=None, basic=None, show=True):
        """One request. `auth` is a full Authorization value ('Bearer …' / 'Proof …')."""
        extra = dict(headers or {})
        if auth:
            extra["Authorization"] = auth
        status, answer = dev_accounts.http(method, url, body=body, basic=basic, headers=extra)
        if show:
            who = auth.split(" ")[0] if auth else ("app credentials" if basic else "no credential")
            self.log(f"$ {method} {url} [{who}]" + (f" {json.dumps(body)}" if body is not None else ""))
            self.log(f"  -> {status} {json.dumps(answer)[:700]}")
        return status, answer

    def remind_api(self, method, path, auth=None, body=None, headers=None):
        if method in ("POST", "PATCH", "PUT") and path.startswith("/schedules"):
            headers = {"Idempotency-Key": f"e2e-{uuid.uuid4()}", **(headers or {})}
        return self.call(method, f"{self.cfg.api_url}/api/v2{path}", auth, body, headers)

    def accounts_app(self, method, path, body=None, app="remind"):
        headers = {"Idempotency-Key": str(uuid.uuid4())} if method in ("POST", "PUT") else None
        basic = (app, self.stack["apps"][app]["app_secret"])
        return self.call(method, self.cfg.accounts_api_url + path, body=body, headers=headers, basic=basic)

    def accounts_me(self, method, path, token, body=None):
        headers = {"Idempotency-Key": str(uuid.uuid4())} if method in ("POST", "PATCH", "PUT") else None
        return self.call(method, self.cfg.accounts_api_url + path, f"Bearer {token}", body, headers)

    # --- identities (the stack's mint helper) ----------------------------------------------------------------------

    def mint_json(self, *args):
        """Runs the mint helper; one retry after the stack's per-network code limit clears (about 30 s)."""
        for attempt in (1, 2):
            result = subprocess.run([self.tsx, self.mint, *args], capture_output=True, text=True, timeout=180,
                                    check=False)
            self.log(f"$ mint {' '.join(args)}  -> exit {result.returncode}")
            if result.returncode == 0:
                return json.loads(result.stdout)
            # The per-network limit clears within a minute (the stack's janitor); the per-address one does not.
            if attempt == 1 and re.search(r"from this network", result.stderr, re.I):
                self.log("  (code limit for this network; waiting 35 s once)")
                time.sleep(35)
                continue
            raise dev_accounts.Failure(f"mint {args[0]} failed: {mask(result.stderr.strip())[-800:]}")
        raise AssertionError("unreachable")

    # --- the remind CLI ------------------------------------------------------------------------------------------

    def cli_env(self, home):
        return {"PATH": "/usr/bin:/bin", "HOME": str(home), "SILICON_HOME": str(home),
                "REMIND_URL": self.cfg.api_url, "ACCOUNTS_URL": self.cfg.accounts_url}

    def remind_cli(self, home, *args, stdin=None, timeout=90, label=None):
        """Runs `remind` with only PATH, HOME, SILICON_HOME and the two origins. Returns (exit, json|None, out, err)."""
        result = subprocess.run([self.remind, *args], input=stdin, capture_output=True, text=True,
                                env=self.cli_env(home), timeout=timeout, check=False)
        shown = label or " ".join(args)
        self.log(f"$ remind {shown}" + ("   (token on stdin)" if stdin else ""))
        for line in result.stdout.strip().splitlines()[:40]:
            self.log("  " + line)
        for line in result.stderr.strip().splitlines()[:12]:
            self.log("  stderr: " + line)
        self.log(f"  exit={result.returncode}")
        try:
            parsed = json.loads(result.stdout) if result.stdout.strip() else None
        except ValueError:
            parsed = None
        if parsed is None and result.stderr.strip().startswith("{"):
            try:
                parsed = json.loads(result.stderr.strip().splitlines()[-1])
            except ValueError:
                parsed = None
        return result.returncode, parsed, result.stdout, result.stderr

    def accounts_cli_run(self, home, *args, stdin=None):
        env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "ACCOUNTS_HOME": str(home)}
        result = subprocess.run([self.accounts_cli, "--url", self.cfg.accounts_url, *args], input=stdin,
                                capture_output=True, text=True, env=env, timeout=60, check=False)
        self.log(f"$ silicon-accounts {' '.join(args)}" + ("   (STK on stdin)" if stdin else ""))
        self.log("  " + (result.stdout.strip() or result.stderr.strip())[:600].replace("\n", "\n  "))
        self.log(f"  exit={result.returncode}")
        return result.returncode, result.stdout, result.stderr


def code(body):
    return dev_accounts.error_code(body)


def eventually(probe, seconds=30, every=0.5):
    """Calls probe() -> (ok, detail) until ok or the deadline; returns the last (ok, detail)."""
    deadline = time.time() + seconds
    ok, detail = probe()
    while not ok and time.time() < deadline:
        time.sleep(every)
        ok, detail = probe()
    return ok, detail


class People:
    """The run's test accounts, made on first use and reused (the stack limits email codes per address)."""

    def __init__(self, run, suffix):
        self.run, self.suffix = run, suffix
        self.carbons, self.silicons, self.app_tokens, self.first_party = {}, {}, {}, {}

    def email(self, role):
        return f"remind-e2e-{role}-{self.suffix}@example.test"

    def carbon(self, role):
        """A Carbon signed in to Remind through the hosted pages; its Remind access token and account."""
        if role not in self.carbons:
            redirect = f"http://localhost:{self.run.cfg.base}/auth/callback"
            signed = self.run.mint_json("app-signin", "--app", "remind", "--email", self.email(role),
                                        "--redirect", redirect, "--exchange")
            tokens = signed["tokens"]
            account = tokens["account"]
            self.carbons[role] = {"email": self.email(role), "uuid": account["uuid"], "id": account["id"],
                                  "access_token": tokens["access_token"], "refresh_token": tokens["refresh_token"]}
        return self.carbons[role]

    def bearer(self, role):
        if role in self.carbons:
            return "Bearer " + self.carbons[role]["access_token"]
        return "Bearer " + self.silicon_token(role)

    def silicon(self, role, custodian, via_session=False):
        """A Silicon looked after by `custodian`: made by the mint helper, or (via_session) with the custodian's
        own Silicon Accounts session, as the account site does; that costs no email code (the stack allows 10 per
        address in 10 minutes, and each new Carbon's hosted sign-in already spends 3)."""
        if role not in self.silicons and via_session:
            handle = f"remind-e2e-{role}-{self.suffix}"
            status, made = self.run.accounts_me("POST", "/v1/me/silicons", self.carbon_first_party(custodian),
                                                {"id": f"si:{handle}", "display_name": handle})
            if status != 201:
                raise dev_accounts.Failure(f"POST /v1/me/silicons answered {status} {code(made)}")
            custodian_account = self.carbons.get(custodian, {})
            self.silicons[role] = {"uuid": made["silicon"]["uuid"], "id": made["silicon"]["id"], "stk": made["stk"],
                                   "custodian": {"uuid": custodian_account.get("uuid"), "id": custodian_account.get("id")}}
        if role not in self.silicons:
            made = self.run.mint_json("silicon", "--custodian-email", self.email(custodian),
                                      "--handle", f"remind-e2e-{role}-{self.suffix}")
            self.silicons[role] = {"uuid": made["uuid"], "id": made["id"], "stk": made["stk"],
                                   "custodian": made["custodian"]}
        return self.silicons[role]

    def slt(self, role, app="remind"):
        silicon = self.silicons[role]
        return self.run.mint_json("slt", "--silicon", silicon["id"], "--stk", silicon["stk"], "--app", app)["slt"]

    def silicon_token(self, role, fresh=False):
        """A Remind access token for a Silicon (a short-lived token exchanged with the app credentials)."""
        if fresh or role not in self.app_tokens:
            tokens = self.run.mint_json("app-token", "--app", "remind", "--slt", self.slt(role))
            self.app_tokens[role] = tokens["access_token"]
        return self.app_tokens[role]

    def carbon_first_party(self, role):
        """The Carbon's own Silicon Accounts session token (what the account site holds)."""
        if role not in self.first_party:
            self.first_party[role] = self.run.mint_json("carbon", "--email", self.email(role))["access_token"]
        return self.first_party[role]

    def app_signin(self, role, app, redirect):
        signed = self.run.mint_json("app-signin", "--app", app, "--email", self.email(role), "--redirect", redirect,
                                    "--exchange")
        return signed["tokens"]["access_token"]


# --- Silicon Accounts webhook deliveries made by hand (forged, stale, repeated) -------------------------------------

def sign(secret, timestamp, body):
    return "v1=" + hmac.new(secret.encode(), f"{timestamp}.".encode() + body, hashlib.sha256).hexdigest()


def delivery(event_type, data, event_id=None, account_uuid=None):
    event_id = event_id or str(uuid.uuid4())
    occurred = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    body = {"app_id": "remind", "data": data, "event_id": event_id, "occurred_at": occurred, "silicon": None,
            "type": event_type}
    if account_uuid:
        body["data"] = {"uuid": account_uuid, **data}
    return event_id, json.dumps(body, separators=(",", ":"), sort_keys=True).encode()


def post_delivery(run, body, secret=None, timestamp=None, signature=None, event_id="", event_type=""):
    """POSTs exact bytes to Remind's webhook, signed with `secret` (or the given signature, or unsigned)."""
    from urllib.error import HTTPError
    from urllib.request import Request
    timestamp = int(time.time()) if timestamp is None else timestamp
    headers = {"Content-Type": "application/json", "User-Agent": "SiliconAccounts-Webhooks/1"}
    if event_id:
        headers["X-Accounts-Event-Id"] = event_id
        headers["X-Accounts-Event-Type"] = event_type
    if secret or signature:
        headers["X-Accounts-Timestamp"] = str(timestamp)
        headers["X-Accounts-Signature"] = signature or sign(secret, timestamp, body)
    request = Request(run.cfg.webhook_url, data=body, method="POST", headers=headers)
    try:
        with dev_accounts.OPENER.open(request, timeout=15) as response:
            status, raw = response.status, response.read()
    except HTTPError as error:
        with error:
            status, raw = error.code, error.read()
    answer = json.loads(raw) if raw else None
    run.log(f"$ POST {run.cfg.webhook_url} [{'signed' if secret else 'forged' if signature else 'unsigned'}, "
            f"timestamp {timestamp}] {body[:160].decode()}…")
    run.log(f"  -> {status} {json.dumps(answer)[:300]}")
    return status, answer


def receipts(run, event_id):
    """How many times Remind recorded a Silicon Accounts event id (it must stay 1)."""
    out = dev_accounts.psql(run.cfg, "SELECT count(*) FROM internal_event_receipts "
                                     f"WHERE source = 'silicon-accounts' AND event_id = '{event_id}'", run.cfg.db)
    return int(out or 0)


def outcomes_logged(run, event_id=None, outcome=None):
    """Webhook outcomes in the API log (ANSI colours removed), newest last."""
    text = (run.cfg.dir / "logs" / "remind-api.log").read_text(errors="replace")
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    lines = [line for line in text.splitlines() if "webhook delivery handled" in line]
    if outcome:
        lines = [line for line in lines if f'event.outcome="{outcome}"' in line or f"event.outcome={outcome}" in line]
    return lines


def replay(run, item):
    """Replays one delivery through Silicon Accounts and waits until it is delivered again and Remind has logged
    one more duplicate. Returns (sent, answered as a duplicate, detail)."""
    before = len(outcomes_logged(run, outcome="duplicate"))
    replays_before = item.get("manual_replays", 0)
    status, answer = run.accounts_app("POST", "/v1/apps/remind/webhook/replay", {"delivery_ids": [item["id"]]})
    sent = status == 200 and item["id"] in (answer or {}).get("replayed", [])
    if not sent:
        return False, False, (status, answer)

    def delivered_again():
        st, record = run.call("GET", f"{run.cfg.accounts_api_url}/v1/apps/remind/webhook/deliveries/{item['id']}",
                              basic=("remind", run.stack["apps"]["remind"]["app_secret"]), show=False)
        done = (st == 200 and record.get("manual_replays", 0) > replays_before and record.get("status") == "delivered"
                and record.get("last_status") == 200 and len(outcomes_logged(run, outcome="duplicate")) > before)
        return done, record
    answered, record = eventually(delivered_again, 30, 1.0)
    if answered:
        item["manual_replays"] = record.get("manual_replays", replays_before + 1)
    run.log(f"  replay of {item['id']}: manual_replays {record.get('manual_replays')}, status {record.get('status')}, "
            f"last_status {record.get('last_status')}")
    return True, answered, record


def find_delivery(run, event_type, account_uuid, newer_than=None, seconds=30):
    """The newest delivery of `event_type` about `account_uuid` at Silicon Accounts, once it is delivered."""
    def probe():
        status, page = run.call("GET", f"{run.cfg.accounts_api_url}/v1/apps/remind/webhook/deliveries?limit=50",
                                basic=("remind", run.stack["apps"]["remind"]["app_secret"]), show=False)
        if status != 200:
            return False, (status, page)
        for item in page.get("items", []):
            if item.get("type") == event_type and item.get("account_uuid") == account_uuid \
                    and (newer_than is None or item.get("created_at", "") > newer_than):
                return item.get("status") == "delivered", item
        return False, f"no {event_type} delivery for {account_uuid} yet"
    return eventually(probe, seconds, 1.0)


# --- scenarios ---------------------------------------------------------------------------------------------------------

def scenario_1(run, people, state):
    """Carbon on the API: a test environment is the Carbon's own resource (reminders are written by Silicons)."""
    run.section("1. Carbon on the API (hosted sign-in, real access token)")
    c1 = people.carbon("c1")
    bearer = people.bearer("c1")
    status, me = run.remind_api("GET", "/auth/me", bearer)
    run.check("the Carbon's access token from the hosted pages is accepted (/auth/me)",
              status == 200 and me.get("uuid") == c1["uuid"] and me.get("kind") == "carbon"
              and me.get("can_manage_reminders") is False, (status, me))
    status, body = run.remind_api("POST", "/schedules", bearer,
                                  {"text": "never", "kind": "recurring", "cron": "0 9 * * *", "timezone": "UTC"})
    run.check("a Carbon cannot create a reminder (403 silicon_only)", status == 403 and code(body) == "silicon_only",
              (status, body))

    name = f"e2e-{people.suffix}"
    status, created = run.remind_api("POST", "/test-environments", bearer,
                                     {"name": name, "description": "Remind e2e run " + people.suffix})
    env = (created or {}).get("environment", {})
    key = (created or {}).get("key", "")
    run.check("create: the Carbon creates a test environment it owns, with a 32-character key",
              status == 201 and env.get("owner", {}).get("uuid") == c1["uuid"]
              and re.fullmatch(r"[A-Za-z0-9]{32}", key or "") is not None, (status, created))
    if status != 201:
        return
    state["env_id"] = env_id = env["id"]
    status, page = run.remind_api("GET", "/test-environments", bearer)
    run.check("list: the environment is listed", status == 200 and env_id in [e["id"] for e in page.get("items", [])],
              (status, page))
    status, one = run.remind_api("GET", f"/test-environments/{env_id}", bearer)
    run.check("read: the environment reads back", status == 200 and one.get("name") == name, (status, one))
    status, inside = run.remind_api("GET", "/schedules", bearer, headers={"X-Remind-Test-Key": key})
    run.check("its key selects the empty environment for the Carbon's requests",
              status == 200 and inside.get("items") == [], (status, inside))

    status, rotated = run.remind_api("POST", f"/test-environments/{env_id}/key-rotations", bearer)
    new_key = (rotated or {}).get("key", "")
    run.check("update: rotating the key gives a new one", status == 200 and new_key and new_key != key,
              (status, rotated))
    status, shown = run.remind_api("GET", f"/test-environments/{env_id}/key", bearer)
    run.check("the environment's key now reads as the new one", status == 200 and shown.get("key") == new_key,
              (status, code(shown)))
    status, body = run.remind_api("GET", "/schedules", bearer, headers={"X-Remind-Test-Key": key})
    run.check("the old key no longer opens the environment", status in (401, 403, 404), (status, body))
    status, body = run.call("POST", f"{run.cfg.api_url}/api/v2/testing-environment/cleanings",
                            headers={"X-Remind-Test-Key": new_key})
    run.check("anyone holding the key can clean the environment", status == 204, (status, body))

    other = people.carbon("c2")
    status, body = run.remind_api("GET", f"/test-environments/{env_id}", people.bearer("c2"))
    run.check(f"an unrelated Carbon ({other['id']}) cannot see it (404)", status == 404, (status, body))

    status, body = run.remind_api("DELETE", f"/test-environments/{env_id}", bearer)
    run.check("delete: the Carbon retires it (204, 30 days to restore)", status == 204, (status, body))
    status, page = run.remind_api("GET", "/test-environments", bearer)
    run.check("a retired environment is not listed",
              status == 200 and env_id not in [e["id"] for e in page.get("items", [])], (status, page))
    status, restored = run.remind_api("POST", f"/test-environments/{env_id}/restorations", bearer)
    run.check("it can be restored within 30 days, with a new key",
              status == 200 and (restored or {}).get("key") not in (None, key, new_key), (status, restored))
    status, body = run.remind_api("DELETE", f"/test-environments/{env_id}", bearer)
    run.check("and retired again", status == 204, (status, body))


def one_time_cron(minutes_ahead=2):
    when = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(minutes=minutes_ahead)).replace(
        second=0, microsecond=0)
    return when, f"{when.minute} {when.hour} {when.day} {when.month} *"


def expire_soon(home):
    """Makes every saved sign-in's access token expire in 5 s (the CLI refreshes when under 60 s are left);
    returns the refresh tokens' first characters, to see them rotate."""
    path = Path(home) / ".remind" / "state.json"
    saved = json.loads(path.read_text())
    for sign_in in saved.get("sign_ins", {}).values():
        sign_in["expires_at"] = int(time.time()) + 5
    path.write_text(json.dumps(saved))
    return refresh_prefixes(home)


def refresh_prefixes(home):
    saved = json.loads((Path(home) / ".remind" / "state.json").read_text())
    return sorted(sign_in["refresh_token"][:12] for sign_in in saved.get("sign_ins", {}).values())


def concurrent_refresh(run, home, label):
    """Four commands at once with an expiring token: one refresh, all succeed, the sign-in survives."""
    before = expire_soon(home)
    processes = [subprocess.Popen([run.remind, "list", "--json"], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  text=True, env=run.cli_env(home)) for _ in range(4)]
    results = [(process.wait(timeout=90), process.stderr.read()[-300:]) for process in processes]
    after = refresh_prefixes(home)
    run.log(f"$ 4 x remind list --json at once ({label}, token expiring): exits {[r[0] for r in results]}, "
            f"refresh token {mask(before[0]) if before else None} -> {mask(after[0]) if after else None}")
    run.check(f"four {label} commands at once with an expiring token all succeed, after one refresh",
              all(code_ == 0 for code_, _ in results) and before != after, results)
    time.sleep(3)
    exit_code, status, _, _ = run.remind_cli(home, "login", "status", "--json")
    run.check("and the sign-in survives (no refresh token was used twice, which would end it)",
              exit_code == 0 and (status or {}).get("verified") is True, (exit_code, status))


def scenario_2(run, people, state):
    """Silicon on the CLI: a short-lived token sign-in in an empty home, the reminder journey, logout."""
    run.section("2. Silicon on the CLI (short-lived token, fresh SILICON_HOME)")
    suspension_start(run, people, state)
    people.carbon("c1")
    s1 = people.silicon("s1", "c1")
    home = Path(tempfile.mkdtemp(prefix="silicon-s1.", dir=run.out))
    state["s1_home"] = home
    slt = people.slt("s1")
    exit_code, signed, _, _ = run.remind_cli(home, "login", "--slt-stdin", "--json", stdin=slt)
    run.check("remind login --slt-stdin signs the Silicon in",
              exit_code == 0 and (signed or {}).get("authenticated") is True and signed.get("uuid") == s1["uuid"]
              and signed.get("kind") == "silicon", (exit_code, signed))
    state["s1_known"] = exit_code == 0
    exit_code, again, _, _ = run.remind_cli(home, "login", "--slt-stdin", "--json", stdin=slt)
    run.check("the same short-lived token cannot be used twice (slt_already_used)",
              exit_code == 3 and code(again) == "slt_already_used", (exit_code, again))
    exit_code, status, _, _ = run.remind_cli(home, "login", "status", "--json")
    run.check("login status --json: authenticated, verified, who",
              exit_code == 0 and status.get("authenticated") is True and status.get("verified") is True
              and status.get("uuid") == s1["uuid"] and status.get("id") == s1["id"] and status.get("kind") == "silicon",
              (exit_code, status))

    state["signing_secret"] = secret = f"e2e-signing-{people.suffix}"
    exit_code, sub, _, _ = run.remind_cli(home, "webhook", "subscribe", run.cfg.receiver_url, "--secret-stdin",
                                          "--json", stdin=secret)
    run.check("write: webhook subscribe (signed deliveries to the local receiver)",
              exit_code == 0 and (sub or {}).get("silicon_uuid") == s1["uuid"], (exit_code, sub))
    exit_code, r1, _, _ = run.remind_cli(home, "create", "--text", f"E2E {people.suffix}: review the build",
                                         "--cron", "0 9 * * MON-FRI", "--timezone", "Asia/Kolkata", "--json")
    run.check("write: create a recurring reminder", exit_code == 0 and (r1 or {}).get("owner", {}).get("uuid") == s1["uuid"],
              (exit_code, r1))
    when, cron = one_time_cron()
    exit_code, r2, _, _ = run.remind_cli(home, "create", "--text", f"E2E {people.suffix}: one-time ping",
                                         "--kind", "one-time", "--cron", cron, "--timezone", "UTC", "--json")
    run.check(f"write: create a one-time reminder due at {when:%H:%M} UTC", exit_code == 0 and (r2 or {}).get("id"),
              (exit_code, r2))
    exit_code, r3, _, _ = run.remind_cli(home, "create", "--text", f"E2E {people.suffix}: weekly review",
                                         "--cron", "0 9 * * 1", "--timezone", "Asia/Kolkata", "--json")
    run.check("write: create the reminder the sharing checks use", exit_code == 0 and (r3 or {}).get("id"),
              (exit_code, r3))
    if not (r1 and r2 and r3):
        return
    state.update(r1=r1["id"], r2=r2["id"], r2_due=when, r3=r3["id"])
    exit_code, listed, _, _ = run.remind_cli(home, "list", "--json")
    ids = [item["id"] for item in (listed or {}).get("items", [])]
    run.check("read: list shows all three", exit_code == 0 and {r1["id"], r2["id"], r3["id"]} <= set(ids),
              (exit_code, ids))
    exit_code, got, _, _ = run.remind_cli(home, "get", r1["id"], "--json")
    run.check("read: get shows the reminder and its next trigger",
              exit_code == 0 and got.get("timezone") == "Asia/Kolkata" and got.get("next_run_at"), (exit_code, got))
    exit_code, edited, _, _ = run.remind_cli(home, "edit", r1["id"], "--text",
                                             f"E2E {people.suffix}: review the release build", "--json")
    run.check("write: edit the text", exit_code == 0 and edited.get("text", "").endswith("release build"),
              (exit_code, edited))
    exit_code, paused, _, _ = run.remind_cli(home, "pause", r1["id"], "--json")
    exit_code2, resumed, _, _ = run.remind_cli(home, "resume", r1["id"], "--json")
    run.check("write: pause, then resume", exit_code == 0 and exit_code2 == 0, (paused, resumed))
    exit_code, archived, _, _ = run.remind_cli(home, "archive", r1["id"], "--json")
    exit_code2, section, _, _ = run.remind_cli(home, "list", "--archived", "--json")
    run.check("write: archive; it moves to the 45-day archive",
              exit_code == 0 and r1["id"] in [i["id"] for i in (section or {}).get("items", [])], (archived, section))

    # Logging out of one machine ends that sign-in only: Silicon Accounts tells Remind `app_revoked`.
    other_token = people.silicon_token("s1", fresh=True)
    exit_code, out, _, _ = run.remind_cli(home, "logout", "--json")
    run.check("logout revokes the sign-in at Silicon Accounts",
              exit_code == 0 and out.get("signed_out") is True and out.get("revoked") is True, (exit_code, out))
    exit_code, status, stdout, _ = run.remind_cli(home, "login", "status", "--json")
    run.check("login status --json says {\"authenticated\":false} (exit 0)",
              exit_code == 0 and status == {"authenticated": False}, (exit_code, stdout))
    exit_code, refused, _, _ = run.remind_cli(home, "list", "--json")
    run.check("commands need a sign-in again (exit 3, not_signed_in)",
              exit_code == 3 and code(refused) == "not_signed_in", (exit_code, refused))
    time.sleep(3)
    status, body = run.remind_api("GET", "/schedules", f"Bearer {other_token}")
    run.check("the Silicon's other sign-in keeps working after that logout (app_revoked is not a sign-out)",
              status == 200, (status, body))
    exit_code, signed, _, _ = run.remind_cli(home, "login", people.slt("s1"), "--json", label="login slt_… --json")
    run.check("signs in again with the positional form `remind login <slt>` (as the Silicon runtime does)",
              exit_code == 0 and (signed or {}).get("authenticated") is True, (exit_code, signed))
    concurrent_refresh(run, home, "Silicon")


def ensure_s1(run, people, state, cli=False):
    """The main Silicon, signed in to Remind (Silicon Accounts tells an app only about accounts that signed in to
    it), with a signed-in CLI home when `cli`. Lets every scenario run on its own (--only)."""
    people.carbon("c1")
    s1 = people.silicon("s1", "c1")
    if not state.get("s1_known"):
        status, me = run.remind_api("GET", "/auth/me", people.bearer("s1"))
        run.check(f"{s1['id']} is signed in to Remind", status == 200 and me.get("uuid") == s1["uuid"], (status, me))
        state["s1_known"] = True
    if cli and not state.get("s1_home"):
        home = Path(tempfile.mkdtemp(prefix="silicon-s1.", dir=run.out))
        exit_code, signed, _, _ = run.remind_cli(home, "login", "--slt-stdin", "--json", stdin=people.slt("s1"))
        run.check("the Silicon signs the CLI in", exit_code == 0 and (signed or {}).get("verified") is True,
                  (exit_code, signed))
        state["s1_home"] = home
    return s1


def ensure_reminder(run, people, state):
    """The reminder the sharing, proof and id checks read (scenario 2 makes it with the CLI)."""
    if not state.get("r3"):
        ensure_s1(run, people, state)
        status, made = run.remind_api("POST", "/schedules", people.bearer("s1"),
                                      {"text": f"E2E {people.suffix}: weekly review", "kind": "recurring",
                                       "cron": "0 9 * * 1", "timezone": "Asia/Kolkata"})
        run.check("the Silicon sets the reminder the next checks read", status == 201, (status, made))
        state["r3"] = (made or {}).get("id")
    return state["r3"]


def device_login(run, people, role, home):
    """`remind login --json` in the background, approved as the Carbon the way the account site's /device page does."""
    label = f"remind e2e {people.suffix} {role}"
    process = subprocess.Popen([run.remind, "login", "--json", "--label", label], stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, env=run.cli_env(home))
    run.log(f"$ remind login --json --label '{label}'   (in the background)")
    user_code, seen = None, []
    deadline = time.time() + 30
    while time.time() < deadline and user_code is None:
        line = process.stderr.readline()
        if not line:
            break
        seen.append(line.strip())
        run.log("  stderr: " + line.strip())
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if event.get("event") == "device_code":
            user_code = event.get("user_code")
    if not user_code:
        process.kill()
        return 1, None, "\n".join(seen)
    run.mint_json("approve", "--email", people.email(role), "--code", user_code)
    stdout, stderr = process.communicate(timeout=60)
    run.log("  " + stdout.strip()[:600])
    run.log(f"  exit={process.returncode}")
    try:
        return process.returncode, json.loads(stdout), user_code
    except ValueError:
        return process.returncode, None, stdout + stderr


def scenario_3(run, people, state):
    """Device flow: the Carbon signs the CLI in with a code it approves, then uses it."""
    run.section("3. Device flow (Carbon CLI sign-in)")
    c1 = people.carbon("c1")
    s1 = ensure_s1(run, people, state)
    ensure_reminder(run, people, state)
    home = Path(tempfile.mkdtemp(prefix="carbon-c1.", dir=run.out))
    state["c1_home"] = home
    exit_code, signed, detail = device_login(run, people, "c1", home)
    run.check("remind login shows a code; approving it signs the Carbon in",
              exit_code == 0 and (signed or {}).get("authenticated") is True and signed.get("uuid") == c1["uuid"]
              and signed.get("kind") == "carbon" and signed.get("method") == "device", (exit_code, signed, detail))
    exit_code, silicons, _, _ = run.remind_cli(home, "silicons", "--json")
    relations = {item["silicon_id"]: item["relation"] for item in (silicons or {}).get("items", [])}
    run.check("remind silicons: the Carbon sees the Silicon it looks after (custodian)",
              exit_code == 0 and relations.get(s1["id"]) == "custodian", (exit_code, relations))
    if state.get("r3"):
        exit_code, listed, _, _ = run.remind_cli(home, "list", "--silicon", s1["id"], "--json")
        run.check("remind list --silicon: the Carbon reads that Silicon's reminders",
                  exit_code == 0 and state["r3"] in [i["id"] for i in (listed or {}).get("items", [])],
                  (exit_code, listed))
        exit_code, refused, _, _ = run.remind_cli(home, "pause", state["r3"], "--json")
        run.check("the custodian cannot change them (exit 4, silicon_only)",
                  exit_code == 4 and code(refused) == "silicon_only", (exit_code, refused))
    exit_code, again, _, _ = run.remind_cli(home, "login", "--json")
    run.check("remind login while signed in says so and changes nothing",
              exit_code == 0 and (again or {}).get("already_signed_in") is True, (exit_code, again))
    before = expire_soon(home)
    exit_code, listed, _, _ = run.remind_cli(home, "silicons", "--json")
    run.check("the Carbon's device sign-in refreshes when its access token runs out",
              exit_code == 0 and refresh_prefixes(home) != before, (exit_code, listed))


def readable(run, auth, reminder):
    status, body = run.remind_api("GET", f"/schedules/{reminder}", auth)
    return status, body


def scenario_4(run, people, state):
    """The custodian circle and explicit sharing by id."""
    run.section("4. Circle and sharing")
    s1 = ensure_s1(run, people, state, cli=True)
    r3 = ensure_reminder(run, people, state)
    s2 = people.silicon("s2", "c1", via_session=True)
    c2 = people.carbon("c2")
    s3 = people.silicon("s3", "c2")
    s1_home = state["s1_home"]

    status, body = readable(run, people.bearer("c1"), r3)
    run.check("custodian: the Carbon reads its Silicon's reminder", status == 200 and body.get("id") == r3,
              (status, body))
    status, body = run.remind_api("GET", f"/schedules?silicon_id={s1['id']}", people.bearer("s2"))
    run.check(f"sibling: {s2['id']} (same custodian) lists {s1['id']}'s reminders",
              status == 200 and r3 in [i["id"] for i in body.get("items", [])], (status, body))
    status, body = run.remind_api("GET", "/silicons", people.bearer("s2"))
    relations = {i["silicon_id"]: i["relation"] for i in (body or {}).get("items", [])}
    run.check("sibling: /silicons names the relation", relations.get(s1["id"]) == "sibling"
              and relations.get(s2["id"]) == "self", relations)
    status, body = run.remind_api("PATCH", f"/schedules/{r3}", people.bearer("s2"), {"text": "not mine"})
    run.check("a sibling cannot change it", status in (403, 404), (status, body))

    status, body = readable(run, people.bearer("c2"), r3)
    run.check(f"an unrelated Carbon ({c2['id']}) cannot read it (404)", status == 404, (status, body))
    status, body = run.remind_api("GET", "/schedules", people.bearer("c2"))
    run.check("and lists nothing", status == 200 and body.get("items") == [], (status, body))

    exit_code, grant, _, _ = run.remind_cli(s1_home, "share", "add", c2["id"], "--json")
    run.check(f"the owner Silicon shares by c: id (remind share add {c2['id']})", exit_code == 0, (exit_code, grant))
    status, body = readable(run, people.bearer("c2"), r3)
    run.check("now that Carbon reads it", status == 200, (status, body))
    status, body = run.remind_api("GET", "/silicons", people.bearer("c2"))
    relations = {i["silicon_id"]: i["relation"] for i in (body or {}).get("items", [])}
    run.check("and sees the Silicon as shared", relations.get(s1["id"]) == "shared", relations)
    exit_code, removed, _, _ = run.remind_cli(s1_home, "share", "remove", c2["id"], "--json")
    status, body = readable(run, people.bearer("c2"), r3)
    run.check("unsharing removes access (404 again)", exit_code == 0 and status == 404, (exit_code, removed, status))

    status, grant = run.remind_api("POST", "/viewers", people.bearer("c1"), {"id": c2["id"], "silicon_id": s1["id"]})
    run.check("the custodian shares its Silicon's reminders too (POST /viewers)", status == 201, (status, grant))
    status, body = readable(run, people.bearer("c2"), r3)
    run.check("the grant works", status == 200, (status, body))
    status, body = run.remind_api("DELETE", f"/viewers/{c2['id']}?silicon_id={s1['id']}", people.bearer("c1"))
    status2, body2 = readable(run, people.bearer("c2"), r3)
    run.check("and the custodian ends it", status == 204 and status2 == 404, (status, body, status2))

    exit_code, refused, _, _ = run.remind_cli(s1_home, "share", "add", s3["id"], "--json")
    run.check(f"Silicons are not open to the world: sharing with {s3['id']} (another custodian's) is refused",
              exit_code == 4 and code(refused) == "silicon_not_open", (exit_code, refused))
    status, body = readable(run, people.bearer("s3"), r3)
    run.check("and that Silicon cannot read it", status == 404, (status, body))
    status, body = run.remind_api("GET", "/silicons", people.bearer("c2"))
    mine = {i["silicon_id"]: i for i in (body or {}).get("items", [])}.get(s3["id"], {})
    run.check(f"{s3['id']}, first met through that share, shows its name and photo once it signed in",
              mine.get("relation") == "custodian" and mine.get("display_name") == f"remind-e2e-s3-{people.suffix}"
              and mine.get("pfp_url", "").startswith("http"), mine)
    status, allowed = run.remind_api("POST", "/allowed-accounts", people.bearer("s3"), {"id": s1["id"]})
    run.check(f"{s3['id']} allows {s1['id']} (POST /allowed-accounts)", status == 201, (status, allowed))
    exit_code, grant, _, _ = run.remind_cli(s1_home, "share", "add", s3["id"], "--json")
    status, body = readable(run, people.bearer("s3"), r3)
    run.check("then the share goes through and it reads the reminder", exit_code == 0 and status == 200,
              (exit_code, grant, status))
    status, body = run.remind_api("DELETE", f"/allowed-accounts/{s1['id']}?silicon_id={s3['id']}", people.bearer("c2"))
    status2, body2 = readable(run, people.bearer("s3"), r3)
    run.check(f"its custodian ({c2['id']}) removes the allowance; the grant it made possible ends",
              status == 204 and status2 == 404, (status, body, status2, body2))


def check_one_time_delivery(run, people, state):
    """The one-time reminder from scenario 2 fired at its minute and reached the receiver, signed, keyed by uuid."""
    if not state.get("r2") or state.get("r2_checked"):
        return
    state["r2_checked"] = True
    due, s1 = state["r2_due"], people.silicons["s1"]
    wait = (due - datetime.datetime.now(datetime.timezone.utc)).total_seconds() + 45
    run.log(f"(waiting up to {max(wait, 0):.0f}s for the one-time reminder due at {due:%H:%M:%S} UTC)")
    path = run.cfg.dir / "deliveries.jsonl"

    def probe():
        if not path.exists():
            return False, "no delivery recorded yet"
        for line in path.read_text().splitlines():
            entry = json.loads(line)
            try:
                body = json.loads(entry["body"])
            except ValueError:
                continue
            if body.get("payload", {}).get("schedule_id") == state["r2"]:
                return True, entry
        return False, "the receiver has not seen it yet"

    ok, entry = eventually(probe, max(wait, 5), 1.0)
    run.check("the one-time reminder fired at its minute and was delivered", ok, entry)
    if not ok:
        return
    body = json.loads(entry["body"])
    headers = entry["headers"]
    expected = base64.b64encode(hmac.new(state["signing_secret"].encode(),
                                         f"{headers.get('webhook-id')}.{headers.get('webhook-timestamp')}.".encode()
                                         + entry["body"].encode(), hashlib.sha256).digest()).decode()
    run.check("the delivery carries silicon_uuid, the current si: id and the exact text",
              body["payload"].get("silicon_uuid") == s1["uuid"] and body["payload"].get("silicon_id") == s1["id"]
              and body["payload"].get("text") == f"E2E {people.suffix}: one-time ping"
              and body.get("type") == "remind.schedule.triggered", body)
    run.check("and is signed with the Silicon's secret (webhook-signature verifies)",
              headers.get("webhook-signature") == f"v1,{expected}", headers.get("webhook-signature"))


def scenario_5(run, people, state):
    """Silicon Accounts webhooks: id change, custodian change, replay, forged deliveries, app removal."""
    run.section("5. Webhooks from Silicon Accounts")
    c1, s1 = people.carbon("c1"), ensure_s1(run, people, state)
    ensure_reminder(run, people, state)
    s2 = people.silicon("s2", "c1", via_session=True)
    c2 = people.carbon("c2")
    check_one_time_delivery(run, people, state)
    suspension_start(run, people, state)
    suspension_check(run, people, state)

    # The custodian changes the Silicon's id; Remind hears account.id_changed and shows the new id.
    new_id = f"si:remind-e2e-s1x-{people.suffix}"
    status, view = run.accounts_me("POST", f"/v1/me/silicons/{s1['uuid']}/id", people.carbon_first_party("c1"),
                                   {"id": new_id})
    run.check(f"the custodian changes {s1['id']} to {new_id} at Silicon Accounts", status == 200, (status, view))
    if status == 200:
        old_id, s1["id"] = s1["id"], new_id
        if state.get("r3"):
            ok, detail = eventually(lambda: (lambda st, b: (st == 200 and b.get("silicon_id") == new_id, (st, b)))(
                *run.remind_api("GET", f"/schedules/{state['r3']}", people.bearer("c1"))), 30, 1.0)
            run.check("Remind shows the new id on the Silicon's reminders (account.id_changed)", ok, detail)
        ok, detail = eventually(lambda: (lambda st, b: (st == 200 and b.get("id") == new_id, (st, b, old_id)))(
            *run.remind_api("GET", "/auth/me", people.bearer("s1"))), 30, 1.0)
        run.check("and to the Silicon itself, even with a token minted under the old id", ok, detail)
        if state.get("s1_home"):
            exit_code, status, _, _ = run.remind_cli(state["s1_home"], "login", "status", "--json")
            run.check("remind login status --json names the Silicon by its new id",
                      exit_code == 0 and (status or {}).get("id") == new_id, (exit_code, status))
        ok, item = find_delivery(run, "account.id_changed", s1["uuid"])
        run.check("Silicon Accounts records the id_changed delivery as delivered", ok, item)
        if ok:
            sent, answered, detail = replay(run, item)
            run.check("a real replay through Silicon Accounts is sent (same event_id)", sent, detail)
            run.check("Remind answers it 2xx and ignores it as a duplicate (one receipt for the event)",
                      answered and receipts(run, item["event_id"]) == 1, (detail, receipts(run, item["event_id"])))
            state["replayed_delivery"] = item

    # The custodian hands a Silicon to another Carbon; Remind hears silicon.custodian_changed. Silicon Accounts
    # tells an app only about accounts that signed in to it, so the Silicon uses Remind first.
    status, me = run.remind_api("GET", "/auth/me", people.bearer("s2"))
    run.check(f"{s2['id']} is signed in to Remind", status == 200 and me.get("uuid") == s2["uuid"], (status, me))
    status, request = run.accounts_me("POST", f"/v1/me/silicons/{s2['uuid']}/transfer", people.carbon_first_party("c1"),
                                      {"to": c2["id"]})
    accepted = None
    if status == 201:
        request_id = (request.get("request") or request)["id"]
        accepted, _ = run.accounts_me("POST", f"/v1/me/custodian-requests/{request_id}/accept",
                                      people.carbon_first_party("c2"), {})
    run.check(f"{c1['id']} transfers {s2['id']} to {c2['id']}, who accepts", status == 201 and accepted == 204,
              (status, request, accepted))
    if accepted == 204:
        def moved():
            st, body = run.remind_api("GET", "/silicons", people.bearer("c2"))
            relations = {i["silicon_id"]: i["relation"] for i in (body or {}).get("items", [])}
            return relations.get(s2["id"]) == "custodian", relations
        ok, detail = eventually(moved, 30, 1.0)
        run.check("the new custodian sees it in Remind (silicon.custodian_changed)", ok, detail)
        st, body = run.remind_api("GET", "/silicons", people.bearer("c1"))
        run.check("the old custodian no longer does",
                  st == 200 and s2["id"] not in [i["silicon_id"] for i in body.get("items", [])], (st, body))
        if state.get("r3"):
            st, body = readable(run, people.bearer("s2"), state["r3"])
            run.check(f"{s2['id']} left {s1['id']}'s circle, so it no longer reads its reminders", st == 404, (st, body))
        s2["custodian"] = {"uuid": c2["uuid"], "id": c2["id"]}

    forged_deliveries(run, state)
    remove_app(run, people, state)
    profile_change(run, people)
    stk_rotation(run, people, state)
    account_deletion(run, people)


def forged_deliveries(run, state):
    secret = run.cfg.webhook_secret()
    event_id, body = delivery("ping", {})
    status, answer = post_delivery(run, body, secret="whsec_" + "0" * 43, event_id=event_id, event_type="ping")
    run.check("a delivery signed with another secret is refused (401 webhook_signature_invalid)",
              status == 401 and code(answer) == "webhook_signature_invalid", (status, answer))
    status, answer = post_delivery(run, body, event_id=event_id, event_type="ping")
    run.check("an unsigned delivery is refused (401)", status == 401, (status, answer))
    status, answer = post_delivery(run, body, secret=secret, timestamp=int(time.time()) - 600, event_id=event_id,
                                   event_type="ping")
    run.check("a correctly signed delivery from 10 minutes ago is refused (stale timestamp)", status == 401,
              (status, answer))
    _, other = delivery("ping", {})
    timestamp = int(time.time())
    status, answer = post_delivery(run, other, signature=sign(secret, timestamp, body), timestamp=timestamp,
                                   event_id=event_id, event_type="ping")
    run.check("a body that does not match its signature is refused", status == 401, (status, answer))
    run.check("none of them was recorded", receipts(run, event_id) == 0, receipts(run, event_id))
    status, answer = post_delivery(run, b'{"type":"ping"}', secret=secret)
    run.check("a signed body that is not an event is refused (400 webhook_body_invalid)",
              status == 400 and code(answer) == "webhook_body_invalid", (status, answer))
    status, first = post_delivery(run, body, secret=secret, event_id=event_id, event_type="ping")
    status2, second = post_delivery(run, body, secret=secret, event_id=event_id, event_type="ping")
    run.check("a genuine delivery is accepted once; the same event_id again is a duplicate",
              status == 200 and (first or {}).get("status") == "ignored" and status2 == 200
              and (second or {}).get("status") == "duplicate" and receipts(run, event_id) == 1, (first, second))
    state["hand_event"] = (event_id, body)


def remove_app(run, people, state):
    """The Silicon removes Remind at Silicon Accounts; its tokens stop working until it signs in again."""
    s1, home = people.silicons["s1"], state.get("s1_home")
    token = people.silicon_token("s1", fresh=True)
    status, _ = run.remind_api("GET", "/schedules", f"Bearer {token}")
    run.check("before: the Silicon's access token works", status == 200, status)
    accounts_home = Path(tempfile.mkdtemp(prefix="accounts-s1.", dir=run.out))
    exit_code, _, err = run.accounts_cli_run(accounts_home, "login", "--silicon", s1["id"], "--stk-stdin",
                                             stdin=s1["stk"])
    exit_code2, out, err2 = run.accounts_cli_run(accounts_home, "--json", "apps", "remove", "remind")
    run.check("the Silicon removes Remind (silicon-accounts login --silicon, then apps remove remind)",
              exit_code == 0 and exit_code2 == 0, (err, out, err2))
    removed_at = time.time()

    def refused():
        st, body = run.remind_api("GET", "/schedules", f"Bearer {token}")
        return st == 401 and code(body) == "token_revoked", (st, body)
    ok, detail = eventually(refused, 30, 1.0)
    run.check(f"its old access token is refused afterwards (401 token_revoked, {time.time() - removed_at:.1f}s)",
              ok, detail)
    ok, item = find_delivery(run, "membership.access_removed", s1["uuid"])
    run.check("Silicon Accounts delivered membership.access_removed", ok, item)
    if home:
        exit_code, body, _, _ = run.remind_cli(home, "list", "--json")
        run.check("the CLI's saved sign-in ended with it (exit 3)", exit_code == 3, (exit_code, body))
    st, body = run.remind_api("GET", "/silicons", people.bearer("c1"))
    run.check("its custodian no longer sees it while access is removed",
              st == 200 and s1["uuid"] not in [i["uuid"] for i in body.get("items", [])], (st, body))
    run.accounts_cli_run(accounts_home, "logout")
    time.sleep(1.2)  # a new sign-in must be at least a second newer than the removal (JWT iat has no fractions)
    if home:
        exit_code, signed, _, _ = run.remind_cli(home, "login", "--slt-stdin", "--json", stdin=people.slt("s1"))
        run.check("signing in to Remind again restores it", exit_code == 0 and (signed or {}).get("verified") is True,
                  (exit_code, signed))
        st, body = run.remind_api("GET", "/silicons", people.bearer("c1"))
        run.check("and its custodian sees it again",
                  st == 200 and s1["uuid"] in [i["uuid"] for i in body.get("items", [])], (st, body))


def profile_change(run, people):
    """account.updated: a new display name at Silicon Accounts reaches Remind."""
    name = f"Remind E2E Carbon {people.suffix}"
    status, body = run.accounts_me("PATCH", "/v1/me", people.carbon_first_party("c1"), {"display_name": name})
    run.check("the Carbon changes its display name at Silicon Accounts", status == 200, (status, code(body)))

    def renamed():
        st, me = run.remind_api("GET", "/auth/me", people.bearer("c1"))
        return st == 200 and me.get("display_name") == name, (st, (me or {}).get("display_name"))
    ok, detail = eventually(renamed, 30, 1.0)
    run.check("Remind shows the new name (account.updated)", ok, detail)


def stk_rotation(run, people, state):
    """membership.signed_out for any reason but app_revoked ends every sign-in: the custodian rotates the STK."""
    s1, home = people.silicons["s1"], state.get("s1_home")
    token = people.silicon_token("s1", fresh=True)
    status, rotated = run.accounts_me("POST", f"/v1/me/silicons/{s1['uuid']}/stk", people.carbon_first_party("c1"), {})
    run.check("the custodian rotates the Silicon's STK (it is signed out everywhere)",
              status == 200 and (rotated or {}).get("stk"), (status, code(rotated)))
    if status != 200:
        return
    s1["stk"] = rotated["stk"]

    def refused():
        st, body = run.remind_api("GET", "/schedules", f"Bearer {token}")
        return st == 401 and code(body) == "token_revoked", (st, body)
    ok, detail = eventually(refused, 30, 1.0)
    run.check("Remind refuses the Silicon's earlier tokens (membership.signed_out, stk_rotated)", ok, detail)
    if home:
        exit_code, body, _, _ = run.remind_cli(home, "list", "--json")
        run.check("the CLI's saved sign-in ended too (exit 3)", exit_code == 3, (exit_code, body))
        time.sleep(1.2)
        exit_code, signed, _, _ = run.remind_cli(home, "login", "--slt-stdin", "--json", stdin=people.slt("s1"))
        run.check("a sign-in with the new STK works", exit_code == 0 and (signed or {}).get("verified") is True,
                  (exit_code, signed))


def account_deletion(run, people):
    """account.deleted: the custodian deletes a Silicon; Remind archives its reminders and ends its subscriptions."""
    s4 = people.silicon("s4", "c1", via_session=True)
    token = "Bearer " + people.silicon_token("s4")
    status, reminder = run.remind_api("POST", "/schedules", token, {"text": f"E2E {people.suffix}: before deletion",
                                                                     "kind": "recurring", "cron": "0 9 * * *",
                                                                     "timezone": "UTC"})
    status2, sub = run.remind_api("POST", "/webhooks", token, {"endpoint_url": run.cfg.receiver_url})
    run.check(f"{s4['id']} sets a reminder and a subscription", status == 201 and status2 == 201, (reminder, sub))
    if status != 201 or status2 != 201:
        return
    status, body = readable(run, people.bearer("c1"), reminder["id"])
    run.check("its custodian reads the reminder", status == 200, (status, body))
    status, body = run.call("DELETE", f"{run.cfg.accounts_api_url}/v1/me/silicons/{s4['uuid']}",
                            f"Bearer {people.carbon_first_party('c1')}", {"confirm": s4["id"]})
    run.check(f"the custodian deletes {s4['id']} at Silicon Accounts", status == 204, (status, body))

    def refused():
        st, body = run.remind_api("GET", "/schedules", token)
        return st == 401 and code(body) == "account_deleted", (st, body)
    ok, detail = eventually(refused, 30, 1.0)
    run.check("Remind refuses the deleted Silicon (401 account_deleted, account.deleted)", ok, detail)
    status, body = readable(run, people.bearer("c1"), reminder["id"])
    run.check("its reminder is gone from its former custodian's view", status == 404, (status, body))
    archived = dev_accounts.psql(run.cfg, "SELECT deleted_at IS NOT NULL AND purge_after > now() + interval '44 days' "
                                          f"FROM schedules WHERE id = '{reminder['id']}'", run.cfg.db)
    disabled = dev_accounts.psql(run.cfg, f"SELECT disabled_at IS NOT NULL FROM hook_destinations WHERE id = '{sub['id']}'",
                                 run.cfg.db)
    run.check("its reminder is archived for 45 days (then the deleted-reminders log) and its subscription disabled",
              archived == "t" and disabled == "t", (archived, disabled))


def deliveries_for(run, schedule_id):
    """(received_at, payload) of every delivery the local receiver got for a reminder."""
    path = run.cfg.dir / "deliveries.jsonl"
    found = []
    if path.exists():
        for line in path.read_text().splitlines():
            entry = json.loads(line)
            try:
                body = json.loads(entry["body"])
            except ValueError:
                continue
            if body.get("payload", {}).get("schedule_id") == schedule_id:
                found.append((entry["received_at"], body["payload"]))
    return found


def suspension_start(run, people, state):
    """A Silicon with an every-minute reminder removes Remind at once; scenario 5 checks nothing fired meanwhile."""
    if state.get("suspended"):
        return
    people.carbon("c1")
    s5 = people.silicon("s5", "c1", via_session=True)
    if datetime.datetime.now().second > 40:  # leave at least 18 s between the removal and the next minute
        time.sleep(62 - datetime.datetime.now().second)
    token = "Bearer " + people.silicon_token("s5")
    status, reminder = run.remind_api("POST", "/schedules", token, {"text": f"E2E {people.suffix}: every minute",
                                                                     "kind": "recurring", "cron": "* * * * *",
                                                                     "timezone": "UTC"})
    status2, _ = run.remind_api("POST", "/webhooks", token, {"endpoint_url": run.cfg.receiver_url})
    home = Path(tempfile.mkdtemp(prefix="accounts-s5.", dir=run.out))
    exit_code, _, err = run.accounts_cli_run(home, "login", "--silicon", s5["id"], "--stk-stdin", stdin=s5["stk"])
    exit_code2, _, err2 = run.accounts_cli_run(home, "--json", "apps", "remove", "remind")
    run.accounts_cli_run(home, "logout")

    def refused():
        st, body = run.remind_api("GET", "/auth/me", token)
        return st == 401 and code(body) == "token_revoked", (st, body)
    ok, detail = eventually(refused, 30, 1.0)
    run.check(f"{s5['id']} sets an every-minute reminder, then removes Remind (suspended from now on)",
              status == 201 and status2 == 201 and exit_code == 0 and exit_code2 == 0 and ok,
              (status, status2, err, err2, detail))
    if status == 201 and ok:
        state["suspended"] = {"schedule": reminder["id"], "at": time.time(),
                              "due": datetime.datetime.fromisoformat(reminder["next_run_at"].replace("Z", "+00:00"))}


def suspension_check(run, people, state):
    """While access is removed nothing fires; signing in again brings the overdue occurrence at once."""
    suspended = state.get("suspended")
    if not suspended or suspended.get("checked"):
        return
    suspended["checked"] = True
    wait = (suspended["due"] - datetime.datetime.now(datetime.timezone.utc)).total_seconds() + 8
    if wait > 0:
        run.log(f"(waiting {wait:.0f}s until a minute passed while access is removed)")
        time.sleep(wait)
    during = deliveries_for(run, suspended["schedule"])
    run.check("a minute passed while access was removed and the reminder did not fire", during == [], during)
    signed_in_at = time.time()
    status, me = run.remind_api("GET", "/auth/me", "Bearer " + people.silicon_token("s5", fresh=True))
    run.check("the Silicon signs in to Remind again", status == 200, (status, me))

    def fired():
        after = [(at, payload) for at, payload in deliveries_for(run, suspended["schedule"]) if at >= signed_in_at]
        return bool(after), after
    ok, after = eventually(fired, 20, 1.0)
    overdue = ok and datetime.datetime.fromisoformat(after[0][1]["scheduled_for"].replace("Z", "+00:00")) \
        < datetime.datetime.fromtimestamp(signed_in_at, datetime.timezone.utc)
    run.check("the overdue occurrence is delivered right after the new sign-in (reminders resume)", overdue, after)
    status, _ = run.remind_api("DELETE", f"/schedules/{suspended['schedule']}", "Bearer " + people.silicon_token("s5"))
    run.log(f"(archived the every-minute reminder: {status})")


def issue_proof(run, issuer, subject_token, receiving_app="remind", scopes=("remind.schedules.read",)):
    status, proof = run.accounts_app("POST", "/v1/proofs/user-verification",
                                     {"subject_token": subject_token, "receiving_app": receiving_app,
                                      "scopes": list(scopes), "access_ttl_seconds": 600}, app=issuer)
    return status, proof


def scenario_6(run, people, state):
    """Remind receives User verification proofs (scope remind.schedules.read) from the apps it allows."""
    run.section("6. Proofs: Remind as a receiving app")
    c1 = people.carbon("c1")
    ensure_s1(run, people, state)
    ensure_reminder(run, people, state)
    at_interface = people.app_signin("c1", "interface", "http://127.0.0.1:9593/interface/callback")
    status, proof = issue_proof(run, "interface", at_interface)
    run.check("interface gets a User verification proof for the Carbon, for Remind", status == 201, (status, proof))
    if status != 201:
        return
    auth = "Proof " + proof["proof_token"]
    status, body = run.remind_api("GET", "/auth/me", auth)
    run.check("Remind accepts it and acts as the Carbon (/auth/me names the issuing app)",
              status == 200 and body.get("uuid") == c1["uuid"] and body.get("credential") == "proof"
              and body.get("issuing_app") == "interface", (status, body))
    if state.get("r3"):
        status, body = run.remind_api("GET", "/schedules", auth)
        run.check("it reads what the Carbon may read (its Silicon's reminders)",
                  status == 200 and state["r3"] in [i["id"] for i in body.get("items", [])], (status, body))
        status, body = run.remind_api("GET", f"/schedules/{state['r3']}/executions", auth)
        run.check("including a reminder's deliveries", status == 200, (status, body))
    status, body = run.remind_api("GET", "/silicons", auth)
    run.check("and the Silicons it looks after", status == 200 and len(body.get("items", [])) >= 1, (status, body))
    status, body = run.remind_api("POST", "/schedules", auth,
                                  {"text": "x", "kind": "recurring", "cron": "0 9 * * *", "timezone": "UTC"})
    run.check("a proof never writes (401 proof_not_accepted)", status == 401 and code(body) == "proof_not_accepted",
              (status, body))
    status, body = run.remind_api("GET", "/viewers", auth)
    run.check("and is not accepted on other reads either", status == 401 and code(body) == "proof_not_accepted",
              (status, body))
    status, body = run.remind_api("GET", "/schedules", "Bearer " + proof["proof_token"])
    run.check("a proof sent as Bearer is named as such (401 proof_as_bearer)",
              status == 401 and code(body) == "proof_as_bearer", (status, body))

    status, narrow = issue_proof(run, "interface", at_interface, scopes=("remind.other",))
    if status == 201:
        status, body = run.remind_api("GET", "/schedules", "Proof " + narrow["proof_token"])
        run.check("a proof without remind.schedules.read is refused (403 proof_scope_missing)",
                  status == 403 and code(body) == "proof_scope_missing", (status, body))
    status, elsewhere = issue_proof(run, "interface", at_interface, receiving_app="briefcase")
    if status == 201:
        status, body = run.remind_api("GET", "/schedules", "Proof " + elsewhere["proof_token"])
        run.check("a proof issued for another receiving app is refused (401 proof_invalid)",
                  status == 401 and code(body) == "proof_invalid", (status, body))
    status, unused = issue_proof(run, "interface", at_interface)
    if status == 201:
        st, _ = run.accounts_app("POST", "/v1/proofs/revoke", {"proof_id": unused["proof_id"]}, app="interface")
        status, body = run.remind_api("GET", "/schedules", "Proof " + unused["proof_token"])
        run.check("a revoked proof is refused (401 proof_invalid)",
                  st == 204 and status == 401 and code(body) == "proof_invalid", (st, status, body))
    st, _ = run.accounts_app("POST", "/v1/proofs/revoke", {"proof_id": proof["proof_id"]}, app="interface")
    revoked_at = time.time()

    def refused():
        status, body = run.remind_api("GET", "/schedules", auth)
        return status == 401 and code(body) == "proof_invalid", (status, body)
    ok, detail = eventually(refused, 40, 2.0)
    run.check(f"a proof revoked after use stops working within Remind's 30-second verification cache "
              f"({time.time() - revoked_at:.0f}s)", st == 204 and ok, detail)

    # Another Carbon, so the first one stays inside the stack's 10 email codes per address in 10 minutes.
    people.carbon("c2")
    at_webkit = people.app_signin("c2", "webkit", "http://127.0.0.1:4260/auth/callback")
    status, foreign = issue_proof(run, "webkit", at_webkit)
    if status == 201:
        status, body = run.remind_api("GET", "/schedules", "Proof " + foreign["proof_token"])
        run.check("a proof from an app REMIND_PROOF_ISSUERS does not list is refused (403 proof_issuer_not_allowed)",
                  status == 403 and code(body) == "proof_issuer_not_allowed", (status, body))
    else:
        run.check("webkit can issue a proof for the issuer check", False, (status, foreign))


def scenario_7(run, people, state):
    """The archive Silicon Apps installs answers the discovery commands in an empty home."""
    run.section("7. Discovery commands from the packaged archive")
    version = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "crates/cli/Cargo.toml").read_text(), re.M).group(1)
    machine = os.uname().machine
    target = {"Darwin": "macos", "Linux": "linux"}[os.uname().sysname] + "-" + {"arm64": "aarch64"}.get(machine, machine)
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    build = subprocess.run(["cargo", "build", "--locked", "--release", "-p", "silicon-remind-cli"], cwd=ROOT,
                           capture_output=True, text=True, check=False, timeout=1800)
    run.log("$ cargo build --locked --release -p silicon-remind-cli  -> exit " + str(build.returncode))
    binary = target_dir / "release" / "remind"
    run.check("the release CLI builds", build.returncode == 0 and binary.exists(), build.stderr[-800:])
    if build.returncode != 0:
        return
    packer = os.environ.get("SILICON_APPS") or shutil.which("silicon-apps") or str(Path.home() / ".apps/bin/silicon-apps")
    dist = run.out / "dist"
    packed = subprocess.run([str(ROOT / "scripts/package-apps.sh"), version, target, str(binary), "--output-dir",
                             str(dist), "--silicon-apps", packer], cwd=ROOT, capture_output=True, text=True,
                            check=False, timeout=600)
    run.log(f"$ scripts/package-apps.sh {version} {target} {binary} --output-dir {dist}  -> exit {packed.returncode}")
    run.log("  " + (packed.stdout + packed.stderr).strip()[-1500:].replace("\n", "\n  "))
    archive = dist / f"remind-{version}-{target}.tar.gz"
    run.check(f"scripts/package-apps.sh packs remind-{version}-{target}.tar.gz", packed.returncode == 0
              and archive.exists(), (packed.stdout + packed.stderr)[-800:])
    if not archive.exists():
        return
    unpacked = Path(tempfile.mkdtemp(prefix="archive.", dir=run.out))
    with tarfile.open(archive) as tar:
        names = sorted(tar.getnames())
        tar.extractall(unpacked, filter="data")
    files = {name.removeprefix("./") for name in names} - {"", ".", "bin"}
    run.check("the archive holds exactly apps.yaml and bin/remind", files == {"apps.yaml", "bin/remind"}, names)
    empty = Path(tempfile.mkdtemp(prefix="empty-home.", dir=run.out))
    env = {"PATH": "/usr/bin:/bin", "HOME": str(empty), "SILICON_HOME": str(empty)}

    def discover(*args):
        result = subprocess.run([str(unpacked / "bin/remind"), *args], capture_output=True, text=True, env=env,
                                timeout=30, check=False)
        run.log(f"$ bin/remind {' '.join(args)}   (empty HOME and SILICON_HOME)\n  exit={result.returncode} "
                f"{result.stdout.strip()[:300]}")
        return result
    help_ = discover("--help")
    run.check("remind --help: exit 0, non-empty", help_.returncode == 0 and len(help_.stdout) > 200, help_.returncode)
    accounts_json = discover("accounts", "--json")
    run.check('remind accounts --json: exit 0 with "app_id":"remind"',
              accounts_json.returncode == 0 and json.loads(accounts_json.stdout).get("app_id") == "remind"
              and '"app_id":"remind"' in accounts_json.stdout.replace(" ", ""), accounts_json.stdout)
    status = discover("login", "status", "--json")
    run.check('remind login status --json: exit 0, {"authenticated":false}',
              status.returncode == 0 and json.loads(status.stdout) == {"authenticated": False}, status.stdout)
    left = [str(p.relative_to(empty)) for p in empty.rglob("*")]
    run.check("nothing was written to the empty home", left == [], left)


def scenario_8(run, people, state):
    """Restarting the API and the worker keeps sign-ins (stateless tokens) and webhook deduplication."""
    run.section("8. Restart safety")
    before = {name: dev_accounts.running(run.cfg, name) for name in ("remind-api", "remind-worker")}
    result = subprocess.run([sys.executable, "-I", str(ROOT / "scripts/dev_accounts.py"), "restart"],
                            capture_output=True, text=True, check=False, timeout=120)
    after = {name: dev_accounts.running(run.cfg, name) for name in ("remind-api", "remind-worker")}
    run.log("$ python3 scripts/dev_accounts.py restart  -> exit " + str(result.returncode))
    run.check("the API and the worker restart (new processes, same data)",
              result.returncode == 0 and all(after.values()) and all(before[n] != after[n] for n in after),
              (before, after, result.stderr[-400:]))
    if state.get("s1_home"):
        exit_code, listed, _, _ = run.remind_cli(state["s1_home"], "list", "--json")
        run.check("the Silicon's CLI sign-in keeps working without signing in again", exit_code == 0, (exit_code, listed))
    if "c1" in people.carbons:
        status, body = run.remind_api("GET", "/auth/me", people.bearer("c1"))
        run.check("the Carbon's access token keeps working", status == 200, (status, body))
    if state.get("c1_home"):
        exit_code, status, _, _ = run.remind_cli(state["c1_home"], "login", "status", "--json")
        run.check("the Carbon's CLI sign-in too", exit_code == 0 and (status or {}).get("verified") is True,
                  (exit_code, status))
    secret = run.cfg.webhook_secret()
    if state.get("hand_event"):
        event_id, body = state["hand_event"]
        status, answer = post_delivery(run, body, secret=secret, event_id=event_id, event_type="ping")
        run.check("an event delivered before the restart is still a duplicate after it",
                  status == 200 and (answer or {}).get("status") == "duplicate", (status, answer))
    else:
        event_id, body = delivery("ping", {})
        first = post_delivery(run, body, secret=secret, event_id=event_id, event_type="ping")
        subprocess.run([sys.executable, "-I", str(ROOT / "scripts/dev_accounts.py"), "restart"], capture_output=True,
                       check=False, timeout=120)
        second = post_delivery(run, body, secret=secret, event_id=event_id, event_type="ping")
        run.check("an event delivered before a restart is a duplicate after it",
                  first[0] == 200 and (second[1] or {}).get("status") == "duplicate", (first, second))
    if state.get("replayed_delivery"):
        item = state["replayed_delivery"]
        sent, answered, detail = replay(run, item)
        run.check("a Silicon Accounts replay after the restart is again answered 2xx as a duplicate (one receipt)",
                  sent and answered and receipts(run, item["event_id"]) == 1, (detail, receipts(run, item["event_id"])))


SCENARIOS = {1: scenario_1, 2: scenario_2, 3: scenario_3, 4: scenario_4, 5: scenario_5, 6: scenario_6,
             7: scenario_7, 8: scenario_8}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", help="comma-separated scenario numbers (default: all, in order)")
    parser.add_argument("--keep-running", action="store_true", help="leave the development stack running")
    args = parser.parse_args(argv)
    if not os.environ.get("REMIND_TEST_STACK"):
        print("e2e-accounts: set REMIND_TEST_STACK to the local stack's JSON file (see --help).", file=sys.stderr)
        return 2
    selected = [int(n) for n in args.only.split(",")] if args.only else sorted(SCENARIOS)
    suffix = os.environ.get("REMIND_E2E_RUN") or time.strftime("%m%d%H%M") + uuid.uuid4().hex[:3]
    try:
        cfg = dev_accounts.Config()
        stack = json.loads(Path(os.environ["REMIND_TEST_STACK"]).read_text())
        out = ROOT / ".mig" / "e2e" / suffix
        out.mkdir(parents=True, exist_ok=True)
        run = Run(cfg, stack, out)
        was_running = all(dev_accounts.running(cfg, name) for name in dev_accounts.SERVICES)
        up = subprocess.run([sys.executable, "-I", str(ROOT / "scripts/dev_accounts.py"), "up"], capture_output=True,
                            text=True, check=False, timeout=600)
        run.log("$ scripts/dev-accounts.sh\n  " + (up.stdout + up.stderr).strip()[-1200:].replace("\n", "\n  "))
        if up.returncode != 0:
            raise dev_accounts.Failure("the development stack did not start:\n" + up.stderr[-1500:])
    except dev_accounts.Failure as error:
        print(f"e2e-accounts: {error}", file=sys.stderr)
        return 1
    print(f"Remind e2e run {suffix}: scenarios {selected}; transcript {out / 'transcript.txt'}")
    people, state = People(run, suffix), {}
    started = time.time()
    try:
        for number in selected:
            try:
                SCENARIOS[number](run, people, state)
            except Exception as error:  # noqa: BLE001  (report it, keep going, still clean up)
                run.check(f"scenario {number} ran to the end", False, f"{type(error).__name__}: {error}")
        check_one_time_delivery(run, people, state)
        suspension_check(run, people, state)
    finally:
        summary = {"run": suffix, "scenarios": selected, "passed": len(run.passed), "failed": run.failed,
                   "seconds": round(time.time() - started),
                   "accounts": {role: c["id"] for role, c in people.carbons.items()}
                   | {role: s["id"] for role, s in people.silicons.items()}}
        (out / "results.json").write_text(json.dumps(summary, indent=1))
        if not was_running and not args.keep_running:
            down = subprocess.run([sys.executable, "-I", str(ROOT / "scripts/dev_accounts.py"), "down"],
                                  capture_output=True, text=True, check=False, timeout=120)
            run.log("$ scripts/dev-accounts-stop.sh\n  " + (down.stdout + down.stderr).strip().replace("\n", "\n  "))
    print(f"\n{len(run.passed)} passed, {len(run.failed)} failed in {summary['seconds']}s"
          + ("" if not run.failed else ":\n  - " + "\n  - ".join(run.failed)))
    return 1 if run.failed else 0


if __name__ == "__main__":
    sys.exit(main())
