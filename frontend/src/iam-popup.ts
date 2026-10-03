export type IdentityKind = "carbon" | "silicon";
export type LoginStart = { url: string; attempt: string };

/** The callback reports server verification, never application credentials. */
export function openIamPopup(
  kind: IdentityKind,
  start: () => Promise<LoginStart>,
  signal: AbortSignal,
): Promise<string> {
  if (signal.aborted) return Promise.reject(new Error("Sign-in cancelled."));
  const popup = window.open("about:blank", `remind-sign-in-${crypto.randomUUID()}`, "popup,width=520,height=720");
  if (!popup) return Promise.reject(new Error("The sign-in popup was blocked. Continue in this tab below."));
  return new Promise((resolve, reject) => {
    let attempt: string | undefined;
    let settled = false;
    const cleanup = () => {
      settled = true;
      window.removeEventListener("message", receive);
      signal.removeEventListener("abort", cancel);
      clearInterval(closed); clearTimeout(timeout);
      popup.close();
    };
    const fail = (error: unknown) => { if (!settled) { cleanup(); reject(error); } };
    const cancel = () => fail(new Error("Sign-in cancelled."));
    const receive = (event: MessageEvent) => {
      if (settled || !attempt || event.origin !== location.origin || event.source !== popup || event.data?.type !== "remind:sign-in" || event.data.attempt !== attempt || event.data.kind !== kind) return;
      if (event.data.ok !== true || typeof event.data.contextId !== "string" || !/^[a-f0-9]{32}$/.test(event.data.contextId)) {
        fail(new Error("Sign-in did not finish. Please try again.")); return;
      }
      cleanup(); resolve(event.data.contextId);
    };
    const closed = setInterval(() => { if (popup.closed) cancel(); }, 500);
    const timeout = setTimeout(() => fail(new Error("Sign-in timed out. Please try again.")), 600000);
    window.addEventListener("message", receive);
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) { cancel(); return; }
    void start().then(result => {
      if (settled) return;
      if (!/^[a-f0-9]{64}$/.test(result.attempt)) throw new Error("The sign-in attempt could not be verified.");
      const url = new URL(result.url);
      if (url.protocol !== "https:" || url.username || url.password || url.searchParams.get("identity_kind") !== kind || url.searchParams.get("display") !== "popup") throw new Error("The sign-in destination could not be verified.");
      attempt = result.attempt;
      popup.location.href = url.href; popup.focus();
    }).catch(fail);
  });
}
