#!/usr/bin/env python3
"""Run Remind on this machine against a local Silicon Accounts stack.

    scripts/dev-accounts.sh [--build]           # dev_accounts.py up: start everything (idempotent)
    scripts/dev-accounts-stop.sh [--drop]       # dev_accounts.py down: stop it, give the webhook URL back
    python3 scripts/dev_accounts.py restart     # restart the API and the worker (same data, same secrets)
    python3 scripts/dev_accounts.py status      # what runs, as JSON (never a secret)

`up` starts, when not already running:
  - databases <db> and <db>_testing on the local PostgreSQL (created when missing), migrated by remind-migrate;
  - a delivery receiver on 127.0.0.1:<base+3> that answers 200 and records every reminder delivery it gets
    (one JSON line per request in <dir>/deliveries.jsonl), so local Silicons can subscribe to
    http://127.0.0.1:<base+3>/hook;
  - remind-api on 127.0.0.1:<base+1> and remind-worker (operational listener on 127.0.0.1:<base+2>);
and points Remind's app webhook at Silicon Accounts to http://127.0.0.1:<base+1>/webhook/ with Remind's own app
credentials (PUT /v1/apps/remind/webhook, every update). The URL that was there before is kept in
<dir>/webhook-previous.json and put back by `down`. The webhook's signing secret reaches the service only through
its environment: the one the stack file seeded, or one Silicon Accounts generated (kept in <dir>/webhook-secret,
mode 0600, never in git). After a (re)start, a test ping proves that deliveries arrive and verify; a secret that no
longer matches is replaced (POST /v1/apps/remind/webhook/generate-secret) and the API restarted.

Configuration (environment):
  REMIND_TEST_STACK    JSON file describing the stack (the Silicon Accounts testkit's shape): accounts_public_url,
                       accounts_api_url, apps.remind.app_secret and apps.remind.webhook_secret_seeded
  ACCOUNTS_URL         Silicon Accounts public URL, the token issuer (default: stack file, else http://localhost:9590)
  ACCOUNTS_API_URL     where Remind calls Silicon Accounts (default: stack file, else ACCOUNTS_URL)
  REMIND_APP_SECRET    remind's app secret at that stack (default: stack file)
  REMIND_DEV_BASE      port block base (default 4180: web 4180, API 4181, worker 4182, delivery receiver 4183)
  REMIND_DEV_PG        PostgreSQL server URL without a database (default postgres://postgres@127.0.0.1:5460)
  REMIND_DEV_DB        database name (default remind_e2e; the testing database is <name>_testing)
  REMIND_DEV_DIR       state directory (default .mig/dev-accounts): logs, keyring, webhook secret, deliveries
  REMIND_DEV_PIDS      pid directory (default .mig/pids)
  REMIND_BIN_DIR       where remind-api, remind-worker and remind-migrate are (default $CARGO_TARGET_DIR/debug,
                       else target/debug)
  REMIND_DEV_PROOF_ISSUERS  REMIND_PROOF_ISSUERS for the service (default remind.schedules.read=interface)
  PSQL                 psql executable (default: PATH, then Homebrew's postgresql@16/@17)

Only loopback Silicon Accounts URLs are accepted: this script never talks to a deployed Silicon Accounts. Nothing it
prints contains a secret.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlparse
from urllib.request import ProxyHandler, Request, build_opener
import uuid

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "remind"
WEBHOOK_PATH = "/webhook/"
LOOPBACK = {"localhost", "127.0.0.1", "::1"}
OPENER = build_opener(ProxyHandler({}))
SERVICES = ("remind-receiver", "remind-api", "remind-worker")


class Failure(Exception):
    """A precise, user-facing reason the command cannot continue."""


def http(method, url, body=None, basic=None, bearer=None, headers=None, timeout=15):
    """One JSON request; returns (status, parsed body or None). Network errors raise Failure."""
    options = {"Accept": "application/json", **(headers or {})}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        options["Content-Type"] = "application/json"
    if basic:
        options["Authorization"] = "Basic " + base64.b64encode(":".join(basic).encode()).decode()
    if bearer:
        options["Authorization"] = "Bearer " + bearer
    try:
        with OPENER.open(Request(url, data=data, method=method, headers=options), timeout=timeout) as response:
            status, raw = response.status, response.read()
    except HTTPError as error:
        with error:
            status, raw = error.code, error.read()
    except (URLError, OSError) as error:
        raise Failure(f"{method} {url} failed: {getattr(error, 'reason', error)}") from None
    try:
        return status, json.loads(raw) if raw else None
    except ValueError:
        return status, {"raw": raw[:300].decode(errors="replace")}


def error_code(body):
    return body.get("error", {}).get("code") if isinstance(body, dict) else None


def port_open(port, host="127.0.0.1"):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.3)
        return sock.connect_ex((host, port)) == 0


def write_private(path, text):
    """Writes a file only its owner can read (secrets, keyrings)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(text)
    os.chmod(path, 0o600)


class Config:
    """Everything `up`, `down`, `restart` and `status` need, read once from the environment."""

    def __init__(self, environ=os.environ):
        stack = {}
        stack_path = environ.get("REMIND_TEST_STACK")
        if stack_path:
            try:
                stack = json.loads(Path(stack_path).read_text())
            except (OSError, ValueError) as error:
                raise Failure(f"REMIND_TEST_STACK={stack_path} cannot be read as JSON: {error}") from None
        app = (stack.get("apps") or {}).get(APP_ID) or {}
        self.accounts_url = (environ.get("ACCOUNTS_URL") or stack.get("accounts_public_url")
                             or "http://localhost:9590").rstrip("/")
        self.accounts_api_url = (environ.get("ACCOUNTS_API_URL") or stack.get("accounts_api_url")
                                 or self.accounts_url).rstrip("/")
        for name, url in (("ACCOUNTS_URL", self.accounts_url), ("ACCOUNTS_API_URL", self.accounts_api_url)):
            if urlparse(url).hostname not in LOOPBACK:
                raise Failure(f"{name}={url} is not on this machine; this script only runs against a local stack.")
        self.app_secret = environ.get("REMIND_APP_SECRET") or app.get("app_secret") or ""
        if not self.app_secret:
            raise Failure("Remind's app secret is unknown: set REMIND_TEST_STACK to the stack file, or REMIND_APP_SECRET.")
        self.seeded_webhook_secret = app.get("webhook_secret_seeded") or ""
        self.base = int(environ.get("REMIND_DEV_BASE", "4180"))
        self.api_port, self.worker_port, self.receiver_port = self.base + 1, self.base + 2, self.base + 3
        self.api_url = f"http://127.0.0.1:{self.api_port}"
        self.webhook_url = f"{self.api_url}{WEBHOOK_PATH}"
        self.receiver_url = f"http://127.0.0.1:{self.receiver_port}/hook"
        self.pg = environ.get("REMIND_DEV_PG", "postgres://postgres@127.0.0.1:5460").rstrip("/")
        self.db = environ.get("REMIND_DEV_DB", "remind_e2e")
        self.test_db = f"{self.db}_testing"
        self.dir = Path(environ.get("REMIND_DEV_DIR", ROOT / ".mig" / "dev-accounts")).resolve()
        self.pids = Path(environ.get("REMIND_DEV_PIDS", ROOT / ".mig" / "pids")).resolve()
        target = environ.get("CARGO_TARGET_DIR")
        default_bin = (Path(target) if target else ROOT / "target") / "debug"
        self.bin_dir = Path(environ.get("REMIND_BIN_DIR", default_bin)).resolve()
        self.proof_issuers = environ.get("REMIND_DEV_PROOF_ISSUERS", "remind.schedules.read=interface")
        self.psql = environ.get("PSQL") or shutil.which("psql") or next(
            (p for p in ("/opt/homebrew/opt/postgresql@16/bin/psql", "/opt/homebrew/opt/postgresql@17/bin/psql",
                         "/usr/local/opt/postgresql@16/bin/psql") if Path(p).exists()), "psql")
        self.log_filter = environ.get("REMIND_LOG_FILTER", "silicon_remind=info,tower_http=warn")

    @property
    def basic(self):
        return (APP_ID, self.app_secret)

    def binary(self, name):
        path = self.bin_dir / name
        if not path.exists():
            raise Failure(f"{path} does not exist: build it (scripts/dev-accounts.sh --build) or set REMIND_BIN_DIR.")
        return str(path)

    def webhook_secret(self):
        """The secret Silicon Accounts signs Remind's deliveries with, as far as this machine knows."""
        stored = self.dir / "webhook-secret"
        if stored.exists():
            return stored.read_text().strip()
        return self.seeded_webhook_secret

    def keyring(self):
        """A development encryption keyring, made once and kept (subscriptions are encrypted with it)."""
        path = self.dir / "encryption-keyring.json"
        if not path.exists():
            key = base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
            write_private(path, json.dumps({"1": key}))
        return path.read_text().strip()

    def service_env(self):
        """The whole environment of remind-api, remind-worker and remind-migrate: nothing else leaks in."""
        db, test_db = f"{self.pg}/{self.db}", f"{self.pg}/{self.test_db}"
        env = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR", "LANG") if key in os.environ}
        env.update({
            "REMIND_ENVIRONMENT": "development",
            "REMIND_BIND_ADDR": f"127.0.0.1:{self.api_port}",
            "REMIND_PUBLIC_BASE_URL": self.api_url,
            "REMIND_LOG_FILTER": self.log_filter,
            "NO_COLOR": "1",  # plain log files, easy to read and search
            "REMIND_DATABASE_URL": db,
            "REMIND_MIGRATOR_DATABASE_URL": db,
            "REMIND_TEST_DATABASE_URL": test_db,
            "REMIND_TEST_MIGRATOR_DATABASE_URL": test_db,
            "ACCOUNTS_URL": self.accounts_url,
            "ACCOUNTS_API_URL": self.accounts_api_url,
            "REMIND_APP_ID": APP_ID,
            "REMIND_APP_SECRET": self.app_secret,
            "REMIND_ACCOUNTS_WEBHOOK_SECRET": self.webhook_secret(),
            "REMIND_PROOF_ISSUERS": self.proof_issuers,
            "REMIND_ENCRYPTION_CURRENT_VERSION": "1",
            "REMIND_ENCRYPTION_KEYRING": self.keyring(),
            "REMIND_WORKER_OPERATIONAL_BIND_ADDR": f"127.0.0.1:{self.worker_port}",
            "REMIND_WORKER_POLL_INTERVAL_MS": "500",
            "REMIND_TEST_WEBHOOK_URLS": self.receiver_url,
            # Never mail or record telemetry from a development service, even with a stray .env around.
            "REMIND_TELEMETRY_ENABLED": "false",
            "REMIND_TELEMETRY_TABLE_KEY": "",
            "REMIND_TELEMETRY_HOME": str(self.dir / "telemetry"),
            "REMIND_POSTMARK_SERVER_TOKEN": "",
        })
        return env


# --- processes ---------------------------------------------------------------------------------------------------

def read_pid(cfg, name):
    try:
        return int((cfg.pids / name).read_text().strip())
    except (OSError, ValueError):
        return None


def command_of(pid):
    result = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True, check=False)
    return result.stdout.strip()


def running(cfg, name):
    """The pid of `name` when the recorded process is alive and still that program (pids get reused)."""
    pid = read_pid(cfg, name)
    if not pid:
        return None
    marker = "dev_accounts.py receiver" if name == "remind-receiver" else name
    return pid if marker in command_of(pid) else None


def start(cfg, name, argv, env, port):
    """Starts one detached process (its own session, output to <dir>/logs/<name>.log) and records its pid."""
    if port_open(port):
        raise Failure(f"127.0.0.1:{port} is taken by another process; {name} cannot listen there "
                      f"(lsof -nP -iTCP:{port} -sTCP:LISTEN shows which).")
    logs = cfg.dir / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    run_dir = cfg.dir / "run"
    run_dir.mkdir(parents=True, exist_ok=True)
    with open(logs / f"{name}.log", "ab") as log, open(os.devnull, "rb") as devnull:
        process = subprocess.Popen(argv, stdin=devnull, stdout=log, stderr=subprocess.STDOUT, env=env,
                                   cwd=run_dir, start_new_session=True)
    cfg.pids.mkdir(parents=True, exist_ok=True)
    (cfg.pids / name).write_text(f"{process.pid}\n")
    return process.pid


def stop(cfg, name, grace=10.0):
    """Stops `name` (TERM, then KILL after `grace` seconds) and forgets its pid. Returns what it did."""
    pid = running(cfg, name)
    (cfg.pids / name).unlink(missing_ok=True)
    if not pid:
        return "not running"
    try:
        os.kill(pid, signal.SIGTERM)
        deadline = time.time() + grace
        while time.time() < deadline:
            if not command_of(pid):
                return f"stopped (pid {pid})"
            time.sleep(0.2)
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        return f"stopped (pid {pid})"
    return f"killed after {grace:.0f}s (pid {pid})"


def wait_until(probe, seconds, what, log=None, alive=None):
    """Waits for probe(); fails at once, with the log's last lines, when `alive` says the process exited."""
    deadline = time.time() + seconds
    reason = f"did not come up within {seconds}s"
    while time.time() < deadline:
        if probe():
            return
        if alive and not alive():
            reason = "exited while starting"
            break
        time.sleep(0.3)
    tail = ""
    if log and log.exists():
        tail = "\n  last lines of " + str(log) + ":\n    " + "\n    ".join(log.read_text(errors="replace").splitlines()[-15:])
    raise Failure(f"{what} {reason}.{tail}")


def ready(url):
    try:
        status, _ = http("GET", url, timeout=2)
    except Failure:
        return False
    return status == 200


# --- databases -----------------------------------------------------------------------------------------------------

def psql(cfg, sql, database="postgres"):
    try:
        result = subprocess.run([cfg.psql, f"{cfg.pg}/{database}", "-v", "ON_ERROR_STOP=1", "-At", "-c", sql],
                                capture_output=True, text=True, check=False, timeout=60)
    except FileNotFoundError:
        raise Failure(f"psql was not found at {cfg.psql}; install PostgreSQL's client or set PSQL.") from None
    if result.returncode != 0:
        raise Failure(f"psql on {cfg.pg}/{database} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def ensure_databases(cfg):
    created = []
    for name in (cfg.db, cfg.test_db):
        if not name.replace("_", "").isalnum() or not name.startswith(APP_ID):
            raise Failure(f"database name {name!r} must start with '{APP_ID}' and use letters, digits and _ only.")
        if psql(cfg, f"SELECT 1 FROM pg_database WHERE datname = '{name}'") != "1":
            psql(cfg, f'CREATE DATABASE "{name}"')
            created.append(name)
    return created


def drop_databases(cfg):
    dropped = []
    for name in (cfg.db, cfg.test_db):
        if psql(cfg, f"SELECT 1 FROM pg_database WHERE datname = '{name}'") == "1":
            psql(cfg, f'DROP DATABASE "{name}" WITH (FORCE)')
            dropped.append(name)
    return dropped


def migrate(cfg):
    result = subprocess.run([cfg.binary("remind-migrate")], env=cfg.service_env(), cwd=cfg.dir,
                            capture_output=True, text=True, check=False, timeout=300)
    if result.returncode != 0:
        raise Failure("remind-migrate failed:\n" + (result.stderr or result.stdout)[-2000:])


# --- the delivery receiver -------------------------------------------------------------------------------------------

def serve_receiver(port, out):
    """Answers every POST with 200 and appends {received_at, path, headers, body} to `out`; GET / says how many."""
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    import threading

    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):  # noqa: N802 (http.server naming)
            length = int(self.headers.get("content-length") or 0)
            body = self.rfile.read(length).decode("utf-8", "replace")
            line = json.dumps({"received_at": time.time(), "path": self.path,
                               "headers": {k.lower(): v for k, v in self.headers.items()}, "body": body})
            with lock, open(out, "a") as handle:
                handle.write(line + "\n")
            self._answer(200, {"ok": True})

        def do_GET(self):  # noqa: N802
            try:
                count = sum(1 for _ in open(out))
            except OSError:
                count = 0
            self._answer(200, {"deliveries": count})

        def _answer(self, status, payload):
            raw = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def log_message(self, *args):
            pass

    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


# --- Remind's app webhook at Silicon Accounts ------------------------------------------------------------------------

def accounts(cfg, method, path, body=None):
    headers = {"Idempotency-Key": str(uuid.uuid4())} if method in ("POST", "PUT") else None
    return http(method, cfg.accounts_api_url + path, body=body, basic=cfg.basic, headers=headers)


def point_webhook(cfg):
    """Points Remind's webhook at the local API (every update), remembering the URL that was there."""
    status, current = accounts(cfg, "GET", f"/v1/apps/{APP_ID}/webhook")
    if status != 200:
        raise Failure(f"GET /v1/apps/{APP_ID}/webhook answered {status} {error_code(current)}: is the app secret right?")
    previous = cfg.dir / "webhook-previous.json"
    if current.get("url") != cfg.webhook_url and not previous.exists():
        previous.parent.mkdir(parents=True, exist_ok=True)
        previous.write_text(json.dumps({"url": current.get("url"), "events": current.get("events")}))
    if current.get("url") == cfg.webhook_url and current.get("events") is None and current.get("secret_set"):
        return "already pointed here"
    status, answer = accounts(cfg, "PUT", f"/v1/apps/{APP_ID}/webhook", {"url": cfg.webhook_url, "events": None})
    if status != 200:
        raise Failure(f"PUT /v1/apps/{APP_ID}/webhook answered {status} {error_code(answer)}: {answer}")
    if answer.get("secret"):
        write_private(cfg.dir / "webhook-secret", answer["secret"])
        return "pointed here with a new secret"
    return "pointed here (secret kept)"


def new_webhook_secret(cfg):
    status, answer = accounts(cfg, "POST", f"/v1/apps/{APP_ID}/webhook/generate-secret", {})
    if status != 200 or not (answer or {}).get("secret"):
        raise Failure(f"generate-secret answered {status} {error_code(answer)}")
    write_private(cfg.dir / "webhook-secret", answer["secret"])


def ping_reaches_service(cfg, seconds=30):
    """Queues a test ping and returns (delivered?, detail) from the delivery's own record at Silicon Accounts."""
    status, queued = accounts(cfg, "POST", f"/v1/apps/{APP_ID}/webhook/test", {})
    if status != 202:
        raise Failure(f"webhook test answered {status} {error_code(queued)}: {queued}")
    delivery = queued["delivery_id"]
    deadline = time.time() + seconds
    while time.time() < deadline:
        status, record = accounts(cfg, "GET", f"/v1/apps/{APP_ID}/webhook/deliveries/{delivery}")
        if status == 200 and record.get("status") == "delivered":
            return True, f"ping {queued['event_id']} delivered"
        if status == 200 and record.get("last_status") is not None and record.get("status") != "delivered":
            return False, f"ping answered HTTP {record.get('last_status')}: {(record.get('last_error') or '')[:200]}"
        time.sleep(0.5)
    return False, f"ping {queued['event_id']} not delivered within {seconds}s"


def give_webhook_back(cfg):
    previous = cfg.dir / "webhook-previous.json"
    if not previous.exists():
        return "nothing to give back"
    saved = json.loads(previous.read_text())
    status, current = accounts(cfg, "GET", f"/v1/apps/{APP_ID}/webhook")
    if status == 200 and current.get("url") != cfg.webhook_url:
        previous.unlink()
        return f"left as is: it points at {current.get('url')} now, not here"
    if not saved.get("url"):
        previous.unlink()
        return "left pointing here: there was no URL before"
    status, answer = accounts(cfg, "PUT", f"/v1/apps/{APP_ID}/webhook", {"url": saved["url"], "events": saved.get("events")})
    if status != 200:
        raise Failure(f"could not put the webhook URL back ({status} {error_code(answer)}); {previous} keeps it")
    previous.unlink()
    return f"back to {saved['url']}"


# --- commands --------------------------------------------------------------------------------------------------------

def start_receiver(cfg):
    if running(cfg, "remind-receiver"):
        return "already running"
    out = cfg.dir / "deliveries.jsonl"
    start(cfg, "remind-receiver", [sys.executable, "-I", str(Path(__file__).resolve()), "receiver",
                                   "--port", str(cfg.receiver_port), "--out", str(out)],
          {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}, cfg.receiver_port)
    wait_until(lambda: port_open(cfg.receiver_port), 10, "the delivery receiver", cfg.dir / "logs/remind-receiver.log")
    return f"listening on {cfg.receiver_url}"


def start_service(cfg):
    """Starts remind-api and remind-worker when they are not running; returns whether the API was (re)started."""
    env = cfg.service_env()
    started = False
    if not running(cfg, "remind-api"):
        start(cfg, "remind-api", [cfg.binary("remind-api")], env, cfg.api_port)
        started = True
    if not running(cfg, "remind-worker"):
        start(cfg, "remind-worker", [cfg.binary("remind-worker")], env, cfg.worker_port)
    wait_until(lambda: ready(f"{cfg.api_url}/health/ready"), 30, "remind-api", cfg.dir / "logs/remind-api.log",
               lambda: running(cfg, "remind-api"))
    wait_until(lambda: ready(f"http://127.0.0.1:{cfg.worker_port}/health/ready"), 30, "remind-worker",
               cfg.dir / "logs/remind-worker.log", lambda: running(cfg, "remind-worker"))
    return started


def prove_webhook(cfg, report):
    delivered, detail = ping_reaches_service(cfg)
    if not delivered:
        report["webhook_ping"] = detail + "; making a new secret and restarting the API"
        new_webhook_secret(cfg)
        stop(cfg, "remind-api")
        start_service(cfg)
        delivered, detail = ping_reaches_service(cfg)
        if not delivered:
            raise Failure(f"Silicon Accounts' deliveries still fail after a new secret: {detail}")
    report["webhook_ping"] = detail


def build(cfg):
    command = ["cargo", "build", "--locked", "-p", "silicon-remind", "-p", "silicon-remind-cli", "--bins"]
    print("$ " + " ".join(command), file=sys.stderr)
    if subprocess.run(command, cwd=ROOT, check=False).returncode != 0:
        raise Failure("cargo build failed")


def cmd_up(cfg, args):
    if args.build:
        build(cfg)
    cfg.dir.mkdir(parents=True, exist_ok=True)
    report = {"databases_created": ensure_databases(cfg)}
    migrate(cfg)
    report["migrated"] = [cfg.db, cfg.test_db]
    report["webhook"] = point_webhook(cfg)
    report["receiver"] = start_receiver(cfg)
    api_started = start_service(cfg)
    if api_started or args.check_webhook:
        prove_webhook(cfg, report)
    report.update(status_of(cfg))
    print(json.dumps(report, indent=1))


def cmd_restart(cfg, _args):
    report = {"remind-api": stop(cfg, "remind-api"), "remind-worker": stop(cfg, "remind-worker")}
    start_service(cfg)
    report.update(status_of(cfg))
    print(json.dumps(report, indent=1))


def cmd_down(cfg, args):
    report = {name: stop(cfg, name) for name in reversed(SERVICES)}
    if not args.keep_webhook:
        report["webhook"] = give_webhook_back(cfg)
    if args.drop:
        report["databases_dropped"] = drop_databases(cfg)
    print(json.dumps(report, indent=1))


def status_of(cfg):
    services = {}
    for name, port in zip(SERVICES, (cfg.receiver_port, cfg.api_port, cfg.worker_port)):
        services[name] = {"pid": running(cfg, name), "port": port, "listening": port_open(port)}
    try:
        status, hook = accounts(cfg, "GET", f"/v1/apps/{APP_ID}/webhook")
        webhook = {"url": hook.get("url"), "events": hook.get("events"), "points_here": hook.get("url") == cfg.webhook_url} \
            if status == 200 else {"error": status}
    except Failure as error:
        webhook = {"error": str(error)}
    return {"api": cfg.api_url, "receiver_url": cfg.receiver_url, "accounts": cfg.accounts_url, "services": services,
            "webhook_at_accounts": webhook, "dir": str(cfg.dir)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    up = commands.add_parser("up", help="start everything that is not running (idempotent)")
    up.add_argument("--build", action="store_true", help="cargo build the service binaries first")
    up.add_argument("--check-webhook", action="store_true", help="send a test ping even when nothing was started")
    down = commands.add_parser("down", help="stop what `up` started and give the webhook URL back")
    down.add_argument("--keep-webhook", action="store_true", help="leave Remind's webhook pointing at this machine")
    down.add_argument("--drop", action="store_true", help="also drop the two development databases")
    commands.add_parser("restart", help="restart remind-api and remind-worker (same data and secrets)")
    commands.add_parser("status", help="what runs, as JSON")
    receiver = commands.add_parser("receiver", help=argparse.SUPPRESS)
    receiver.add_argument("--port", type=int, required=True)
    receiver.add_argument("--out", required=True)
    args = parser.parse_args(argv)
    if args.command == "receiver":
        serve_receiver(args.port, args.out)
        return 0
    try:
        cfg = Config()
        {"up": cmd_up, "down": cmd_down, "restart": cmd_restart,
         "status": lambda c, _a: print(json.dumps(status_of(c), indent=1))}[args.command](cfg, args)
    except Failure as error:
        print(f"dev-accounts: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
