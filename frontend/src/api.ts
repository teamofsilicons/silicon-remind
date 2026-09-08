export class ApiError extends Error {
  constructor(
    message: string,
    public code?: string,
    public requestId?: string,
    public status?: number,
  ) {
    super(message);
  }
}
const pending = new Map<string, string>();
export async function request<T>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<T> {
  const signature = method + path + JSON.stringify(body);
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
