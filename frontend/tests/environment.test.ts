import assert from "node:assert/strict";
import { test, beforeEach, afterEach } from "node:test";
import { NextRequest, NextResponse } from "next/server";
import { applyTestEnv, mockFetch, json } from "./env";
import {
  selectedEnvironment,
  writeEnvironment,
  environmentCookieName,
} from "../lib/server/environment";
import { sealSession } from "../lib/server/session";
import { GET } from "../app/api/[...path]/route";
const uuid = "0192a3b4-0000-4000-8000-000000000000";
const selection = {
  v: 1 as const,
  account: uuid,
  id: "0192a3b4-0000-4000-8000-000000000001",
  name: "Preview",
  key: "testkey1234567890testkey12345678",
};
let restore: (() => void) | undefined;
beforeEach(() => applyTestEnv());
afterEach(() => {
  restore?.();
  restore = undefined;
});
function selectedCookie() {
  const response = NextResponse.next();
  writeEnvironment(response, selection);
  return response.headers.get("set-cookie")!.split(";")[0]!;
}
test("test keys are sealed, httpOnly and bound to the canonical account UUID", () => {
  const response = NextResponse.next();
  writeEnvironment(response, selection);
  const header = response.headers.get("set-cookie")!;
  assert.match(header, /HttpOnly/);
  assert.ok(!header.includes(selection.key));
  const request = new NextRequest("http://127.0.0.1:4260", {
    headers: { cookie: header.split(";")[0]! },
  });
  assert.deepEqual(selectedEnvironment(request, uuid), selection);
  assert.equal(selectedEnvironment(request, "someoneElse"), null);
  writeEnvironment(response, null);
  assert.match(response.headers.get("set-cookie")!, /Max-Age=0/);
});
test("service version and test key come from the server; environment administration stays in Production", async () => {
  const mock = mockFetch(() => json(200, { items: [] }));
  restore = mock.restore;
  const session = {
    v: 1 as const,
    at: "eyJ.live.sig",
    rt: "sar_live",
    ae: Date.now() + 1800000,
    re: Date.now() + 86400000,
    scope: "profile",
    acct: {
      uuid,
      id: "c:ada",
      kind: "carbon" as const,
      display_name: "Ada",
      pfp_url: null,
      custodian: null,
    },
  };
  const cookie = `remind_session=${sealSession(session)}; ${selectedCookie()}`;
  for (const path of ["schedules", "test-environments", "viewers"]) {
    const response = await GET(
      new NextRequest(`http://127.0.0.1:4260/api/${path}`, {
        headers: {
          cookie,
          "x-remind-test-key": "injected",
          "x-remind-api-version": "1",
        },
      }),
    );
    assert.equal(response.status, 200);
  }
  for (const call of mock.calls)
    assert.equal(
      new Headers(call.init.headers).get("x-remind-api-version"),
      "2",
    );
  assert.equal(
    new Headers(mock.calls[0]!.init.headers).get("x-remind-test-key"),
    selection.key,
  );
  assert.equal(
    new Headers(mock.calls[1]!.init.headers).get("x-remind-test-key"),
    null,
  );
  assert.equal(
    new Headers(mock.calls[2]!.init.headers).get("x-remind-test-key"),
    null,
  );
  assert.equal(environmentCookieName(), "remind_environment");
});
