import { type NextRequest, NextResponse } from "next/server";
import { serverEnv } from "@/lib/server/env";
import { freshSession } from "@/lib/server/fresh";
import {
  clientHeaders,
  errorBody,
  readSession,
  sameOriginProblem,
  writeSession,
  clearSession,
} from "@/lib/server/session";
import {
  selectedEnvironment,
  writeEnvironment,
} from "@/lib/server/environment";
export const dynamic = "force-dynamic";
async function handle(request: NextRequest) {
  const problem = sameOriginProblem(request);
  if (problem)
    return NextResponse.json(
      errorBody("cross_site_request", problem, "Use Remind’s own pages."),
      { status: 403 },
    );
  const state = await freshSession(
    readSession(request),
    "api",
    clientHeaders(request),
  );
  if (state.kind === "signed_out") {
    const response = NextResponse.json(errorBody("signed_out", state.message, "Sign in again."), { status: 401 });
    if (state.clear) clearSession(response);
    return response;
  }
  if (state.kind === "unavailable") return NextResponse.json(errorBody(state.code, state.message, "Wait a moment and retry."), { status: state.status });
  const finish = (value: unknown, status = 200) => {
    const response = NextResponse.json(value, {
      status,
      headers: { "Cache-Control": "private, no-store" },
    });
    if (state.rotated) writeSession(response, state.session);
    return response;
  };
  if (request.method === "GET") {
    const current = selectedEnvironment(request, state.session.acct.uuid);
    return finish({
      environment: current ? { id: current.id, name: current.name } : null,
    });
  }
  let id: unknown;
  try {
    id = (await request.json()).id;
  } catch {
    return finish(
      errorBody(
        "invalid_request",
        "Choose a test environment or Production.",
        "Try again.",
      ),
      400,
    );
  }
  if (id === null) {
    const response = finish({ environment: null });
    writeEnvironment(response, null);
    return response;
  }
  if (typeof id !== "string" || !/^[0-9a-f-]{36}$/.test(id))
    return finish(
      errorBody(
        "invalid_request",
        "The environment id is invalid.",
        "Choose an environment from the list.",
      ),
      400,
    );
  const env = serverEnv();
  const headers = {
    Authorization: `Bearer ${state.session.at}`,
    "X-Remind-API-Version": "2",
  };
  try {
    const [metadata, key] = await Promise.all([
      fetch(`${env.appApiUrl}/test-environments/${id}`, {
        headers,
        cache: "no-store",
        signal: AbortSignal.timeout(15000),
      }),
      fetch(`${env.appApiUrl}/test-environments/${id}/key`, {
        headers,
        cache: "no-store",
        signal: AbortSignal.timeout(15000),
      }),
    ]);
    for (const result of [metadata, key]) {
      if (!result.ok) {
        const response = new NextResponse(await result.text(), {
          status: result.status,
          headers: {
            "Content-Type": "application/json",
            "Cache-Control": "private, no-store",
          },
        });
        if (state.rotated) writeSession(response, state.session);
        return response;
      }
    }
    const environment = await metadata.json();
    const secret = await key.json();
    if (
      typeof environment.name !== "string" ||
      !/^[A-Za-z0-9]{32}$/.test(secret.key)
    )
      throw new Error("Invalid environment response");
    const response = finish({ environment: { id, name: environment.name } });
    writeEnvironment(response, {
      v: 1,
      account: state.session.acct.uuid,
      id,
      name: environment.name,
      key: secret.key,
    });
    return response;
  } catch {
    return finish(
      errorBody(
        "service_unreachable",
        "The environment could not be selected.",
        "Wait a moment and retry.",
      ),
      502,
    );
  }
}
export { handle as GET, handle as POST };
