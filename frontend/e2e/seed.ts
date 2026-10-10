/** Creates an isolated Silicon through the shared local Accounts testkit, then signs it in to real Remind. */
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { expect } from "@playwright/test";
export async function seedSilicon(email: string) {
  const mint = process.env.REMIND_E2E_MINT;
  const stackPath = process.env.TEST_STACK_JSON;
  if (!mint || !stackPath)
    throw new Error(
      "Set REMIND_E2E_MINT and TEST_STACK_JSON to the local Accounts testkit; Remind browser journeys exercise real Accounts and Remind services.",
    );
  const stack = JSON.parse(readFileSync(stackPath, "utf8"));
  const silicon = JSON.parse(
    execFileSync(
      process.env.REMIND_E2E_TSX || "./node_modules/.bin/tsx",
      [
        mint,
        "silicon",
        "--custodian-email",
        email,
        "--handle",
        `remind-web-${Date.now().toString(36)}`,
      ],
      { encoding: "utf8", timeout: 60000 },
    ),
  );
  async function post(
    path: string,
    data: object,
    headers: Record<string, string> = {},
  ) {
    const response = await fetch(`${stack.accounts_api_url}${path}`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...headers },
      body: JSON.stringify(data),
    });
    expect(response.status, path).toBe(
      path === "/v1/me/short-lived-tokens" ? 201 : 200,
    );
    return response.json();
  }
  const firstParty = await post("/v1/silicons/login", {
    id: silicon.id,
    stk: silicon.stk,
    client_label: "Remind browser test",
  });
  const slt = await post(
    "/v1/me/short-lived-tokens",
    { app_id: "remind" },
    { Authorization: `Bearer ${firstParty.access_token}` },
  );
  const token = await fetch(`${stack.accounts_api_url}/v1/oauth/token`, {
    method: "POST",
    headers: {
      Authorization: `Basic ${Buffer.from(`remind:${stack.apps.remind.app_secret}`).toString("base64")}`,
      "Content-Type": "application/x-www-form-urlencoded",
    },
    body: new URLSearchParams({
      grant_type: "urn:silicon:params:oauth:grant-type:slt",
      slt: slt.slt,
    }),
  });
  expect(token.status).toBe(200);
  const tokens = await token.json();
  const status = await fetch(
    `${process.env.APP_API_URL || "http://127.0.0.1:4181"}/api/v2/auth/me`,
    {
      headers: {
        Authorization: `Bearer ${tokens.access_token}`,
        "X-Remind-API-Version": "2",
      },
    },
  );
  expect(status.status).toBe(200);
  return { uuid: silicon.uuid as string, id: silicon.id as string, tokens };
}
