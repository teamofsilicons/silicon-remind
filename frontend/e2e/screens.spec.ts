import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import {
  BASE,
  putSession,
  runAccounts,
  sameOrigin,
  watchErrors,
} from "./support";
import { seedSilicon } from "./seed";

let silicon: Awaited<ReturnType<typeof seedSilicon>>;
test.beforeAll(async () => {
  mkdirSync("test-results/screens", { recursive: true });
  silicon = await seedSilicon(runAccounts().carbon.email);
  for (const [text, cron, timezone] of [
    ["Review the launch checklist with c:ada", "0 9 * * 1-5", "Asia/Kolkata"],
    ["Send the weekly release notes", "30 16 * * 5", "Europe/London"],
    ["Check the overnight delivery report", "0 8 * * *", "America/New_York"],
  ]) {
    const response = await fetch("http://127.0.0.1:4181/api/v2/schedules", {
      method: "POST",
      headers: {
        Authorization: `Bearer ${silicon.tokens.access_token}`,
        "Content-Type": "application/json",
        "X-Remind-API-Version": "2",
        "Idempotency-Key": crypto.randomUUID(),
      },
      body: JSON.stringify({ text, cron, timezone, kind: "recurring" }),
    });
    expect(response.status).toBe(201);
  }
});

for (const theme of ["light", "dark"] as const) {
  for (const mobile of [false, true]) {
    test(`real product screens ${theme} ${mobile ? "phone" : "desktop"}`, async ({
      browser,
    }) => {
      const context = await browser.newContext({
        baseURL: BASE,
        bypassCSP: true,
        colorScheme: theme,
        reducedMotion: "reduce",
        viewport: mobile
          ? { width: 390, height: 844 }
          : { width: 1440, height: 1000 },
      });
      const token = silicon.tokens;
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
          display_name: "Release companion",
          pfp_url: null,
          custodian: token.account.custodian ?? null,
        },
      });
      const page = await context.newPage();
      const errors = watchErrors(page);
      async function capture(name: string) {
        await page.waitForLoadState("networkidle");
        await page.waitForTimeout(600);
        await expect
          .poll(() =>
            page.evaluate(
              () => document.documentElement.scrollWidth <= innerWidth,
            ),
          )
          .toBe(true);
        await page.addScriptTag({
          path: join(process.cwd(), "node_modules/axe-core/axe.min.js"),
        });
        const violations = await page.evaluate(async () => {
          const axe = (
            window as unknown as {
              axe: {
                run: (
                  node: Document,
                  options: object,
                ) => Promise<{
                  violations: Array<{ id: string; nodes: unknown[] }>;
                }>;
              };
            }
          ).axe;
          return (
            await axe.run(document, {
              runOnly: {
                type: "tag",
                values: [
                  "wcag2a",
                  "wcag2aa",
                  "wcag21a",
                  "wcag21aa",
                  "wcag22aa",
                ],
              },
            })
          ).violations;
        });
        expect(violations, name).toEqual([]);
        const dialog = page.getByRole("dialog");
        const modal = await dialog.count() > 0;
        if (modal) {
          await expect(dialog).toHaveCSS("opacity", "1");
          await page.getByRole("button", { name: "Close dialog", exact: true }).focus();
          const box = await dialog.boundingBox();
          expect(box).not.toBeNull();
          expect(box!.y).toBeGreaterThanOrEqual(0);
          expect(box!.y + box!.height).toBeLessThanOrEqual((page.viewportSize()?.height ?? 0) + 1);
        } else { await page.locator("main").focus(); }
        await page.screenshot({
          path: `test-results/screens/${name}-${theme}-${mobile ? "phone" : "desktop"}.png`,
          fullPage: !modal,
        });
      }
      for (const path of [
        "reminders",
        "silicons",
        "webhooks",
        "testing",
        "settings",
      ]) {
        await page.goto(`/${path}${path === "reminders" ? `?silicon=${silicon.uuid}` : ""}`);
        await capture(path);
      }
      await page.goto(`/reminders?silicon=${silicon.uuid}`);
      await page
        .getByRole("button", {
          name: "Review the launch checklist with c:ada",
          exact: true,
        })
        .click();
      await capture("reminder-details");
      await page
        .getByRole("button", { name: "Edit reminder", exact: true })
        .click();
      await capture("reminder-editor");
      const csrf = await context.request.post(`${BASE}/auth/environment`, {
        headers: sameOrigin(),
        data: { id: null },
      });
      expect(csrf.status()).toBe(200);
      expect(errors).toEqual([]);
      await context.close();
    });
  }
}
