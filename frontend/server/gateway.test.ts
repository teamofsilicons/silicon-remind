import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer, type Server } from "node:http";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { createGateway } from "./gateway.ts";

const listen = (server: Server) =>
  new Promise<string>((resolve) =>
    server.listen(0, "127.0.0.1", () =>
      resolve(
        `http://127.0.0.1:${(server.address() as { port: number }).port}`,
      ),
    ),
  );
const close = (server: Server) =>
  new Promise<void>((resolve, reject) => {
    server.closeAllConnections();
    server.close((e) => (e ? reject(e) : resolve()));
  });
const cookies = (response: Response) =>
  response.headers
    .getSetCookie()
    .map((c) => c.split(";")[0])
    .join("; ");

test("IAm browser handoff binds the browser, consumes state once, and keeps tokens on the server", async () => {
  let exchanges = 0;
  const envId = "11111111-1111-4111-8111-111111111111";
  const upstream = createServer(async (req, res) => {
    res.setHeader("Content-Type", "application/json");
    if (req.url === "/api/v1/auth/login") {
      let text = "";
      for await (const chunk of req) text += chunk;
      assert.equal(JSON.parse(text).slt, "valid-short-lived-token");
      assert.equal(req.headers["x-org-id"], "tos");
      assert.ok(req.headers["idempotency-key"]);
      exchanges++;
      res.end(
        JSON.stringify({
          access_token: "private-access",
          refresh_token: "private-refresh",
          expires_in: 3600,
        }),
      );
    } else if (req.url === "/api/v1/auth/me") {
      assert.equal(req.headers.authorization, "Bearer private-access");
      res.end(
        JSON.stringify({
          public_id: "person",
          principal_id: "person-id",
          org_id: "tos",
          actor_type: "carbon",
          can_manage_reminders: false,
        }),
      );
    } else if (req.url === `/api/v1/test-environments/${envId}/restorations`) {
      res.end(JSON.stringify({ environment_id: envId, key: "A".repeat(32) }));
    } else if (req.url === `/api/v1/test-environments/${envId}`) {
      assert.equal(req.headers.authorization, "Bearer private-access");
      res.end(JSON.stringify({ id: envId, name: "Restored sandbox" }));
    } else {
      res.writeHead(404);
      res.end("{}");
    }
  });
  const upstreamUrl = await listen(upstream);
  const directory = await mkdtemp(join(tmpdir(), "remind-browser-auth-"));
  let gateway: ReturnType<typeof createGateway>;
  const server = createServer((req, res) => {
    void gateway(req, res);
  });
  const origin = await listen(server);
  gateway = createGateway({
    origin,
    upstream: upstreamUrl,
    key: randomBytes(32).toString("base64url"),
    directory,
  });
  const start = (cookie = "") =>
    fetch(origin + "/ui/auth/start", {
      method: "POST",
      headers: {
        Origin: origin,
        "X-Remind-UI": "1",
        "Content-Type": "application/json",
        Cookie: cookie,
      },
      body: JSON.stringify({ org: "tos" }),
    });
  const callback = (url: string, cookie = "") =>
    fetch(url, {
      headers: { Cookie: cookie, "Sec-Fetch-Site": "cross-site" },
      redirect: "manual",
    });
  try {
    const csrf = await fetch(origin + "/ui/auth/start", {
      method: "POST",
      headers: { Origin: "https://attacker.example", "X-Remind-UI": "1" },
      body: '{"org":"tos"}',
    });
    assert.equal(csrf.status, 403);
    const begin = await start();
    assert.equal(begin.status, 200);
    const auth = new URL((await begin.json()).url);
    assert.equal(auth.origin, "https://auth.iam.teamofsilicons.com");
    assert.equal(auth.pathname, "/login");
    assert.equal(auth.searchParams.get("app_id"), "tos>remind");
    assert.equal(auth.searchParams.get("org_id"), "tos");
    const redirect = new URL(auth.searchParams.get("redirect_uri")!);
    assert.equal(redirect.origin, origin);
    assert.equal(redirect.pathname, "/ui/auth/callback");
    assert.match(redirect.searchParams.get("state")!, /^[a-f0-9]{64}$/);
    redirect.searchParams.set("slt", "valid-short-lived-token");
    const boundCookie = cookies(begin);
    assert.ok(
      begin.headers
        .getSetCookie()
        .some(
          (c) =>
            c.startsWith("remind_login=") &&
            c.includes("SameSite=Lax") &&
            c.includes("HttpOnly"),
        ),
    );
    const missing = await callback(redirect.href);
    assert.equal(missing.headers.get("location"), "/?login_error=1#reminders");
    assert.equal(exchanges, 0);
    const other = await start();
    const crossBrowser = await callback(redirect.href, cookies(other));
    assert.equal(
      crossBrowser.headers.get("location"),
      "/?login_error=1#reminders",
    );
    assert.equal(exchanges, 0);
    const altered = new URL(redirect);
    altered.searchParams.set("state", "0".repeat(64));
    assert.equal(
      (await callback(altered.href, boundCookie)).headers.get("location"),
      "/?login_error=1#reminders",
    );
    assert.equal(exchanges, 0);
    // A real cross-site navigation sends the Lax correlation cookie, not Strict.
    const correlationOnly = boundCookie
      .split("; ")
      .find((c) => c.startsWith("remind_login="))!;
    const success = await callback(redirect.href, correlationOnly);
    assert.equal(success.status, 303);
    assert.equal(success.headers.get("location"), "/#reminders");
    assert.equal(success.headers.get("referrer-policy"), "no-referrer");
    assert.equal(exchanges, 1);
    const sessionResponse = await fetch(origin + "/ui/session", {
      headers: { Cookie: cookies(success) },
    });
    const body = await sessionResponse.text();
    assert.ok(!body.includes("private-access"));
    assert.ok(!body.includes("private-refresh"));
    assert.equal(JSON.parse(body).identity.public_id, "person");
    const restored = await fetch(
      origin + `/ui/api/test-environments/${envId}/restorations`,
      {
        method: "POST",
        headers: {
          Cookie: cookies(success),
          Origin: origin,
          "X-Remind-UI": "1",
          "Content-Type": "application/json",
        },
        body: "{}",
      },
    );
    assert.equal(restored.status, 200);
    const restoredSession = await (
      await fetch(origin + "/ui/session", {
        headers: { Cookie: cookies(success) },
      })
    ).json();
    assert.equal(
      restoredSession.contexts.find((c: { id: string }) => c.id === envId).name,
      "Restored sandbox",
    );
    assert.equal(restoredSession.productionIdentity.public_id, "person");

    assert.equal(
      (await callback(redirect.href, correlationOnly)).headers.get("location"),
      "/?login_error=1#reminders",
    );
    assert.equal(exchanges, 1);
    // Starting a second attempt invalidates the first tab's outstanding callback.
    const old = await start(cookies(success));
    const oldUrl = new URL(
      new URL((await old.json()).url).searchParams.get("redirect_uri")!,
    );
    oldUrl.searchParams.set("slt", "valid-short-lived-token");
    await start(cookies(old));
    assert.equal(
      (await callback(oldUrl.href, cookies(old))).headers.get("location"),
      "/?login_error=1#reminders",
    );
    assert.equal(exchanges, 1);
  } finally {
    await close(server);
    await close(upstream);
    await rm(directory, { recursive: true, force: true });
  }
});
