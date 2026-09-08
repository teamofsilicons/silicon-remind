import { For, Show, onMount, onCleanup, type JSX } from "solid-js";
export function Badge(p: { value: string }) {
  return <span class={"badge " + p.value}>{p.value.replaceAll("_", " ")}</span>;
}
export function Panel(p: {
  title: string;
  action?: JSX.Element;
  children: JSX.Element;
}) {
  return (
    <section class="panel">
      <div class="panel-head">
        <h2>{p.title}</h2>
        {p.action}
      </div>
      {p.children}
    </section>
  );
}
export function Empty(p: { title: string; detail?: string }) {
  return (
    <div class="empty">
      <span class="empty-icon" aria-hidden="true">
        ◷
      </span>
      <h3>{p.title}</h3>
      <p>{p.detail}</p>
    </div>
  );
}
export interface Field {
  name: string;
  label: string;
  type?: string;
  value?: string;
  placeholder?: string;
  required?: boolean;
  hint?: string;
  options?: { value: string; label: string }[];
}
export interface DialogSpec {
  title: string;
  description?: string;
  fields?: Field[];
  submit?: string;
  danger?: boolean;
  run: (values: Record<string, string>) => Promise<void>;
}
export function Modal(p: {
  spec: DialogSpec;
  busy: boolean;
  error: string;
  close: () => void;
  submit: (v: Record<string, string>) => void;
}) {
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  onCleanup(() => dialog.close());
  return (
    <dialog
      ref={dialog}
      onCancel={(e) => {
        e.preventDefault();
        if (!p.busy) p.close();
      }}
      aria-labelledby="dialog-title"
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          p.submit(
            Object.fromEntries(new FormData(e.currentTarget)) as Record<
              string,
              string
            >,
          );
        }}
      >
        <header>
          <h2 id="dialog-title">{p.spec.title}</h2>
          <button
            type="button"
            class="icon-button"
            aria-label="Close dialog"
            disabled={p.busy}
            onClick={p.close}
          >
            ×
          </button>
        </header>
        <Show when={p.spec.description}>
          <p class="muted">{p.spec.description}</p>
        </Show>
        <div class="fields">
          <For each={p.spec.fields}>
            {(f) => (
              <label>
                {f.label}
                <Show
                  when={f.type === "textarea"}
                  fallback={
                    <Show
                      when={f.options}
                      fallback={
                        <input
                          name={f.name}
                          type={f.type || "text"}
                          value={f.value || ""}
                          required={f.required}
                          placeholder={f.placeholder}
                          autocomplete={
                            f.type === "password" ? "off" : undefined
                          }
                        />
                      }
                    >
                      <select
                        name={f.name}
                        value={f.value}
                        required={f.required}
                      >
                        <For each={f.options}>
                          {(o) => <option value={o.value}>{o.label}</option>}
                        </For>
                      </select>
                    </Show>
                  }
                >
                  <textarea
                    name={f.name}
                    rows={5}
                    required={f.required}
                    value={f.value || ""}
                    placeholder={f.placeholder}
                  />
                </Show>
                <Show when={f.hint}>
                  <span class="hint">{f.hint}</span>
                </Show>
              </label>
            )}
          </For>
        </div>
        <Show when={p.error}>
          <p class="error" role="alert">
            {p.error}
          </p>
        </Show>
        <footer>
          <button
            type="button"
            class="secondary"
            disabled={p.busy}
            onClick={p.close}
          >
            Cancel
          </button>
          <button
            type="submit"
            class={p.spec.danger ? "danger" : "primary"}
            disabled={p.busy}
          >
            {p.busy ? "Working…" : p.spec.submit || "Save changes"}
          </button>
        </footer>
      </form>
    </dialog>
  );
}
