/** Refresh through the real Accounts stack using a local session whose stored expiry is advanced for each test. */
import { expect, test, type BrowserContext, type Page } from "@playwright/test";
import { locateSession } from "../lib/server/refresh-state";
import type { StoredSession } from "../lib/server/session";
import { accountsSession, appConfig, putSession, runAccounts, sameOrigin, sessionOf, signIn, storedSession } from "./support";

test.describe.configure({ mode: "serial" });

let context: BrowserContext;
let page: Page;

async function setExpiry(session: StoredSession, ms: number): Promise<void> {
  const { store, key, entry } = await locateSession(session);
  const expiring = { ...entry.value.session, ae: Date.now() + ms };
  expect(await store.write(key, entry.version, { ...entry.value, session: expiring }, expiring.re)).toBe(true);
  await putSession(context, expiring);
}

test.beforeAll(async ({ browser }) => {
  context = await accountsSession(browser, "carbon");
  page = await context.newPage();
  await signIn(page, runAccounts().carbon.email, appConfig.home);
});

test.afterAll(async () => {
  await context?.close();
});

test("a page load refreshes a token in its last two minutes, and the render uses the new one", async () => {
  const { session: before } = await storedSession(context);
  await setExpiry(before, 60_000);
  const response = await page.goto(appConfig.home);
  expect(response?.status()).toBe(200);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  const { session: after } = await storedSession(context);
  expect(after.rt !== before.rt).toBe(true);
  expect(after.at !== before.at).toBe(true);
  expect(after.ae).toBeGreaterThan(Date.now() + 25 * 60_000);
  expect(after.acct.uuid).toBe(before.acct.uuid);
});

test("a request still carrying the spent refresh token gets the same new tokens, and the sign-in lives on", async () => {
  const { session: current } = await storedSession(context);
  await setExpiry(current, 60_000);
  await page.goto(appConfig.home);
  const { session: rotated } = await storedSession(context);
  expect(rotated.rt !== current.rt).toBe(true);
  // A second tab sends the old cookie after rotation and should receive the current tokens.
  await putSession(context, current, 60_000);
  await page.goto(appConfig.home);
  const { session: again } = await storedSession(context);
  expect(again.rt === rotated.rt).toBe(true);
  // Had the spent token reached Accounts, the whole sign-in would be revoked now; it is not.
  await setExpiry(again, 60_000);
  await page.goto(appConfig.home);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  expect((await sessionOf(context)).signed_in).toBe(true);
});

test("the session keeper refreshes inside its five minutes, and only then", async () => {
  const { session: before } = await storedSession(context);
  await setExpiry(before, 4 * 60_000);
  const response = await page.request.post("/auth/refresh", { headers: sameOrigin() });
  expect(response.status()).toBe(200);
  const body = await response.json();
  expect(Date.parse(body.expires_at)).toBeGreaterThan(Date.now() + 25 * 60_000);
  const { session: after } = await storedSession(context);
  expect(after.rt !== before.rt).toBe(true);
  const quiet = await page.request.post("/auth/refresh", { headers: sameOrigin() });
  expect((await quiet.json()).expires_at).toBe(body.expires_at);
});
