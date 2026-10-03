export class ApiError extends Error {
  code?: string;
  requestId?: string;
  status?: number;
  constructor(message: string, code?: string, requestId?: string, status?: number) {
    super(message);
    this.code = code;
    this.requestId = requestId;
    this.status = status;
  }
}
const pending = new Map<string, string>();
let accountContext = "signed-out";
let productionContext = "signed-out";
let environmentContext = "production";
let selectionGeneration = 0;
const selectionActions = new Set(["login", "logout", "account", "context", "organization"]);
export async function request<T>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<T> {
  const capturedContext = accountContext;
  const capturedProduction = productionContext;
  const capturedEnvironment = environmentContext;
  const generation = selectionActions.has(path) ? ++selectionGeneration : selectionGeneration;
  const signature = JSON.stringify([capturedEnvironment, capturedContext, capturedProduction, method, path, body]);
  let mutation = pending.get(signature);
  if (method !== "GET") {
    mutation ||= crypto.randomUUID();
    pending.set(signature, mutation);
  }
  let response: Response;
  try {
    response = await fetch("/ui/" + path, {
      method,
      headers: {
        "Content-Type": "application/json",
        "X-Remind-UI": "1",
        "X-Remind-Account": capturedContext,
        "X-Remind-Production-Account": capturedProduction,
        "X-Remind-Context": capturedEnvironment,
        "X-Remind-Telemetry": localStorage.getItem("remind.telemetry") === "off" ? "off" : "on",
        ...(mutation ? { "Idempotency-Key": mutation } : {}),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(
      "Connection interrupted. Retry the same action to recover its result.",
    );
  }
  let result: any = null;
  try {
    result = response.status === 204 ? null : await response.json();
  } catch {
    throw new ApiError(
      "The server returned an unexpected response. Please retry.",
    );
  }
  if (response.ok) {
    if (generation !== selectionGeneration || (path.startsWith("api") && (capturedContext !== accountContext || capturedProduction !== productionContext || capturedEnvironment !== environmentContext)))
      throw new ApiError("Account changed while loading. Reload this view.", undefined, undefined, 409);
    if (["session", "login", "logout", "account", "context", "organization"].includes(path) && result) {
      const nextAccount = result.activeAccount || "signed-out";
      const nextProduction = result.productionAccount || "signed-out";
      const nextEnvironment = result.active || "production";
      if (accountContext !== nextAccount || productionContext !== nextProduction || environmentContext !== nextEnvironment) selectionGeneration++;
      accountContext = nextAccount;
      productionContext = nextProduction;
      environmentContext = nextEnvironment;
    }
    pending.delete(signature);
    return result;
  }
  if (response.status < 500 && response.status !== 429)
    pending.delete(signature);
  throw new ApiError(
    result?.error?.message || "Request failed",
    result?.error?.code,
    result?.error?.request_id,
    response.status,
  );
}
export const api = <T>(path: string, method = "GET", body?: unknown) =>
  request<T>("api" + path, method, body);
export function query(values: Record<string, string | undefined>) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(values)) if (v) q.set(k, v);
  return "?" + q;
}
export const date = (value: string | null | undefined) =>
  value
    ? new Date(value).toLocaleString(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
      })
    : "—";
