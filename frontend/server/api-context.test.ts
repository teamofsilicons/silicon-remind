import { test } from "node:test";
import assert from "node:assert/strict";
import { request, api } from "../src/api.ts";

test("pending browser operations keep their world and identity and cannot restore old selection", async () => {
  const originalFetch = globalThis.fetch;
  const storage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: { getItem: () => null } });
  const result = (active: string, account: string) => new Response(JSON.stringify({ active, activeAccount: account, productionAccount: "production-account" }));
  try {
    globalThis.fetch = async () => result("production", "alice");
    await request("session");
    let oldSession!: (response: Response) => void;
    globalThis.fetch = () => new Promise(resolve => { oldSession = resolve; });
    const pendingSession = request("session");
    const staleSession = assert.rejects(pendingSession, /changed while loading/);
    globalThis.fetch = async () => result("production", "bob");
    await request("account", "POST", { id: "bob" });
    oldSession(result("production", "alice"));
    await staleSession;
    globalThis.fetch = async (_url, init) => {
      assert.equal(new Headers(init?.headers).get("X-Remind-Account"), "bob");
      return new Response("{}");
    };
    await api("/schedules");

    globalThis.fetch = async () => result("sandbox-a", "signed-out");
    await request("context", "POST", { id: "sandbox-a" });
    const keys: string[] = [];
    let oldRead!: (response: Response) => void;
    globalThis.fetch = (_url, init) => {
      const headers = new Headers(init?.headers);
      assert.equal(headers.get("X-Remind-Context"), "sandbox-a");
      keys.push(headers.get("Idempotency-Key")!);
      return new Promise(resolve => { oldRead = resolve; });
    };
    const pendingRead = api("/testing-environment/cleanings", "POST", {});
    const staleRead = assert.rejects(pendingRead, /changed while loading/);
    globalThis.fetch = async () => result("sandbox-b", "signed-out");
    await request("context", "POST", { id: "sandbox-b" });
    oldRead(new Response("{}"));
    await staleRead;
    globalThis.fetch = async (_url, init) => {
      const headers = new Headers(init?.headers);
      assert.equal(headers.get("X-Remind-Context"), "sandbox-b");
      keys.push(headers.get("Idempotency-Key")!);
      return new Response("{}");
    };
    await api("/testing-environment/cleanings", "POST", {});
    assert.notEqual(keys[0], keys[1]);
  } finally {
    globalThis.fetch = originalFetch;
    if (storage) Object.defineProperty(globalThis, "localStorage", storage);
    else Reflect.deleteProperty(globalThis, "localStorage");
  }
});
