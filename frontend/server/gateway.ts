import { randomBytes, createCipheriv, createDecipheriv } from "node:crypto";
import { mkdir, readFile, writeFile, rename } from "node:fs/promises";
import { resolve } from "node:path";
import type { IncomingMessage, ServerResponse } from "node:http";
interface Context {
  id: string;
  name: string;
  org: string;
  key?: string;
  identity?: any;
  token?: string;
  refresh?: string;
  expires?: number;
  pending?: string;
}
interface State {
  active: string;
  contexts: Record<string, Context>;
  touched: number;
  login?: { nonce: string; contextId: string; org: string; expires: number };
}
const MAX = 1_048_576;
export function createGateway(config: {
  origin: string;
  upstream: string;
  key: string;
  directory: string;
  authOrigin?: string;
}) {
  const upstream = new URL(config.upstream),
    origin = new URL(config.origin),
    key = Buffer.from(config.key, "base64url");
  const authOrigin = new URL(
    config.authOrigin || "https://auth.iam.teamofsilicons.com",
  );
  if (
    authOrigin.protocol !== "https:" ||
    authOrigin.username ||
    authOrigin.password ||
    authOrigin.pathname !== "/" ||
    authOrigin.search ||
    authOrigin.hash
  )
    throw Error("IAM auth origin must be an HTTPS origin");
  const secure = origin.protocol === "https:";
  const loginCookieName = secure ? "__Host-remind_login" : "remind_login";
  const sessionCookie = (id: string) =>
    `remind_session=${id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800${secure ? "; Secure" : ""}`;
  const loginCookie = (value: string, age = 600) =>
    `${loginCookieName}=${value}; HttpOnly; SameSite=Lax; Path=/; Max-Age=${age}${secure ? "; Secure" : ""}`;
  if (key.length !== 32) throw Error("SESSION_KEY must encode 32 bytes");
  if (
    !["https:"].includes(upstream.protocol) &&
    !(
      upstream.protocol === "http:" &&
      ["127.0.0.1", "localhost", "[::1]"].includes(upstream.hostname)
    )
  )
    throw Error("Upstream must use HTTPS or loopback");
  if (
    upstream.username ||
    upstream.password ||
    upstream.search ||
    upstream.hash ||
    upstream.pathname !== "/"
  )
    throw Error("Upstream must be an origin");
  const directory = resolve(config.directory),
    locks = new Map<string, Promise<void>>();
  const fresh = (): State => ({
    active: "production",
    contexts: { production: { id: "production", name: "Production", org: "" } },
    touched: Date.now(),
  });
  async function load(id: string): Promise<State> {
    try {
      const b = await readFile(resolve(directory, id));
      const dec = createDecipheriv("aes-256-gcm", key, b.subarray(0, 12));
      dec.setAuthTag(b.subarray(12, 28));
      const s = JSON.parse(
        Buffer.concat([dec.update(b.subarray(28)), dec.final()]).toString(),
      );
      return Date.now() - s.touched < 7 * 86400000 ? s : fresh();
    } catch (e: any) {
      if (e.code === "ENOENT") return fresh();
      throw Error(
        "Session could not be read. Sign in in a new browser session.",
      );
    }
  }
  async function save(id: string, s: State) {
    await mkdir(directory, { recursive: true, mode: 0o700 });
    const iv = randomBytes(12),
      c = createCipheriv("aes-256-gcm", key, iv);
    const b = Buffer.concat([c.update(JSON.stringify(s)), c.final()]);
    const p = resolve(directory, id + ".tmp");
    await writeFile(p, Buffer.concat([iv, c.getAuthTag(), b]), { mode: 0o600 });
    await rename(p, resolve(directory, id));
  }
  async function remote(
    path: string,
    ctx: Context,
    method = "GET",
    body?: unknown,
    mutation?: string,
    bearer = true,
  ) {
    const headers: Record<string, string> = { Accept: "application/json" };
    if (ctx.org) headers["X-Org-ID"] = ctx.org;
    if (ctx.key) headers["X-Remind-Test-Key"] = ctx.key;
    if (bearer && ctx.token) headers.Authorization = "Bearer " + ctx.token;
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (mutation) headers["Idempotency-Key"] = mutation;
    const response = await fetch(new URL(path, upstream), {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      redirect: "error",
      signal: AbortSignal.timeout(30000),
    });
    const reader = response.body?.getReader();
    let size = 0;
    const chunks: Uint8Array[] = [];
    if (reader)
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.length;
        if (size > 16 * 1024 * 1024) {
          await reader.cancel();
          throw Error("Response exceeded the allowed size");
        }
        chunks.push(value);
      }
    const bytes = Buffer.concat(chunks);
    let data: any = null;
    try {
      data = bytes.length ? JSON.parse(bytes.toString()) : null;
    } catch {
      throw Error("The backend returned an unexpected response");
    }
    if (!response.ok)
      throw Object.assign(Error(data?.error?.message || "Request failed"), {
        status: response.status,
        detail: data?.error,
        retry: response.headers.get("retry-after"),
      });
    return data;
  }
  const summary = (s: State) => ({
    active: s.active,
    contexts: Object.values(s.contexts).map((c) => ({
      id: c.id,
      name: c.name,
      org: c.org,
      identity: c.identity || null,
    })),
    identity: s.contexts[s.active]?.identity || null,
    productionIdentity: s.contexts.production.identity || null,
  });
  return async (req: IncomingMessage, res: ServerResponse) => {
    const url = new URL(req.url || "/", origin);
    if (!url.pathname.startsWith("/ui/")) return false;
    const callback =
      url.pathname === "/ui/auth/callback" && req.method === "GET";
    res.setHeader("Referrer-Policy", "no-referrer");
    res.setHeader("Cache-Control", "no-store");
    res.setHeader("Content-Type", "application/json");
    res.setHeader("X-Content-Type-Options", "nosniff");
    if (
      req.headers.host !== origin.host ||
      (!callback &&
        req.headers.origin &&
        req.headers.origin !== origin.origin) ||
      (!callback && req.headers["sec-fetch-site"] === "cross-site") ||
      (req.method !== "GET" && req.headers["x-remind-ui"] !== "1")
    ) {
      res.writeHead(403);
      res.end(
        JSON.stringify({ error: { message: "Request origin was rejected." } }),
      );
      return true;
    }
    const cookie = (req.headers.cookie || "").match(
      /(?:^|; )remind_session=([a-f0-9]{64})(?:;|$)/,
    )?.[1];
    const correlation = (req.headers.cookie || "")
      .split(";")
      .map((v) => v.trim())
      .find((v) => v.startsWith(loginCookieName + "="))
      ?.slice(loginCookieName.length + 1);
    const correlationMatch = correlation?.match(
      /^([a-f0-9]{64})[.]([a-f0-9]{64})$/,
    );
    const id =
      (callback ? correlationMatch?.[1] : cookie) ||
      randomBytes(32).toString("hex");
    if (!cookie && !callback) res.setHeader("Set-Cookie", sessionCookie(id));
    const previous = locks.get(id) || Promise.resolve();
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const tail = previous.then(() => gate);
    locks.set(id, tail);
    await previous;
    try {
      let size = 0;
      const chunks: Buffer[] = [];
      for await (const b of req) {
        size += b.length;
        if (size > MAX)
          throw Object.assign(Error("Request is too large"), { status: 413 });
        chunks.push(b);
      }
      let body: any = {};
      if (size)
        try {
          body = JSON.parse(Buffer.concat(chunks).toString());
        } catch {
          throw Object.assign(Error("Invalid JSON"), { status: 400 });
        }
      const state = await load(id);
      state.touched = Date.now();
      let ctx = state.contexts[state.active],
        data: any;
      async function authenticated(c: Context) {
        if (c.refresh && (c.pending || (c.expires || 0) < Date.now() + 30000)) {
          c.pending ||= randomBytes(16).toString("hex");
          await save(id, state);
          try {
            const tokens = await remote(
              "/api/v1/auth/refresh",
              c,
              "POST",
              { refresh_token: c.refresh },
              c.pending,
              false,
            );
            c.token = tokens.access_token;
            c.refresh = tokens.refresh_token;
            c.expires = Date.now() + tokens.expires_in * 1000;
            delete c.pending;
            await save(id, state);
          } catch (e: any) {
            if (e.status === 401) {
              delete c.token;
              delete c.refresh;
              delete c.identity;
              delete c.pending;
              await save(id, state);
            }
            throw e;
          }
        }
      }
      if (callback) {
        const attempt = state.login;
        const nonce = url.searchParams.get("state");
        const slt = url.searchParams.get("slt");
        if (
          !correlationMatch ||
          !attempt ||
          nonce !== correlationMatch[2] ||
          nonce !== attempt.nonce ||
          attempt.expires < Date.now() ||
          attempt.contextId !== state.active ||
          !slt ||
          slt.length > 16384 ||
          url.searchParams.getAll("slt").length !== 1 ||
          url.searchParams.getAll("state").length !== 1
        ) {
          throw Object.assign(Error("Login callback could not be verified"), {
            status: 400,
          });
        }
        // Consume correlation before exchanging the one-use SLT. An interrupted
        // browser handoff restarts sign-in; it must never replay a callback.
        delete state.login;
        await save(id, state);
        const candidate = {
          ...state.contexts[attempt.contextId],
          org: attempt.org,
        };
        const tokens = await remote(
          "/api/v1/auth/login",
          candidate,
          "POST",
          { slt },
          attempt.nonce,
          false,
        );
        candidate.token = tokens.access_token;
        candidate.refresh = tokens.refresh_token;
        candidate.expires = Date.now() + tokens.expires_in * 1000;
        delete candidate.pending;
        candidate.identity = await remote("/api/v1/auth/me", candidate);
        state.contexts[attempt.contextId] = candidate;
        await save(id, state);
        res.setHeader("Set-Cookie", [sessionCookie(id), loginCookie("", 0)]);
        res.writeHead(303, { Location: "/#reminders" });
        res.end();
        return true;
      } else if (url.pathname === "/ui/auth/start" && req.method === "POST") {
        if (state.active !== "production")
          throw Object.assign(
            Error(
              "Use a test IAm token for this sandbox; the hosted IAm sign-in is production-only.",
            ),
            { status: 422 },
          );
        const org = typeof body.org === "string" ? body.org.trim() : "";
        if (!/^[a-z0-9][a-z0-9_-]{0,99}$/.test(org))
          throw Object.assign(Error("Enter an organization handle"), {
            status: 422,
          });
        const nonce = randomBytes(32).toString("hex");
        state.login = {
          nonce,
          org,
          contextId: state.active,
          expires: Date.now() + 600000,
        };
        const redirect = new URL("/ui/auth/callback", origin);
        redirect.searchParams.set("state", nonce);
        const destination = new URL("/login", authOrigin);
        destination.searchParams.set("app_id", "tos>remind");
        destination.searchParams.set("org_id", org);
        destination.searchParams.set("redirect_uri", redirect.href);
        res.setHeader("Set-Cookie", [
          sessionCookie(id),
          loginCookie(`${id}.${nonce}`),
        ]);
        data = { url: destination.href };
      } else if (url.pathname === "/ui/session" && req.method === "GET") {
        data = summary(state);
      } else if (url.pathname === "/ui/login" && req.method === "POST") {
        delete state.login;
        if (
          typeof body.org !== "string" ||
          typeof body.slt !== "string" ||
          !body.org.trim() ||
          !body.slt.trim()
        )
          throw Object.assign(
            Error("Organization and short-lived token are required"),
            { status: 422 },
          );
        const candidate = { ...ctx, org: body.org.trim() };
        const tokens = await remote(
          "/api/v1/auth/login",
          candidate,
          "POST",
          { slt: body.slt.trim() },
          (req.headers["idempotency-key"] as string) ||
            randomBytes(16).toString("hex"),
          false,
        );
        candidate.token = tokens.access_token;
        candidate.refresh = tokens.refresh_token;
        candidate.expires = Date.now() + tokens.expires_in * 1000;
        delete candidate.pending;
        candidate.identity = await remote("/api/v1/auth/me", candidate);
        state.contexts[state.active] = candidate;
        data = summary(state);
      } else if (url.pathname === "/ui/logout" && req.method === "POST") {
        delete state.login;
        if (ctx.refresh)
          await remote(
            "/api/v1/auth/logout",
            ctx,
            "POST",
            { token: ctx.refresh },
            undefined,
            false,
          );
        delete ctx.token;
        delete ctx.refresh;
        delete ctx.identity;
        delete ctx.pending;
        data = summary(state);
      } else if (url.pathname === "/ui/context" && req.method === "POST") {
        delete state.login;
        if (body.action === "import") {
          if (
            typeof body.key !== "string" ||
            !/^[a-zA-Z0-9]{32}$/.test(body.key)
          )
            throw Object.assign(
              Error("Enter the 32-character Remind test key"),
              { status: 422 },
            );
          const candidate: Context = {
            id: "",
            name: "",
            org: "",
            key: body.key,
          };
          const env = await remote("/api/v1/testing-environment", candidate);
          candidate.id = env.id;
          candidate.name = env.name;
          if (body.id && body.id !== env.id)
            throw Object.assign(
              Error("The key belongs to a different environment"),
              { status: 422 },
            );
          state.contexts[env.id] = candidate;
          state.active = env.id;
        } else if (body.action === "forget") {
          if (body.id === "production")
            throw Object.assign(Error("Production cannot be forgotten"), {
              status: 422,
            });
          delete state.contexts[body.id];
          if (state.active === body.id) state.active = "production";
        } else {
          if (!Object.hasOwn(state.contexts, body.id))
            throw Object.assign(Error("Import this environment first"), {
              status: 404,
            });
          state.active = body.id;
        }
        data = summary(state);
      } else if (url.pathname.startsWith("/ui/api/")) {
        const path = url.pathname.slice(7),
          method = req.method || "GET";
        const allowed = [
          /^\/schedules(?:\/[a-f0-9-]+(?:\/executions)?)?$/,
          /^\/webhook$/,
          /^\/webhooks(?:\/[a-f0-9-]+)?$/,
          /^\/silicons$/,
          /^\/auth\/me$/,
          /^\/test-environments(?:\/[a-f0-9-]+(?:\/(?:key|key-rotations|restorations))?)?$/,
          /^\/testing-environment(?:\/(?:iam|cleanings))?$/,
          /^\/health\/(?:live|ready)$/,
        ];
        if (
          !allowed.some((r) => r.test(path)) ||
          !["GET", "POST", "PATCH", "PUT", "DELETE"].includes(method)
        )
          throw Object.assign(Error("Unknown action"), { status: 404 });
        if (path.startsWith("/test-environments"))
          ctx = state.contexts.production;
        const health = path.startsWith("/health/");
        if (health) ctx = { id: "", name: "", org: "" };
        else if (!path.startsWith("/testing-environment"))
          await authenticated(ctx);
        data = await remote(
          (health ? "" : "/api/v1") + path + url.search,
          ctx,
          method,
          ["GET", "HEAD", "DELETE"].includes(method) ? undefined : body,
          (req.headers["idempotency-key"] as string) ||
            (["POST", "PATCH", "PUT"].includes(method)
              ? randomBytes(16).toString("hex")
              : undefined),
        );
        if (path === "/auth/me") ctx.identity = data;
        if (path === "/testing-environment" && data?.name) ctx.name = data.name;
        if (
          method === "DELETE" &&
          /^\/test-environments\/[a-f0-9-]+$/.test(path)
        ) {
          const envId = path.split("/")[2];
          delete state.contexts[envId];
          if (state.active === envId) state.active = "production";
        }
        if (data?.key && path.startsWith("/test-environments")) {
          let env = data.environment;
          const envId = env?.id || data.environment_id;
          if (envId) {
            if (!env)
              env = await remote(
                "/api/v1/test-environments/" + envId,
                state.contexts.production,
              );
            const old = state.contexts[envId];
            state.contexts[envId] =
              old?.key === data.key
                ? { ...old, name: env?.name || old.name }
                : {
                    id: envId,
                    name: env?.name || old?.name || envId,
                    org: old?.org || "",
                    key: data.key,
                  };
          }
        }
      } else throw Object.assign(Error("Unknown action"), { status: 404 });
      await save(id, state);
      res.statusCode = data === null ? 204 : 200;
      res.end(data === null ? undefined : JSON.stringify(data));
    } catch (e: any) {
      if (callback) {
        res.setHeader("Set-Cookie", loginCookie("", 0));
        res.writeHead(303, { Location: "/?login_error=1#reminders" });
        res.end();
        return true;
      }
      res.statusCode = e.status || 502;
      if (e.retry) res.setHeader("Retry-After", e.retry);
      res.end(
        JSON.stringify({
          error: e.detail || {
            message: e.status
              ? e.message
              : "Could not complete the request. Try again.",
            code: "gateway_error",
          },
        }),
      );
    } finally {
      release();
      if (locks.get(id) === tail) locks.delete(id);
    }
    return true;
  };
}
