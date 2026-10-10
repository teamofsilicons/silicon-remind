import { expect, test, type Locator } from "@playwright/test";
import {
  accountsSession,
  BASE,
  putSession,
  runAccounts,
  sameOrigin,
  signIn,
  watchErrors,
} from "./support";
import { seedSilicon } from "./seed";
async function hold(button: Locator) {
  await button.focus();
  await button.press("Space", { delay: 1400 });
}
test("Carbon manages real test environments and the key stays sealed", async ({
  browser,
}) => {
  const context = await accountsSession(browser, "carbon");
  const page = await context.newPage();
  const errors = watchErrors(page);
  await signIn(page, runAccounts().carbon.email, "/testing");
  const name = `Browser preview ${Date.now()}`;
  await page
    .getByRole("button", { name: "New environment", exact: true })
    .click();
  await page.getByRole("textbox", { name: "Environment name" }).fill(name);
  await page
    .getByRole("textbox", { name: "Description", exact: true })
    .fill("A real isolated browser proof");
  await page
    .getByRole("button", { name: "Create environment", exact: true })
    .click();
  const keyDialog = page.getByRole("dialog", { name: `Key for ${name}` });
  await expect(keyDialog).toBeVisible();
  const key = await keyDialog
    .locator("p")
    .filter({ hasText: /^[A-Za-z0-9]{32}$/ })
    .textContent();
  expect(key).toHaveLength(32);
  await keyDialog.getByRole("button", { name: "Done", exact: true }).click();
  const row = page
    .getByRole("listitem")
    .filter({ has: page.getByRole("heading", { name, exact: true }) });
  await row
    .getByRole("button", { name: "Use environment", exact: true })
    .click();
  await expect(
    page.getByText(`Testing · ${name}`, { exact: true }),
  ).toBeVisible();
  const cookie = (await context.cookies()).find(
    (c) => c.name === "remind_environment",
  );
  expect(cookie?.httpOnly).toBe(true);
  expect(cookie?.value).not.toContain(key!);
  const service = await context.request.get(`${BASE}/api/testing-environment`);
  expect(service.status()).toBe(200);
  const selected = await service.json();
  expect(selected.name).toBe(name);
  await hold(row.getByRole("button", { name: /Hold to rotate key/ }));
  await expect(keyDialog).toBeVisible();
  const rotated = await keyDialog
    .locator("p")
    .filter({ hasText: /^[A-Za-z0-9]{32}$/ })
    .textContent();
  expect(rotated).not.toBe(key);
  await keyDialog.getByRole("button", { name: "Done", exact: true }).click();
  const stale = await context.request.get(`${BASE}/api/schedules`);
  expect(stale.status()).toBe(409);
  expect((await stale.json()).error.code).toBe("test_key_invalid");
  const signedIn = await context.request.get(`${BASE}/auth/session`);
  expect((await signedIn.json()).signed_in).toBe(true);
  await row.getByRole("button", { name: "Refresh selection", exact: true }).click();
  expect((await context.request.get(`${BASE}/api/schedules`)).status()).toBe(200);
  await page
    .getByRole("button", { name: "Back to Production", exact: true })
    .click();
  await expect(
    page.getByText(`Testing · ${name}`, { exact: true }),
  ).toHaveCount(0);

  await hold(row.getByRole("button", { name: /Hold to retire/ }));
  await expect(row).toHaveCount(0);
  await page
    .getByRole("checkbox", { name: "Include retired environments" })
    .check();
  await row
    .getByRole("button", { name: "Restore environment", exact: true })
    .click();
  await expect(keyDialog).toBeVisible();
  await keyDialog.getByRole("button", { name: "Done", exact: true }).click();
  await expect(
    row.getByRole("button", { name: "Use environment", exact: true }),
  ).toBeVisible();
  expect(errors).toEqual([]);
  await context.close();
});
test("Silicon creates, edits, pauses, shares, configures delivery and archives real reminders", async ({
  browser,
}) => {
  const silicon = await seedSilicon(runAccounts().carbon.email);
  const token = silicon.tokens;
  const context = await browser.newContext({ baseURL: BASE });
  await putSession(context, {
    v: 1,
    at: token.access_token,
    rt: token.refresh_token,
    ae: Date.now() + token.expires_in * 1000,
    re: Date.parse(token.refresh_token_expires_at),
    scope: token.scope,
    acct: {
      uuid: silicon.uuid,
      id: silicon.id,
      kind: "silicon",
      display_name: token.account.display_name || silicon.id,
      pfp_url: null,
      custodian: token.account.custodian ?? null,
    },
  });
  const page = await context.newPage();
  const errors = watchErrors(page);
  await page.goto("/reminders");
  await page.getByRole("button", { name: "New reminder", exact: true }).click();
  await page
    .getByRole("textbox", { name: "Reminder", exact: true })
    .fill("Remember the browser proof");
  await page
    .getByRole("textbox", { name: "Timezone", exact: true })
    .fill("Asia/Kolkata");
  await page
    .getByRole("button", { name: "Create reminder", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page
    .getByRole("button", { name: "Remember the browser proof", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Edit reminder", exact: true })
    .click();
  await page
    .getByRole("textbox", { name: "Reminder", exact: true })
    .fill("Browser proof updated");
  await page.getByRole("button", { name: "Save changes", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page
    .getByRole("checkbox", { name: "Select Browser proof updated" })
    .check();
  await page
    .getByRole("button", { name: "Pause selected", exact: true })
    .click();
  await expect(page.getByText("paused", { exact: true })).toBeVisible();
  await page
    .getByRole("checkbox", { name: "Select Browser proof updated" })
    .check();
  await page
    .getByRole("button", { name: "Resume selected", exact: true })
    .click();
  await expect(page.getByText("active", { exact: true })).toBeVisible();
  await page.goto("/silicons");
  await page
    .getByRole("textbox", { name: "Account to share with" })
    .fill(runAccounts().friend.id);
  await page.getByRole("button", { name: "Add viewer", exact: true }).click();
  await expect(
    page.getByText(runAccounts().friend.id, { exact: true }),
  ).toBeVisible();
  const friend = await accountsSession(browser, "friend");
  const friendPage = await friend.newPage();
  await signIn(friendPage, runAccounts().friend.email, "/reminders");
  await expect(
    friendPage.getByRole("button", {
      name: "Browser proof updated",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    friendPage.getByRole("button", { name: "New reminder", exact: true }),
  ).toHaveCount(0);
  await friend.close();
  await page.goto("/webhooks");
  await page
    .getByRole("textbox", { name: "Destination URL" })
    .fill("http://127.0.0.1:4183/browser-reminders");
  await page
    .getByRole("textbox", { name: "Signing secret (optional)" })
    .fill("local-browser-proof-secret");
  await page.getByRole("button", { name: "Add webhook", exact: true }).click();
  await expect(
    page.getByText("http://127.0.0.1:4183/browser-reminders", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Signing secret (optional)" }),
  ).toHaveValue("");
  await page.goto("/reminders");
  await page
    .getByRole("button", { name: "Browser proof updated", exact: true })
    .click();
  await hold(page.getByRole("button", { name: /Hold to archive/ }));
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.goto("/archive");
  await expect(
    page.getByRole("button", { name: "Browser proof updated", exact: true }),
  ).toBeVisible();
  const csrf = await context.request.post(`${BASE}/auth/environment`, {
    headers: { Origin: "https://evil.example" },
    data: { id: null },
  });
  expect(csrf.status()).toBe(403);
  const logout = await context.request.post(`${BASE}/auth/sign-out`, {
    headers: sameOrigin(),
  });
  expect(logout.status()).toBe(204);
  expect(
    (await context.cookies()).some((c) => c.name === "remind_environment"),
  ).toBe(false);
  expect(errors).toEqual([]);
  await context.close();
});
