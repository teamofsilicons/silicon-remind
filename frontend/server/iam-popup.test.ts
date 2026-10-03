import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer, type Server } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { createGateway } from "./gateway.ts";

const listen = (s: Server) => new Promise<string>(resolve => s.listen(0, "127.0.0.1", () => resolve(`http://127.0.0.1:${(s.address() as { port: number }).port}`)));
const close = (s: Server) => new Promise<void>(resolve => { s.closeAllConnections(); s.close(() => resolve()); });
const cookies = (r: Response) => r.headers.getSetCookie().map(c => c.split(";")[0]).join("; ");

test("typed popup and full-page callbacks verify the exchanged kind and expose only completion correlation", async () => {
  let kind = "carbon";
  let exchanges = 0;
  const upstream = createServer((req, res) => {
    res.setHeader("Content-Type", "application/json");
    const actor = { type: kind, public_id: kind === "carbon" ? "c:alice" : "si:chef" };
    if (req.url === "/api/v1/auth/login") {
      exchanges++;
      res.end(JSON.stringify({ access_token: "secret-access", refresh_token: "secret-refresh", expires_in: 3600, org_id: "tos", actor }));
    } else if (req.url === "/api/v1/auth/organizations") res.end(JSON.stringify({ items: [{ org_id: "tos" }] }));
    else if (req.url === "/api/v1/auth/me") res.end(JSON.stringify({ actor_type: kind, public_id: actor.public_id, org_id: "tos" }));
    else { res.writeHead(404); res.end("{}"); }
  });
  const upstreamUrl = await listen(upstream);
  const directory = await mkdtemp(join(tmpdir(), "remind-typed-signin-"));
  let gateway: ReturnType<typeof createGateway>;
  const server = createServer((req, res) => { void gateway(req, res); });
  const origin = await listen(server);
  gateway = createGateway({ origin, upstream: upstreamUrl, directory, key: randomBytes(32).toString("base64url") });
  const start = (identity_kind: string, display: string) => fetch(origin + "/ui/auth/start", { method: "POST", headers: { Origin: origin, "X-Remind-UI": "1", "Content-Type": "application/json", "X-Remind-Account": "signed-out", "X-Remind-Context": "production" }, body: JSON.stringify({ identity_kind, display }) });
  try {
    assert.equal((await start("unexpected", "popup")).status, 400);
    assert.equal((await start("carbon", "other")).status, 400);
    for (const identity of ["carbon", "silicon"]) for (const display of ["page", "popup"]) {
      kind = identity;
      const started = await start(identity, display);
      const data = await started.json();
      const destination = new URL(data.url);
      assert.equal(destination.searchParams.get("identity_kind"), identity);
      assert.equal(destination.searchParams.get("display"), display === "popup" ? "popup" : null);
      const callback = new URL(destination.searchParams.get("redirect_uri")!);
      assert.equal(callback.searchParams.get("state"), data.attempt);
      callback.searchParams.set("slt", "one-use-code");
      const response = await fetch(callback, { headers: { Cookie: cookies(started), "Sec-Fetch-Site": "cross-site" }, redirect: "manual" });
      if (display === "popup") {
        assert.equal(response.status, 200);
        const html = await response.text();
        assert.match(html, /remind:sign-in/);
        assert.ok(html.includes(`"attempt":"${data.attempt}"`));
        assert.ok(html.includes(`"kind":"${identity}"`));
        assert.ok(html.includes(`"ok":true`));
        assert.ok(html.includes(JSON.stringify(origin)));
        assert.match(response.headers.get("content-security-policy")!, /script-src 'nonce-/);
        for (const secret of ["secret-access", "secret-refresh", "one-use-code"]) assert.ok(!html.includes(secret));
      } else {
        assert.equal(response.status, 303);
        assert.equal(response.headers.get("location"), "/#reminders");
      }
      const selected = await (await fetch(origin + "/ui/session", { headers: { Cookie: cookies(response) } })).json();
      assert.equal(selected.identity.actor_type, identity);
      const replay = await fetch(callback, { headers: { Cookie: cookies(started) }, redirect: "manual" });
      assert.equal(replay.headers.get("location"), "/?login_error=1#reminders");
    }
    assert.equal(exchanges, 4);
    kind = "carbon";
    const started = await start("silicon", "popup");
    const data = await started.json();
    const callback = new URL(new URL(data.url).searchParams.get("redirect_uri")!);
    callback.searchParams.set("slt", "wrong-kind-code");
    const refused = await fetch(callback, { headers: { Cookie: cookies(started) }, redirect: "manual" });
    assert.match(await refused.text(), /"ok":false/);
    const selected = await (await fetch(origin + "/ui/session", { headers: { Cookie: cookies(started) } })).json();
    assert.equal(selected.identity, null);
  } finally { await close(server); await close(upstream); await rm(directory, { recursive: true, force: true }); }
});

test("an interrupted login retries its original encrypted attempt and stale tabs cannot start a new one", async () => {
  let exchangeUnavailable = true, identityUnavailable = true;
  const exchanges: Array<{ key: string | undefined; body: string }> = [];
  const upstream = createServer(async (req, res) => {
    res.setHeader("Content-Type", "application/json");
    if (req.url === "/api/v1/auth/login") {
      let body = ""; for await (const chunk of req) body += chunk;
      exchanges.push({ key: req.headers["idempotency-key"] as string, body });
      if (exchangeUnavailable) { exchangeUnavailable = false; res.writeHead(503); res.end("{}"); return; }
      res.end(JSON.stringify({ access_token: "private-access", refresh_token: "private-refresh", expires_in: 3600, org_id: "tos", actor: { type: "carbon", public_id: "c:alice" } }));
    } else if (req.url === "/api/v1/auth/organizations") res.end(JSON.stringify({ items: [{ org_id: "tos" }] }));
    else if (req.url === "/api/v1/auth/me") {
      if (identityUnavailable) { identityUnavailable = false; res.writeHead(503); res.end("{}"); return; }
      res.end(JSON.stringify({ actor_type: "carbon", public_id: "c:alice", org_id: "tos" }));
    } else { res.writeHead(404); res.end("{}"); }
  });
  const upstreamUrl = await listen(upstream);
  const directory = await mkdtemp(join(tmpdir(), "remind-login-retry-"));
  const key = randomBytes(32).toString("base64url");
  let gateway: ReturnType<typeof createGateway>;
  const server = createServer((req, res) => { void gateway(req, res); });
  const origin = await listen(server);
  const restart = () => { gateway = createGateway({ origin, upstream: upstreamUrl, directory, key }); };
  restart();
  const start = (cookie = "", account = "signed-out", world = "production") => fetch(origin + "/ui/auth/start", { method: "POST", headers: { Origin: origin, Cookie: cookie, "X-Remind-UI": "1", "X-Remind-Account": account, "X-Remind-Context": world, "Content-Type": "application/json" }, body: JSON.stringify({ identity_kind: "carbon", display: "popup" }) });
  try {
    const begin = await start(); const cookie = cookies(begin); const data = await begin.json();
    const callback = new URL(new URL(data.url).searchParams.get("redirect_uri")!); callback.searchParams.set("slt", "original-one-use-code");
    const first = await fetch(callback, { headers: { Cookie: cookie }, redirect: "manual" });
    const html = await first.text(); assert.match(html, /Retry this sign-in/); assert.ok(!html.includes("original-one-use-code"));
    restart();
    const retry = origin + "/ui/auth/retry?state=" + data.attempt;
    const second = await fetch(retry, { headers: { Cookie: cookie }, redirect: "manual" });
    assert.match(await second.text(), /Retry this sign-in/);
    assert.deepEqual(exchanges[0], exchanges[1]);
    restart();
    const third = await fetch(retry, { headers: { Cookie: cookie }, redirect: "manual" });
    assert.match(await third.text(), /"ok":true/);
    assert.equal(exchanges.length, 2, "verified exchange tokens survive a discovery retry without re-exchanging the SLT");
    const session = await (await fetch(origin + "/ui/session", { headers: { Cookie: cookie } })).json();
    assert.equal(session.identity.public_id, "c:alice");
    assert.equal((await start(cookie)).status, 409, "old signed-out tab cannot target the new account");
    assert.equal((await start(cookie, session.activeAccount, "stale-world")).status, 409);
    assert.equal((await start(cookie, session.activeAccount)).status, 200);
  } finally { await close(server); await close(upstream); await rm(directory, { recursive: true, force: true }); }
});
