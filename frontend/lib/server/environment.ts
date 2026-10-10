import type { NextRequest, NextResponse } from "next/server";
import { secureCookies } from "./env";
import { seal, unseal } from "./seal";
export interface SelectedEnvironment {
  v: 1;
  account: string;
  id: string;
  name: string;
  key: string;
}
export const environmentCookieName = () =>
  `${secureCookies() ? "__Host-" : ""}remind_environment`;
export function selectedEnvironment(
  request: NextRequest,
  account: string,
): SelectedEnvironment | null {
  const value = unseal<SelectedEnvironment>(
    request.cookies.get(environmentCookieName())?.value,
    "remind-environment",
  );
  return value?.v === 1 &&
    value.account === account &&
    typeof value.id === "string" &&
    typeof value.name === "string" &&
    /^[A-Za-z0-9]{32}$/.test(value.key)
    ? value
    : null;
}
export function writeEnvironment(
  response: NextResponse,
  value: SelectedEnvironment | null,
) {
  response.cookies.set(
    environmentCookieName(),
    value ? seal(value, "remind-environment") : "",
    {
      httpOnly: true,
      secure: secureCookies(),
      sameSite: "lax",
      path: "/",
      maxAge: value ? 86400 : 0,
    },
  );
}
