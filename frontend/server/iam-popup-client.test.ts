import { test } from "node:test";
import assert from "node:assert/strict";
import { openIamPopup } from "../src/iam-popup.ts";

test("popup sign-in rejects spoofed messages, handles blocking and cancellation, and never navigates after cancellation", async t => {
  let receive: ((event: any) => void) | undefined;
  let opened = true;
  const popup = { closed: false, location: { href: "about:blank" }, focus() {}, close() { this.closed = true; } };
  const browser = { open: () => opened ? popup : null, addEventListener(_type: string, f: any) { receive = f; }, removeEventListener() { receive = undefined; } };
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window"), oldLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  Object.defineProperty(globalThis, "window", { configurable: true, value: browser });
  Object.defineProperty(globalThis, "location", { configurable: true, value: { origin: "https://remind.test" } });
  t.after(() => {
    if (oldWindow) Object.defineProperty(globalThis, "window", oldWindow); else Reflect.deleteProperty(globalThis, "window");
    if (oldLocation) Object.defineProperty(globalThis, "location", oldLocation); else Reflect.deleteProperty(globalThis, "location");
  });
  let starts = 0;
  const start = async () => { starts++; return { url: "https://auth.iam.test/login?identity_kind=carbon&display=popup", attempt: "a".repeat(64) }; };
  opened = false;
  await assert.rejects(openIamPopup("carbon", start, new AbortController().signal), /Continue in this tab/);
  assert.equal(starts, 0);
  opened = true;
  const controller = new AbortController();
  const pending = openIamPopup("carbon", start, controller.signal);
  await Promise.resolve();
  const valid = { origin: "https://remind.test", source: popup, data: { type: "remind:sign-in", attempt: "a".repeat(64), kind: "carbon", ok: true, contextId: "b".repeat(32) } };
  for (const invalid of [{ ...valid, origin: "https://attacker.test" }, { ...valid, source: {} }, { ...valid, data: { ...valid.data, attempt: "c".repeat(64) } }, { ...valid, data: { ...valid.data, kind: "silicon" } }]) {
    receive!(invalid); assert.equal(popup.closed, false);
  }
  receive!(valid);
  assert.equal(await pending, "b".repeat(32));
  assert.equal(popup.closed, true);
  popup.closed = false; popup.location.href = "about:blank";
  let finishStart!: (value: Awaited<ReturnType<typeof start>>) => void;
  const cancel = new AbortController();
  const waiting = openIamPopup("carbon", () => new Promise(resolve => { finishStart = resolve; }), cancel.signal);
  cancel.abort();
  await assert.rejects(waiting, /cancelled/);
  finishStart(await start());
  await Promise.resolve();
  assert.equal(popup.location.href, "about:blank");
});
