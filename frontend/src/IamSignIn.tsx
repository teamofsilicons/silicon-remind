import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { ArcButton } from "./arc";
import { request } from "./api";
import { openIamPopup, type IdentityKind, type LoginStart } from "./iam-popup";

export function IamSignIn(props: { close: () => void; signedIn: (kind: IdentityKind, contextId: string) => Promise<void> }) {
  let dialog!: HTMLDialogElement;
  let controller: AbortController | undefined;
  let generation = 0;
  const [busy, setBusy] = createSignal<IdentityKind>();
  const [error, setError] = createSignal("");
  onMount(() => dialog.showModal());
  onCleanup(() => { generation++; controller?.abort(); });
  async function signIn(kind: IdentityKind, popup: boolean) {
    const current = ++generation;
    controller?.abort();
    controller = new AbortController();
    setBusy(kind); setError("");
    const start = () => request<LoginStart>("auth/start", "POST", { identity_kind: kind, display: popup ? "popup" : "page" });
    try {
      if (popup) {
        const contextId = await openIamPopup(kind, start, controller.signal);
        if (current !== generation) return;
        await props.signedIn(kind, contextId);
        props.close();
      } else {
        const result = await start();
        if (current !== generation) return;
        window.location.assign(result.url);
      }
    } catch (e) {
      if (current === generation) setError(e instanceof Error ? e.message : "Sign-in could not be completed.");
    } finally { if (current === generation) setBusy(undefined); }
  }
  return <dialog ref={dialog} aria-labelledby="iam-sign-in-title" onCancel={props.close}>
    <div class="iam-sign-in">
      <header><h2 id="iam-sign-in-title">Sign in to Remind</h2><ArcButton class="icon-button" aria-label="Close sign-in" onClick={props.close}>×</ArcButton></header>
      <p class="muted">Choose your account type, then select one organization in IAM.</p>
      <div class="actions"><ArcButton class="primary" disabled={!!busy()} onClick={() => void signIn("carbon", true)}>Continue as Carbon</ArcButton><ArcButton class="secondary" disabled={!!busy()} onClick={() => void signIn("silicon", true)}>Continue as Silicon</ArcButton></div>
      <Show when={busy()}><p role="status">Complete sign-in in the IAM window, or continue in this tab.</p></Show>
      <Show when={error()}><p class="error" role="alert">{error()}</p></Show>
      <p class="muted">Prefer this tab?</p>
      <div class="actions"><ArcButton class="text-button" onClick={() => void signIn("carbon", false)}>Carbon sign-in in this tab</ArcButton><ArcButton class="text-button" onClick={() => void signIn("silicon", false)}>Silicon sign-in in this tab</ArcButton></div>
    </div>
  </dialog>;
}
