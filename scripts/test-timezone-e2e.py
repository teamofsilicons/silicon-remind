#!/usr/bin/env python3
"""Exercise native Remind API/CLI with disposable PostgreSQL and loopback IAM.

Requires PostgreSQL binaries (PG_BIN may override the Homebrew location).
Use --serve to retain the fixture for browser checks; create the printed stop_file
or interrupt this process to stop every service and remove the temporary data.
IAM credentials and tokens below are invented fixtures, never real credentials.
"""

import argparse
import base64
from datetime import datetime, timezone as utc_timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import threading
import time
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlencode, urlsplit, urlunsplit
from urllib.request import Request, urlopen
import uuid
from zoneinfo import ZoneInfo


ROOT = Path(__file__).resolve().parents[1]
PG_BIN = Path(os.environ.get("PG_BIN", "/opt/homebrew/opt/postgresql@16/bin"))
ORG = "timezone-e2e"
APP = "timezone-e2e>remind"
APP_SECRET = "local-timezone-e2e-application-secret"
SLT = "oac_local_timezone_e2e_only"
ACCESS = "oat_local_timezone_e2e_only"
REFRESH = "ort_local_timezone_e2e_only"
PRINCIPAL = "00000000-0000-4000-8000-000000000001"
MEMBERSHIP = "00000000-0000-4000-8000-000000000002"
ORGANIZATION = "00000000-0000-4000-8000-000000000003"


def available_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class IamFixture(BaseHTTPRequestHandler):
    calls = []

    def log_message(self, *_):
        pass

    def respond(self, status, body):
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        parsed = urlsplit(self.path)
        if parsed.path != "/login":
            return self.respond(404, {"error": "unexpected fixture route"})
        target = urlsplit(parse_qs(parsed.query).get("redirect_uri", [""])[0])
        if (target.scheme not in {"http", "https"}
                or target.hostname not in {"127.0.0.1", "localhost", "::1"}
                or target.username or target.password):
            return self.respond(400, {"error": "loopback redirect_uri required"})
        query = parse_qs(target.query)
        query["slt"] = [SLT]
        location = urlunsplit(target._replace(query=urlencode(query, doseq=True)))
        self.send_response(302)
        self.send_header("Location", location)
        self.end_headers()

    def do_POST(self):
        self.calls.append(self.path)
        expected = "Basic " + base64.b64encode(f"{APP}:{APP_SECRET}".encode()).decode()
        if self.headers.get("Authorization") != expected:
            return self.respond(401, {"error": {"code": "unauthenticated"}})
        body = parse_qs(self.rfile.read(int(self.headers.get("Content-Length", 0))).decode())
        if self.path == "/api/v1/app-auth/tokens":
            if body.get("slt") != [SLT] and body.get("refresh_token") != [REFRESH]:
                return self.respond(401, {"error": {"code": "invalid_grant"}})
            return self.respond(200, {
                "access_token": ACCESS, "refresh_token": REFRESH,
                "token_type": "Bearer", "expires_in": 86400, "scope": "",
                "actor": {"type": "silicon", "principal_id": PRINCIPAL,
                          "public_id": f"clock:{ORG}"}, "org_id": ORG,
            })
        if self.path == "/api/v1/oauth/introspect":
            org = self.headers.get("X-Org-Id")
            if body.get("token") != [ACCESS] or org not in {None, ORG}:
                return self.respond(200, {"active": False})
            snapshot = {
                "principal_id": PRINCIPAL, "actor_type": "silicon",
                "public_id": f"clock:{ORG}", "organization_id": ORGANIZATION,
                "org_id": ORG, "membership_id": MEMBERSHIP,
                "membership_version": 1, "authorization_epoch": 1,
                "audience": APP, "testing_environment_id": None,
                "scopes": [], "org_role": "member", "tags": None,
            }
            return self.respond(200, {
                "active": True, "principal_id": PRINCIPAL, "client_id": APP,
                "expires_at": int(time.time()) + 86400,
                **({"authorization": snapshot, "org_id": ORG,
                    "membership_id": MEMBERSHIP} if org else {"authorizations": [snapshot]}),
            })
        self.respond(404, {"error": {"code": "unexpected_fixture_route"}})


class WebhookReceiver(BaseHTTPRequestHandler):
    events = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.events.append(payload)
        self.send_response(204)
        self.end_headers()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--worker", action="store_true", help="Also verify real next-minute webhook delivery")
    args = parser.parse_args()
    build_env = dict(os.environ, DEVELOPER_DIR="/Library/Developer/CommandLineTools")
    if not args.skip_build:
        subprocess.run(["cargo", "build", "--workspace", "--bin", "remind-api",
                        "--bin", "remind-migrate", "--bin", "remind", "--bin", "remind-worker"],
                       cwd=ROOT, env=build_env, check=True)
    clean_env = {key: value for key, value in os.environ.items()
                 if key in {"PATH", "HOME", "USER", "TMPDIR", "LANG"}}
    stopped = threading.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: stopped.set())
    with tempfile.TemporaryDirectory(prefix="remind-timezone-e2e-") as directory:
        scratch = Path(directory)
        database = scratch / "postgres"
        pg_port, api_port = available_port(), available_port()
        iam = ThreadingHTTPServer(("127.0.0.1", 0), IamFixture)
        threading.Thread(target=iam.serve_forever, daemon=True).start()
        origin = f"http://127.0.0.1:{api_port}"
        database_url = f"postgres://remind_e2e@127.0.0.1:{pg_port}/postgres"
        iam_origin = f"http://127.0.0.1:{iam.server_port}"
        encoded_key = base64.urlsafe_b64encode(b"e" * 32).decode().rstrip("=")
        env = dict(clean_env,
                   REMIND_ENVIRONMENT="development", REMIND_BIND_ADDR=f"127.0.0.1:{api_port}",
                   REMIND_PUBLIC_BASE_URL=origin, REMIND_DATABASE_URL=database_url,
                   REMIND_MIGRATOR_DATABASE_URL=database_url, REMIND_IAM_BASE_URL=iam_origin,
                   REMIND_IAM_APP_ID=APP, REMIND_IAM_APP_SECRET=APP_SECRET,
                   REMIND_IAM_WEBHOOK_KEYRING=json.dumps({"1": "whs_" + encoded_key}),
                   REMIND_INTERNAL_API_TOKEN="local-timezone-e2e-internal-token",
                   REMIND_ENCRYPTION_CURRENT_VERSION="1",
                   REMIND_ENCRYPTION_KEYRING=json.dumps({"1": encoded_key}),
                   REMIND_TELEMETRY_ENABLED="false", REMIND_LOG_FILTER="warn",
                   SILICON_HOME=str(scratch / "cli"))
        (scratch / "cli").mkdir()
        api = None
        worker = None
        receiver = None
        pg_started = False
        log = (scratch / "services.log").open("w+")

        def run(command, **kwargs):
            return subprocess.run([str(x) for x in command], cwd=scratch, env=env,
                                  check=True, stdout=log, stderr=log, **kwargs)

        def request(method, path, payload=None, key=None):
            headers = {"Authorization": f"Bearer {ACCESS}", "X-Org-Id": ORG,
                       "Content-Type": "application/json", "X-Remind-Telemetry": "off",
                       "Idempotency-Key": key or str(uuid.uuid4())}
            call = Request(origin + path, method=method, headers=headers,
                           data=None if payload is None else json.dumps(payload).encode())
            try:
                response = urlopen(call, timeout=10)
            except HTTPError as error:
                response = error
            with response:
                data = response.read()
                return response.status, json.loads(data) if data else None

        def counts():
            sql = "SELECT (SELECT count(*) FROM schedules),(SELECT count(*) FROM executions),(SELECT count(*) FROM idempotency_records),(SELECT count(*) FROM audit_records)"
            result = subprocess.run([str(PG_BIN / "psql"), database_url, "-Atc", sql],
                                    cwd=scratch, env=env, check=True, capture_output=True, text=True)
            return result.stdout.strip()

        def cli(*arguments, success=True):
            result = subprocess.run([str(ROOT / "target/debug/remind"), "--url", origin,
                                     "--org", ORG, "--no-update", "--json", *arguments],
                                    cwd=scratch, env=env, capture_output=True, text=True, timeout=20)
            if success:
                assert result.returncode == 0, result.stderr
                return json.loads(result.stdout)
            return result

        try:
            run([PG_BIN / "initdb", "-D", database, "-U", "remind_e2e", "--auth=trust", "--no-locale", "--encoding=UTF8"])
            run([PG_BIN / "pg_ctl", "-D", database, "-l", scratch / "postgres.log",
                 "-o", f"-h 127.0.0.1 -p {pg_port} -k {scratch}", "-w", "start"])
            pg_started = True
            run([ROOT / "target/debug/remind-migrate"])
            api = subprocess.Popen([str(ROOT / "target/debug/remind-api")], cwd=scratch,
                                   env=env, stdout=log, stderr=log)
            for _ in range(100):
                if api.poll() is not None:
                    raise RuntimeError("API exited before becoming ready")
                try:
                    if request("GET", "/health/ready")[0] == 200:
                        break
                except (URLError, TimeoutError):
                    time.sleep(0.1)
            else:
                raise RuntimeError("API readiness deadline exceeded")

            before_iam = len(IamFixture.calls)
            for kind in ("recurring", "one-time"):
                for timezone_args in ([], ["--timezone"], ["--timezone", ""], ["--timezone", "   "]):
                    result = cli("create", "--text", "missing timezone", "--cron", "0 9 * * *",
                                 "--kind", kind, *timezone_args, success=False)
                    assert result.returncode == 2, result.stderr
                    assert "mandatory" in result.stderr and "--timezone Asia/Kolkata" in result.stderr
            assert len(IamFixture.calls) == before_iam, "CLI invalid requests reached IAM"
            print("PASS: CLI rejects 8 missing/blank timezone cases before authentication", flush=True)
            login = cli("login", SLT)
            assert login, "CLI login returned no result"

            for kind in ("recurring", "one_time"):
                for marker in ("omitted", None, "", " \t\n"):
                    body = {"text": "missing timezone", "kind": kind, "cron": "0 9 * * *"}
                    if marker != "omitted":
                        body["timezone"] = marker
                    before = counts()
                    status, error = request("POST", "/api/v1/schedules", body)
                    assert status == 422, (status, error)
                    assert error["error"]["code"] == "timezone_required", error
                    assert all(word in error["error"]["message"] for word in ("mandatory", "IANA", "timezone", "Asia/Kolkata"))
                    assert counts() == before, "rejection wrote reminder state"
            print("PASS: API rejects 8 missing/null/blank cases without reminder or idempotency writes", flush=True)
            before = counts()
            status, error = request("POST", "/api/v1/schedules", {
                "text": "invalid IANA timezone", "kind": "recurring",
                "cron": "0 9 * * *", "timezone": "Not/A_Timezone",
            })
            assert status == 422 and error["error"]["code"] == "validation_failed", (status, error)
            assert counts() == before, "invalid IANA timezone wrote reminder state"
            print("PASS: API rejects a non-IANA identifier without reminder writes", flush=True)

            for kind in ("recurring", "one_time"):
                for timezone in ("Asia/Kolkata", "UTC"):
                    body = {"text": f"API {kind} {timezone}", "kind": kind,
                            "cron": "0 9 * * *", "timezone": timezone}
                    key = str(uuid.uuid4())
                    status, created = request("POST", "/api/v1/schedules", body, key)
                    assert status == 201, (status, created)
                    assert created["timezone"] == timezone
                    trigger = datetime.fromisoformat(created["next_run_at"].replace("Z", "+00:00"))
                    assert trigger.astimezone(ZoneInfo(timezone)).strftime("%H:%M") == "09:00"
                    before = counts()
                    assert request("POST", "/api/v1/schedules", body, key) == (status, created)
                    assert counts() == before, "idempotent replay created extra rows"
                    path = "/api/v1/schedules/" + created["id"]
                    assert request("GET", path)[1]["timezone"] == timezone
                    status, patched = request("PATCH", path, {"text": "edited without timezone"})
                    assert status == 200 and patched["timezone"] == timezone, (status, patched)
            print("PASS: API persists 4 explicit timezone/kind combinations, schedules 09:00 locally, replays idempotently, and preserves timezone on PATCH", flush=True)

            for kind in ("recurring", "one-time"):
                for timezone in ("Asia/Kolkata", "UTC"):
                    created = cli("create", "--text", f"CLI {kind} {timezone}", "--cron", "0 9 * * *",
                                  "--kind", kind, "--timezone", timezone)
                    assert created["timezone"] == timezone, created
                    assert cli("get", created["id"])["timezone"] == timezone
            print("PASS: actual CLI login/create/get through API and PostgreSQL", flush=True)
            if args.worker:
                receiver = ThreadingHTTPServer(("127.0.0.1", 0), WebhookReceiver)
                threading.Thread(target=receiver.serve_forever, daemon=True).start()
                status, subscription = request("POST", "/api/v1/webhooks", {
                    "endpoint_url": f"http://127.0.0.1:{receiver.server_port}/reminders",
                })
                assert status == 201, (status, subscription)
                due = datetime.fromtimestamp(((int(time.time()) + 5) // 60 + 1) * 60, utc_timezone.utc)
                expected = {}
                for timezone in ("Asia/Kolkata", "UTC"):
                    local = due.astimezone(ZoneInfo(timezone))
                    status, created = request("POST", "/api/v1/schedules", {
                        "text": f"Worker delivery {timezone}", "kind": "one_time",
                        "cron": f"{local.minute} {local.hour} {local.day} {local.month} *",
                        "timezone": timezone,
                    })
                    assert status == 201, (status, created)
                    assert datetime.fromisoformat(created["next_run_at"].replace("Z", "+00:00")) == due
                    expected[created["id"]] = timezone
                worker_env = dict(env, REMIND_WORKER_OPERATIONAL_BIND_ADDR=f"127.0.0.1:{available_port()}")
                worker = subprocess.Popen([str(ROOT / "target/debug/remind-worker")], cwd=scratch,
                                          env=worker_env, stdout=log, stderr=log)
                print(f"WAIT: actual worker delivery due at {due.isoformat()}", flush=True)
                deadline = due.timestamp() + 15
                while time.time() < deadline:
                    if worker.poll() is not None:
                        raise RuntimeError("worker exited before delivery")
                    delivered = {event["payload"]["schedule_id"] for event in WebhookReceiver.events}
                    if expected.keys() <= delivered:
                        break
                    if stopped.wait(0.5):
                        raise RuntimeError("interrupted while waiting for worker")
                else:
                    raise AssertionError("worker did not deliver both due reminders")
                for event in WebhookReceiver.events:
                    payload = event["payload"]
                    if payload["schedule_id"] not in expected:
                        continue
                    assert event["type"] == "remind.schedule.triggered", event
                    assert payload["timezone"] == expected[payload["schedule_id"]], event
                    assert datetime.fromisoformat(payload["scheduled_for"].replace("Z", "+00:00")) == due
                    status, reminder = request("GET", "/api/v1/schedules/" + payload["schedule_id"])
                    assert status == 200 and reminder["section"] == "archived" and reminder["status"] == "completed", reminder
                print("PASS: actual worker delivered Kolkata and UTC events at the intended instant and archived both one-time reminders", flush=True)
            details = {"api_origin": origin, "iam_origin": iam_origin, "org_id": ORG,
                       "slt": SLT, "access_token": ACCESS, "database_url": database_url,
                       "scratch": str(scratch), "stop_file": str(scratch / "stop")}
            (scratch / "fixture.json").write_text(json.dumps(details, indent=2))
            print("READY " + json.dumps(details), flush=True)
            if args.serve:
                while not stopped.wait(0.5) and not (scratch / "stop").exists():
                    if api.poll() is not None:
                        raise RuntimeError("API stopped unexpectedly")
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read()[-12000:], flush=True)
            raise
        finally:
            if worker and worker.poll() is None:
                worker.terminate()
                try:
                    worker.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    worker.kill()
                    worker.wait()
            if receiver:
                receiver.shutdown()
                receiver.server_close()
            if api and api.poll() is None:
                api.terminate()
                try:
                    api.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    api.kill()
                    api.wait()
            iam.shutdown()
            iam.server_close()
            if pg_started:
                run([PG_BIN / "pg_ctl", "-D", database, "-m", "immediate", "-w", "stop"])
            log.close()


if __name__ == "__main__":
    main()
