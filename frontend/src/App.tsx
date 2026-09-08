import {
  createSignal,
  createResource,
  For,
  Show,
  onMount,
  onCleanup,
} from "solid-js";
import { api, request, query, date, ApiError } from "./api";
import { Badge, Panel, Empty, Modal, type DialogSpec, type Field } from "./ui";
import type {
  Session,
  Schedule,
  Execution,
  Environment,
  Silicon,
  Page,
} from "./types";
type View =
  | "reminders"
  | "archive"
  | "silicons"
  | "webhook"
  | "testing"
  | "settings";
const navigation: { id: View; label: string; icon: string }[] = [
  { id: "reminders", label: "Reminders", icon: "◷" },
  { id: "archive", label: "Archive", icon: "▤" },
  { id: "silicons", label: "Silicons", icon: "⌘" },
  { id: "webhook", label: "Webhooks", icon: "↗" },
  { id: "testing", label: "Testing environments", icon: "◇" },
  { id: "settings", label: "Settings", icon: "⚙" },
];
const initialView = () => {
  const v = location.hash.slice(1) as View;
  return navigation.some((n) => n.id === v) ? v : "reminders";
};
export default function App() {
  const [session, { refetch: reloadSession }] = createResource(() =>
    request<Session>("session"),
  );
  const [view, setView] = createSignal<View>(initialView()),
    [revision, refresh] = createSignal(0),
    [dialog, setDialog] = createSignal<DialogSpec>(),
    [busy, setBusy] = createSignal(false),
    [dialogError, setDialogError] = createSignal(""),
    [notice, setNotice] = createSignal(""),
    [globalError, setGlobalError] = createSignal(""),
    [mobile, setMobile] = createSignal(false);
  const [status, setStatus] = createSignal(""),
    [silicon, setSilicon] = createSignal(""),
    [cursor, setCursor] = createSignal(""),
    [history, setHistory] = createSignal<string[]>([]),
    [selected, setSelected] = createSignal<string[]>([]),
    [detailId, setDetailId] = createSignal(""),
    [execCursor, setExecCursor] = createSignal(""),
    [execHistory, setExecHistory] = createSignal<string[]>([]),
    [includeDeleted, setIncludeDeleted] = createSignal(false),
    [secret, setSecret] = createSignal("");
  const active = () =>
      (session.error ? undefined : session())?.active || "production",
    identity = () => (session.error ? undefined : session())?.identity,
    context = () =>
      (session.error ? undefined : session())?.contexts.find(
        (c) => c.id === active(),
      ),
    test = () => active() !== "production";
  const readError = (e: unknown) =>
    e instanceof ApiError
      ? e.message + (e.requestId ? " · Request " + e.requestId : "")
      : e instanceof Error
        ? e.message
        : "Could not complete this action.";
  const owner = (r: Schedule) =>
    !!identity()?.can_manage_reminders &&
    identity()?.public_id === r.silicon_id &&
    r.section === "current";
  const manager = (e: Environment) => {
    const i = (session.error ? undefined : session())?.productionIdentity;
    return (
      !!i &&
      (i.principal_id === e.creator_id ||
        ["owner", "admin"].includes(i.org_role))
    );
  };
  function navigate(v: View) {
    location.hash = v;
    setView(v);
    setCursor("");
    setHistory([]);
    setSelected([]);
    setDetailId("");
    setMobile(false);
    setGlobalError("");
  }
  onMount(() => {
    if (new URL(location.href).searchParams.has("login_error")) {
      setGlobalError(
        "IAm sign-in could not be completed. Please continue with IAm again.",
      );
      window.history.replaceState(null, "", location.pathname + location.hash);
    }
    const changed = () => navigate(initialView());
    window.addEventListener("hashchange", changed);
    onCleanup(() => window.removeEventListener("hashchange", changed));
  });
  function invalidate() {
    refresh((x) => x + 1);
  }
  async function perform(fn: () => Promise<void>) {
    setGlobalError("");
    try {
      await fn();
    } catch (e) {
      setGlobalError(readError(e));
    }
  }
  function open(spec: DialogSpec) {
    setDialogError("");
    setDialog(spec);
  }
  async function submit(values: Record<string, string>) {
    setBusy(true);
    setDialogError("");
    try {
      await dialog()!.run(values);
      setDialog(undefined);
      invalidate();
      await reloadSession();
    } catch (e) {
      setDialogError(readError(e));
    } finally {
      setBusy(false);
    }
  }
  function confirm(
    title: string,
    description: string,
    run: () => Promise<void>,
    label: string,
    danger = false,
  ) {
    open({ title, description, submit: label, danger, run });
  }
  const [signingIn, setSigningIn] = createSignal(false);
  async function continueWithIam() {
    if (signingIn()) return;
    setSigningIn(true);
    try {
      const result = await request<{ url: string }>("auth/start", "POST", {});
      window.location.assign(result.url);
    } catch (e) {
      setSigningIn(false);
      throw e;
    }
  }
  function login() {
    if (test()) return tokenLogin();
    void perform(continueWithIam);
  }
  function changeOrganization() {
    login();
  }
  function tokenLogin() {
    open({
      title: test() ? "Sign in to " + context()?.name : "Sign in to Remind",
      description:
        "Use a short-lived token from Silicon IAm for tos>remind. Choose your organizations in IAm." +
        (test()
          ? " The token must come from this environment’s linked IAm sandbox."
          : ""),
      submit: "Sign in",
      fields: [
        {
          name: "slt",
          label: "IAm short-lived token",
          type: "password",
          required: true,
          placeholder: "Paste your short-lived token",
        },
      ],
      run: async (v) => {
        await request("login", "POST", v);
        setNotice("Signed in.");
      },
    });
  }
  async function switchContext(id: string) {
    await request("context", "POST", { id });
    await reloadSession();
    setDetailId("");
    setSelected([]);
    setCursor("");
    setHistory([]);
    setSilicon("");
    setStatus("");
    invalidate();
  }
  function importEnvironment() {
    open({
      title: "Import test environment",
      description:
        "A Remind test key gives access to its sandbox. Sign in with a test IAm identity to use reminders.",
      submit: "Import environment",
      fields: [
        {
          name: "id",
          label: "Environment ID",
          placeholder: "Optional — verified against the key",
        },
        {
          name: "key",
          label: "Remind test key",
          type: "password",
          required: true,
        },
      ],
      run: async (v) => {
        await request("context", "POST", { action: "import", ...v });
        setDetailId("");
        setSelected([]);
        setCursor("");
        setNotice("Environment imported. Sign in with a test identity.");
      },
    });
  }
  const source = () => ({
    view: view(),
    scope: active(),
    user: identity()?.principal_id,
    production: (session.error ? undefined : session())?.productionIdentity
      ?.principal_id,
    revision: revision(),
    cursor: cursor(),
    status: status(),
    silicon: silicon(),
    deleted: includeDeleted(),
  });
  const [rows] = createResource(source, async (s) => {
    if (["reminders", "archive"].includes(s.view) && s.user)
      return api<Page<Schedule>>(
        "/schedules" +
          query({
            section: s.view === "archive" ? "archived" : "current",
            status: s.status,
            silicon_id: s.silicon,
            cursor: s.cursor,
            limit: "25",
          }),
      );
    if (s.view === "silicons" && s.user)
      return api<Page<Silicon>>(
        "/silicons" + query({ after: s.cursor, limit: "25" }),
      );
    if (s.view === "testing" && s.production)
      return api<Page<Environment>>(
        "/test-environments" +
          query({
            include_deleted: String(s.deleted),
            after: s.cursor,
            limit: "25",
          }),
      );
    return { items: [], next_cursor: null };
  });
  const [detail] = createResource(
    () => ({ id: detailId(), scope: active(), revision: revision() }),
    async (s) => (s.id ? api<Schedule>("/schedules/" + s.id) : null),
  );
  const [executions] = createResource(
    () => ({
      id: detailId(),
      scope: active(),
      cursor: execCursor(),
      revision: revision(),
    }),
    async (s) =>
      s.id
        ? api<Page<Execution>>(
            "/schedules/" +
              s.id +
              "/executions" +
              query({ cursor: s.cursor, limit: "10" }),
          )
        : null,
  );
  const [destinations] = createResource(
    () => ({
      enabled: view() === "webhook" && !!identity()?.can_manage_reminders,
      scope: active(),
      revision: revision(),
    }),
    async (s) => {
      if (!s.enabled) return null;
      try {
        return await api<{
          items: Array<{
            id: string;
            endpoint_url: string;
            version: number;
            updated_at: string;
          }>;
        }>("/webhooks");
      } catch (e) {
        if (e instanceof ApiError && e.code === "webhook_not_configured")
          return { items: [] };
        throw e;
      }
    },
  );
  const [testInfo] = createResource(
    () => ({
      enabled: test() && (view() === "testing" || view() === "settings"),
      id: active(),
      revision: revision(),
    }),
    async (s) => (s.enabled ? api<Environment>("/testing-environment") : null),
  );
  const [health] = createResource(
    () => ({ enabled: view() === "settings", revision: revision() }),
    async (s) =>
      s.enabled
        ? api<{ status: string; version: string }>("/health/ready")
        : null,
  );
  function resetList() {
    setCursor("");
    setHistory([]);
    setSelected([]);
  }
  function next() {
    const n = rows()?.next_cursor;
    if (n) {
      setHistory((h) => [...h, cursor()]);
      setCursor(n);
      setSelected([]);
    }
  }
  function previous() {
    const h = [...history()];
    setCursor(h.pop() || "");
    setHistory(h);
    setSelected([]);
  }
  function selectDetail(id: string) {
    setDetailId(id);
    setExecCursor("");
    setExecHistory([]);
  }
  const scheduleFields = (r?: Schedule): Field[] => [
    {
      name: "text",
      label: "Reminder",
      type: "textarea",
      required: true,
      value: r?.text,
      placeholder: "What should this Silicon be reminded about?",
    },
    {
      name: "kind",
      label: "Repeat",
      value: r?.kind || "recurring",
      options: [
        { value: "recurring", label: "Recurring" },
        { value: "one_time", label: "One time" },
      ],
    },
    {
      name: "cron",
      label: "Schedule",
      required: true,
      value: r?.cron || "0 9 * * *",
      hint: "Five-field cron: minute · hour · day · month · weekday. One time uses the next match.",
    },
    {
      name: "timezone",
      label: "Timezone",
      required: true,
      value: r?.timezone || "UTC",
      hint: "An IANA timezone, for example Asia/Kolkata or America/New_York.",
    },
  ];
  function edit(r?: Schedule) {
    open({
      title: r ? "Edit reminder" : "New reminder",
      description: r
        ? undefined
        : "The reminder will be delivered to every subscribed webhook endpoint when one is configured.",
      submit: r ? "Save reminder" : "Create reminder",
      fields: scheduleFields(r),
      run: async (v) => {
        if (new TextEncoder().encode(v.text).length > 100000)
          throw Error("Reminder text must be at most 100,000 UTF-8 bytes.");
        const body = r
          ? Object.fromEntries(
              Object.entries(v).filter(
                ([k, value]) => r[k as keyof Schedule] !== value,
              ),
            )
          : v;
        if (r && !Object.keys(body).length) return;
        const result = await api<Schedule>(
          r ? "/schedules/" + r.id : "/schedules",
          r ? "PATCH" : "POST",
          body,
        );
        selectDetail(result.id);
        setNotice(r ? "Reminder updated." : "Reminder created.");
      },
    });
  }
  async function statusChange(ids: string[], newStatus: string) {
    await api("/schedules", "PATCH", { schedule_ids: ids, status: newStatus });
    setSelected([]);
    invalidate();
    setNotice(
      ids.length +
        " reminder" +
        (ids.length === 1 ? "" : "s") +
        (newStatus === "paused" ? " paused." : " resumed."),
    );
  }
  function archive(r: Schedule) {
    confirm(
      "Archive reminder?",
      "This reminder will remain readable for 45 days, then be permanently deleted. Future occurrences will stop.",
      async () => {
        await api("/schedules/" + r.id, "DELETE");
        setDetailId("");
        setNotice("Reminder archived.");
      },
      "Archive reminder",
    );
  }
  function setWebhook() {
    open({
      title: "Add webhook subscription",
      description:
        "Use any HTTP(S) endpoint that should receive reminder events." +
        (test() ? " You can use a sandbox endpoint for this environment." : ""),
      fields: [
        {
          name: "endpoint_url",
          label: "Endpoint URL",
          type: "url",
          required: true,
        },
        {
          name: "signing_secret",
          label: "Signing secret (optional)",
          type: "password",
          required: false,
        },
      ],
      run: async (v) => {
        await api("/webhooks", "POST", v);
        setNotice("Webhook subscription added.");
      },
    });
  }
  function newEnvironment() {
    open({
      title: "Create test environment",
      description:
        "An empty Remind replica linked to an IAm test environment. Limited to 100 retained reminders.",
      submit: "Create environment",
      fields: [
        { name: "name", label: "Name", required: true },
        { name: "description", label: "Description", type: "textarea" },
        {
          name: "iam_test_key",
          label: "IAm test key",
          type: "password",
          required: true,
        },
        {
          name: "iam_app_secret",
          label: "IAm test app secret",
          type: "password",
          hint: "Optional now. Configure it before signing in to this sandbox.",
        },
      ],
      run: async (v) => {
        const result = await api<{ environment: Environment; key: string }>(
          "/test-environments",
          "POST",
          {
            ...v,
            iam_app_secret: v.iam_app_secret || undefined,
            description: v.description || undefined,
          },
        );
        setSecret(result.key);
        setNotice(
          result.environment.name +
            " created. Its key is also saved in this browser session.",
        );
      },
    });
  }
  async function environmentKey(e: Environment, action = "key") {
    const result = await api<{ key: string }>(
      "/test-environments/" + e.id + "/" + action,
      action === "key" ? "GET" : "POST",
      action === "key" ? undefined : {},
    );
    setSecret(result.key);
    await reloadSession();
    invalidate();
  }
  function configureIam() {
    open({
      title: "Configure test IAm app",
      description:
        "Use the tos>remind app secret imported into this environment’s IAm sandbox. Production credentials cannot be used here.",
      fields: [
        {
          name: "iam_app_secret",
          label: "Test app secret",
          type: "password",
          required: true,
        },
      ],
      run: async (v) => {
        await api("/testing-environment/iam", "PUT", v);
        setNotice("Test IAm app configured.");
      },
    });
  }
  const title = () => navigation.find((n) => n.id === view())!.label;
  const needsLogin = () =>
    view() === "testing"
      ? !(session.error ? undefined : session())?.productionIdentity
      : !identity() && !["settings"].includes(view());
  return (
    <div class="app-shell">
      <a class="skip" href="#main">
        Skip to content
      </a>
      <aside id="workspace-navigation" classList={{ sidebar: true, open: mobile() }}>
        <a class="brand" href="#reminders">
          <img src="/brand/mark.svg" alt="" />
          silicon <span>REMIND</span>
        </a>
        <div class="context">
          <label>
            WORKSPACE
            <select
              aria-label="Environment"
              value={active()}
              disabled={session.loading}
              onChange={(e) =>
                void perform(() => switchContext(e.currentTarget.value))
              }
            >
              <For
                each={
                  (session.error ? undefined : session())?.contexts || [
                    { id: "production", name: "Production" },
                  ]
                }
              >
                {(c) => (
                  <option value={c.id} selected={c.id === active()}>
                    {c.name}
                  </option>
                )}
              </For>
            </select>
          </label>
          <Show when={context()?.org}>
            <label>
              Organization
              <select aria-label="Organization" value={context()?.org}
                onChange={(e) => {
                  const org = e.currentTarget.value;
                  void perform(async () => {
                    await request("organization", "POST", { org });
                    await switchContext(active());
                  });
                }}>
                <For each={context()?.organizations || []}>
                  {(org) => <option value={org}>{org}</option>}
                </For>
              </select>
            </label>
          </Show>
        </div>
        <nav aria-label="Main navigation">
          <For each={navigation}>
            {(n) => (
              <a
                href={"#" + n.id}
                aria-current={view() === n.id ? "page" : undefined}
                classList={{ active: view() === n.id }}
                onClick={() => navigate(n.id)}
              >
                <span aria-hidden="true">{n.icon}</span>
                {n.label}
              </a>
            )}
          </For>
        </nav>
        <div class="sidebar-foot">
          <a
            href="https://iam.teamofsilicons.com"
            target="_blank"
            rel="noreferrer"
          >
            Silicon IAm ↗
          </a>
          <div class="person">
            <span class="avatar">
              {identity()?.public_id?.slice(0, 1).toUpperCase() || "S"}
            </span>
            <div>
              <strong>{identity()?.public_id || "Not signed in"}</strong>
              <span>
                {identity()
                  ? identity()!.actor_type + " · " + identity()!.org_id
                  : "Connect with IAm"}
              </span>
            </div>
          </div>
        </div>
      </aside>
      <div class="workspace">
        <header class="topbar">
          <button
            class="icon-button mobile-menu"
            aria-label="Toggle navigation"
            aria-expanded={mobile()}
            aria-controls="workspace-navigation"
            onClick={() => setMobile(!mobile())}
          >
            ☰
          </button>
          <span>
            Silicon / Remind{" "}
            <span class={"plane " + (test() ? "testing" : "")}>
              {test() ? context()?.name : "Production"}
            </span>
          </span>
          <div class="actions">
            <Show
              when={identity()}
              fallback={
                <button
                  class="text-button"
                  disabled={signingIn()}
                  onClick={login}
                >
                  {test()
                    ? "Sign in to sandbox"
                    : signingIn()
                      ? "Opening IAm…"
                      : "Continue with IAm"}
                </button>
              }
            >
              <button
                class="text-button"
                onClick={() =>
                  confirm(
                    "Sign out?",
                    "Revoke this Remind session. Other environment sessions remain available.",
                    async () => {
                      await request("logout", "POST", {});
                      setDetailId("");
                      setNotice("Signed out.");
                    },
                    "Sign out",
                  )
                }
              >
                Sign out
              </button>
            </Show>
          </div>
        </header>
        <main id="main">
          <Show when={test()}>
            <div class="test-banner">
              <span>◇ Test environment</span>
              <span>Isolated from production · 100-reminder limit</span>
            </div>
          </Show>
          <div class="page-heading">
            <div>
              <p class="eyebrow">WORKSPACE</p>
              <h1>{title()}</h1>
              <p class="muted">
                {
                  {
                    reminders: "The right reminder, at the right time.",
                    archive: "Past reminders, retained for 45 days.",
                    silicons: "Reminders across your organization.",
                    webhook: "Optional receivers for your reminders.",
                    testing: "A separate space to try everything.",
                    settings: "Your session and service connection.",
                  }[view()]
                }
              </p>
            </div>
            <div class="actions">
              <Show
                when={
                  view() === "reminders" && identity()?.can_manage_reminders
                }
              >
                <button class="primary" onClick={() => edit()}>
                  ＋ New reminder
                </button>
              </Show>
              <Show when={view() === "testing"}>
                <button class="secondary" onClick={importEnvironment}>
                  Import environment
                </button>
                <Show
                  when={
                    (session.error ? undefined : session())?.productionIdentity
                  }
                >
                  <button class="primary" onClick={newEnvironment}>
                    ＋ Create environment
                  </button>
                </Show>
              </Show>
            </div>
          </div>
          <Show when={notice()}>
            <div class="notice" role="status">
              {notice()}
              <button
                class="icon-button"
                aria-label="Dismiss notification"
                onClick={() => setNotice("")}
              >
                ×
              </button>
            </div>
          </Show>
          <Show when={globalError() || session.error}>
            <div class="error" role="alert">
              {globalError() || readError(session.error)}
            </div>
          </Show>
          <Show
            when={!session.loading}
            fallback={
              <div class="loading" role="status">
                Loading workspace…
              </div>
            }
          >
            <Show
              when={!needsLogin()}
              fallback={
                <Panel
                  title={
                    view() === "testing"
                      ? "Production session required"
                      : "Connect your workspace"
                  }
                >
                  <div class="login-panel">
                    <h2>
                      {view() === "testing"
                        ? "Manage your organization’s environments"
                        : "Sign in with Silicon IAm"}
                    </h2>
                    <p class="muted">
                      {view() === "testing"
                        ? "Environment creation and management use your production session. Imported sandboxes can still be used from the workspace selector."
                        : test()
                          ? "Use an IAm test token from this sandbox’s linked environment."
                          : "Continue with your Silicon IAm account to open your workspace. You’ll return here automatically after signing in."}
                    </p>
                    <button
                      class="primary"
                      onClick={() =>
                        void perform(async () => {
                          if (view() === "testing" && test())
                            await switchContext("production");
                          login();
                        })
                      }
                    >
                      {test()
                        ? "Sign in with a test token"
                        : signingIn()
                          ? "Opening IAm…"
                          : "Continue with IAm"}
                    </button>
                    <button class="text-button" onClick={importEnvironment}>
                      Use a test environment
                    </button>
                  </div>
                </Panel>
              }
            >
              <Show
                when={
                  identity() &&
                  !identity()?.can_manage_reminders &&
                  ["reminders", "archive"].includes(view())
                }
              >
                <p class="permission-note">
                  Read-only access · You can view every Silicon’s reminders in{" "}
                  {identity()?.org_id}.
                </p>
              </Show>
              <Show
                when={["reminders", "archive", "silicons", "testing"].includes(
                  view(),
                )}
              >
                <Panel
                  title={
                    view() === "testing"
                      ? "Organization environments"
                      : view() === "silicons"
                        ? "Registered Silicons"
                        : view() === "archive"
                          ? "Archived reminders"
                          : "Your organization’s reminders"
                  }
                  action={
                    <button
                      class="text-button"
                      onClick={invalidate}
                      disabled={rows.loading}
                    >
                      Refresh
                    </button>
                  }
                >
                  <Show when={["reminders", "archive"].includes(view())}>
                    <div class="filters">
                      <label>
                        Silicon
                        <input
                          placeholder="All Silicons"
                          value={silicon()}
                          onChange={(e) => {
                            setSilicon(e.currentTarget.value);
                            resetList();
                          }}
                        />
                      </label>
                      <label>
                        Status
                        <select
                          value={status()}
                          onChange={(e) => {
                            setStatus(e.currentTarget.value);
                            resetList();
                          }}
                        >
                          <option value="">All statuses</option>
                          <option value="active">Active</option>
                          <option value="paused">Paused</option>
                          <option value="completed">Completed</option>
                        </select>
                      </label>
                      <form
                        class="lookup"
                        onSubmit={(e) => {
                          e.preventDefault();
                          const id = String(
                            new FormData(e.currentTarget).get("id"),
                          );
                          if (!/^[a-f0-9-]{36}$/.test(id)) {
                            setGlobalError("Enter a valid reminder UUID.");
                            return;
                          }
                          selectDetail(id);
                        }}
                      >
                        <label>
                          Find by ID
                          <input
                            name="id"
                            placeholder="Reminder UUID"
                            required
                          />
                        </label>
                        <button class="secondary">Open</button>
                      </form>
                    </div>
                  </Show>
                  <Show when={view() === "testing"}>
                    <div class="filters">
                      <label class="check">
                        <input
                          type="checkbox"
                          checked={includeDeleted()}
                          onChange={(e) => {
                            setIncludeDeleted(e.currentTarget.checked);
                            resetList();
                          }}
                        />
                        Include deleted environments
                      </label>
                      <span class="muted">
                        Owned by{" "}
                        {
                          (session.error ? undefined : session())
                            ?.productionIdentity?.org_id
                        }{" "}
                        · production session
                      </span>
                    </div>
                  </Show>
                  <Show when={selected().length}>
                    <div class="bulk">
                      <strong>{selected().length} selected</strong>
                      <button
                        class="secondary"
                        onClick={() =>
                          confirm(
                            "Pause selected reminders?",
                            `${selected().length} reminders will be paused together.`,
                            () => statusChange(selected(), "paused"),
                            "Pause reminders",
                          )
                        }
                      >
                        Pause
                      </button>
                      <button
                        class="secondary"
                        onClick={() =>
                          confirm(
                            "Resume selected reminders?",
                            `${selected().length} reminders will calculate their next occurrence.`,
                            () => statusChange(selected(), "active"),
                            "Resume reminders",
                          )
                        }
                      >
                        Resume
                      </button>
                      <button
                        class="text-button"
                        onClick={() => setSelected([])}
                      >
                        Clear
                      </button>
                    </div>
                  </Show>
                  <Show
                    when={!rows.loading}
                    fallback={
                      <div class="loading" role="status">
                        Loading…
                      </div>
                    }
                  >
                    <Show
                      when={!rows.error}
                      fallback={
                        <div class="error" role="alert">
                          {readError(rows.error)}{" "}
                          <button onClick={invalidate}>Retry</button>
                        </div>
                      }
                    >
                      <Show
                        when={rows()?.items.length}
                        fallback={
                          <Empty
                            title={
                              view() === "testing"
                                ? "No environments yet"
                                : view() === "silicons"
                                  ? "No Silicons registered"
                                  : "No reminders here"
                            }
                            detail={
                              view() === "reminders"
                                ? identity()?.can_manage_reminders
                                  ? "Create a reminder or change your filters."
                                  : "Reminders created by your organization’s Silicons appear here."
                                : view() === "archive"
                                  ? "Archived reminders will appear here."
                                  : view() === "testing"
                                    ? "Create an environment or import an existing test key."
                                    : "Silicons appear after they sign in to Remind."
                            }
                          />
                        }
                      >
                        <div class="table-scroll">
                          <table>
                            <thead>
                              <tr>
                                <Show
                                  when={["reminders", "archive"].includes(
                                    view(),
                                  )}
                                >
                                  <th class="check-cell">
                                    <Show
                                      when={
                                        view() === "reminders" &&
                                        identity()?.can_manage_reminders
                                      }
                                    >
                                      <input
                                        type="checkbox"
                                        aria-label="Select all owned reminders on this page"
                                        checked={
                                          selected().length > 0 &&
                                          selected().length ===
                                            (
                                              rows()?.items as Schedule[]
                                            ).filter(owner).length
                                        }
                                        onChange={(e) =>
                                          setSelected(
                                            e.currentTarget.checked
                                              ? (rows()?.items as Schedule[])
                                                  .filter(owner)
                                                  .map((r) => r.id)
                                              : [],
                                          )
                                        }
                                      />
                                    </Show>
                                  </th>
                                  <th>Reminder</th>
                                  <th>Silicon</th>
                                  <th>Schedule</th>
                                  <th>Status</th>
                                  <th>
                                    {view() === "archive"
                                      ? "Deletes after"
                                      : "Next occurrence"}
                                  </th>
                                </Show>
                                <Show when={view() === "silicons"}>
                                  <th>Silicon</th>
                                  <th>Reminders</th>
                                  <th />
                                </Show>
                                <Show when={view() === "testing"}>
                                  <th>Environment</th>
                                  <th>Status</th>
                                  <th>Last activity</th>
                                  <th>Actions</th>
                                </Show>
                              </tr>
                            </thead>
                            <tbody>
                              <Show
                                when={["reminders", "archive"].includes(view())}
                              >
                                <For each={rows()?.items as Schedule[]}>
                                  {(r) => (
                                    <tr>
                                      <td class="check-cell">
                                        <Show when={owner(r)}>
                                          <input
                                            type="checkbox"
                                            aria-label={
                                              "Select " + r.text.slice(0, 60)
                                            }
                                            checked={selected().includes(r.id)}
                                            onChange={(e) =>
                                              setSelected((ids) =>
                                                e.currentTarget.checked
                                                  ? [...ids, r.id]
                                                  : ids.filter(
                                                      (id) => id !== r.id,
                                                    ),
                                              )
                                            }
                                          />
                                        </Show>
                                      </td>
                                      <td>
                                        <button
                                          class="row-link reminder-text"
                                          onClick={() => selectDetail(r.id)}
                                        >
                                          {r.text}
                                        </button>
                                        <small>
                                          {r.kind === "one_time"
                                            ? "One time"
                                            : "Recurring"}
                                        </small>
                                      </td>
                                      <td>{r.silicon_id}</td>
                                      <td>
                                        <code>{r.cron}</code>
                                        <small>{r.timezone}</small>
                                      </td>
                                      <td>
                                        <Badge value={r.status} />
                                      </td>
                                      <td>
                                        {date(
                                          view() === "archive"
                                            ? r.purge_after
                                            : r.next_run_at,
                                        )}
                                        <small>Local time</small>
                                      </td>
                                    </tr>
                                  )}
                                </For>
                              </Show>
                              <Show when={view() === "silicons"}>
                                <For each={rows()?.items as Silicon[]}>
                                  {(s) => (
                                    <tr>
                                      <td>
                                        <strong>{s.silicon_id}</strong>
                                        <small class="mono">
                                          {s.principal_id}
                                        </small>
                                      </td>
                                      <td>{s.reminder_count}</td>
                                      <td>
                                        <button
                                          class="text-button"
                                          onClick={() => {
                                            setSilicon(s.silicon_id);
                                            navigate("reminders");
                                          }}
                                        >
                                          View reminders →
                                        </button>
                                      </td>
                                    </tr>
                                  )}
                                </For>
                              </Show>
                              <Show when={view() === "testing"}>
                                <For each={rows()?.items as Environment[]}>
                                  {(e) => (
                                    <tr>
                                      <td>
                                        <strong>{e.name}</strong>
                                        <small>{e.description || e.id}</small>
                                        <small class="mono">{e.id}</small>
                                      </td>
                                      <td>
                                        <Badge
                                          value={
                                            e.deleted_at ? "deleted" : "active"
                                          }
                                        />
                                        <Show when={e.purge_after}>
                                          <small>
                                            Recover before {date(e.purge_after)}
                                          </small>
                                        </Show>
                                      </td>
                                      <td>{date(e.last_activity_at)}</td>
                                      <td>
                                        <div class="actions wrap">
                                          <Show when={!e.deleted_at}>
                                            <Show
                                              when={(session.error
                                                ? undefined
                                                : session()
                                              )?.contexts.some(
                                                (c) => c.id === e.id,
                                              )}
                                            >
                                              <button
                                                class="text-button"
                                                onClick={() =>
                                                  void perform(() =>
                                                    switchContext(e.id),
                                                  )
                                                }
                                              >
                                                Use
                                              </button>
                                            </Show>
                                            <Show when={manager(e)}>
                                              <button
                                                class="text-button"
                                                onClick={() =>
                                                  void perform(() =>
                                                    environmentKey(e),
                                                  )
                                                }
                                              >
                                                Show key
                                              </button>
                                              <button
                                                class="text-button"
                                                onClick={() =>
                                                  confirm(
                                                    "Rotate environment key?",
                                                    "The previous key will immediately stop working. This browser will save the new key.",
                                                    () =>
                                                      environmentKey(
                                                        e,
                                                        "key-rotations",
                                                      ),
                                                    "Rotate key",
                                                  )
                                                }
                                              >
                                                Rotate
                                              </button>
                                              <button
                                                class="text-button danger-text"
                                                onClick={() =>
                                                  confirm(
                                                    "Delete " + e.name + "?",
                                                    "Access stops immediately. This environment can be restored for 30 days.",
                                                    async () => {
                                                      await api(
                                                        "/test-environments/" +
                                                          e.id,
                                                        "DELETE",
                                                      );
                                                      setNotice(
                                                        "Environment deleted.",
                                                      );
                                                    },
                                                    "Delete environment",
                                                    true,
                                                  )
                                                }
                                              >
                                                Delete
                                              </button>
                                            </Show>
                                          </Show>
                                          <Show
                                            when={e.deleted_at && manager(e)}
                                          >
                                            <button
                                              class="text-button"
                                              onClick={() =>
                                                confirm(
                                                  "Restore " + e.name + "?",
                                                  "Restoration generates a fresh key. Previous keys remain revoked.",
                                                  () =>
                                                    environmentKey(
                                                      e,
                                                      "restorations",
                                                    ),
                                                  "Restore environment",
                                                )
                                              }
                                            >
                                              Restore
                                            </button>
                                          </Show>
                                        </div>
                                      </td>
                                    </tr>
                                  )}
                                </For>
                              </Show>
                            </tbody>
                          </table>
                        </div>
                        <div class="pagination">
                          <span>{rows()?.items.length} loaded</span>
                          <button
                            class="secondary"
                            disabled={!history().length}
                            onClick={previous}
                          >
                            Previous
                          </button>
                          <button
                            class="secondary"
                            disabled={!rows()?.next_cursor}
                            onClick={next}
                          >
                            Next
                          </button>
                        </div>
                      </Show>
                    </Show>
                  </Show>
                </Panel>
              </Show>
              <Show when={view() === "webhook"}>
                <Panel
                  title="Webhook subscriptions"
                  action={
                    <Show when={identity()?.can_manage_reminders}>
                      <button class="primary" onClick={setWebhook}>
                        Add webhook
                      </button>
                    </Show>
                  }
                >
                  <Show
                    when={identity()?.can_manage_reminders}
                    fallback={
                      <Empty
                        title="Webhook settings belong to a Silicon"
                        detail="Sign in as a Silicon to add or remove its receivers. Reminders work without a webhook."
                      />
                    }
                  >
                    <Show
                      when={!destinations.loading}
                      fallback={<div class="loading" role="status">Loading webhooks…</div>}
                    >
                      <Show
                        when={!destinations.error}
                        fallback={
                          <div class="error" role="alert">
                            {readError(destinations.error)}
                            {" "}<button class="text-button" onClick={invalidate}>Retry</button>
                          </div>
                        }
                      >
                        <Show
                          when={!destinations.error && destinations()?.items.length ? destinations() : undefined}
                          fallback={
                            <Empty
                              title="No webhook subscriptions"
                              detail="Reminders can run without subscribers. Add a webhook when you want to receive deliveries."
                            />
                          }
                        >
                          {(collection) => (
                            <div class="detail-body">
                              <For each={collection().items}>
                                {(d) => (
                                  <div class="list-row subscription-row">
                                    <div class="subscription-info">
                                      <p class="mono break">{d.endpoint_url}</p>
                                      <small class="mono break">{d.id}</small>
                                      <small>Added {date(d.updated_at)}</small>
                                    </div>
                                    <button
                                      class="secondary danger-text"
                                      onClick={() =>
                                        confirm(
                                          "Remove webhook subscription?",
                                          "This endpoint will stop receiving future reminder deliveries.",
                                          async () => {
                                            await api("/webhooks/" + d.id, "DELETE");
                                            setNotice("Webhook subscription removed.");
                                          },
                                          "Remove subscription",
                                          true,
                                        )
                                      }
                                    >
                                      Remove subscription
                                    </button>
                                  </div>
                                )}
                              </For>
                            </div>
                          )}
                        </Show>
                      </Show>
                    </Show>
                    <p class="panel-note">
                      Each receiver gets the reminder independently. Deliveries can
                      be retried; use the execution ID to ignore duplicates.
                    </p>
                  </Show>
                </Panel>
              </Show>
              <Show when={view() === "settings"}>
                <div class="settings-grid">
                  <Panel title="Session">
                    <div class="detail-body">
                      <dl>
                        <dt>Identity</dt>
                        <dd>{identity()?.public_id || "Not signed in"}</dd>
                        <dt>Organization</dt>
                        <dd>{identity()?.org_id || "—"}</dd>
                        <dt>Access</dt>
                        <dd>
                          {identity()?.can_manage_reminders
                            ? "Manage own reminders"
                            : identity()
                              ? "Read only"
                              : "—"}
                        </dd>
                      </dl>
                      <div class="actions">
                        <button class="secondary" onClick={changeOrganization}>
                          {test() ? "Sign in with a test token" : "Continue with IAm"}
                        </button>
                        <Show when={identity()}>
                          <button
                            class="text-button"
                            onClick={() =>
                              void perform(async () => {
                                await api("/auth/me");
                                await reloadSession();
                                setNotice("Session verified.");
                              })
                            }
                          >
                            Verify session
                          </button>
                        </Show>
                      </div>
                    </div>
                  </Panel>
                  <Panel title="Service">
                    <div class="detail-body">
                      <dl>
                        <dt>Status</dt>
                        <dd>
                          {health.loading
                            ? "Checking…"
                            : health.error
                              ? "Unavailable"
                              : (health.error ? undefined : health())?.status ||
                                "—"}
                        </dd>
                        <dt>Version</dt>
                        <dd>
                          {(health.error ? undefined : health())?.version ||
                            "—"}
                        </dd>
                      </dl>
                      <button class="secondary" onClick={invalidate}>
                        Check readiness
                      </button>
                      <p class="hint">
                        The service connection is configured by the frontend
                        host. Sessions are encrypted on the server.
                      </p>
                    </div>
                  </Panel>
                </div>
              </Show>
            </Show>
          </Show>
          <Show
            when={test() && (view() === "testing" || view() === "settings")}
          >
            <Panel title="Selected test environment">
              <div class="detail-body">
                <Show when={testInfo.error}>
                  <p class="error">{readError(testInfo.error)}</p>
                </Show>
                <dl>
                  <dt>Name</dt>
                  <dd>
                    {(testInfo.error ? undefined : testInfo())?.name ||
                      context()?.name}
                  </dd>
                  <dt>Environment ID</dt>
                  <dd class="mono break">{active()}</dd>
                  <dt>IAm environment</dt>
                  <dd class="mono break">
                    {(testInfo.error ? undefined : testInfo())
                      ?.iam_environment_id || "—"}
                  </dd>
                  <dt>Last activity</dt>
                  <dd>
                    {date(
                      (testInfo.error ? undefined : testInfo())
                        ?.last_activity_at,
                    )}
                  </dd>
                </dl>
                <div class="actions wrap">
                  <button class="secondary" onClick={configureIam}>
                    Configure IAm
                  </button>
                  <button
                    class="secondary danger-text"
                    onClick={() =>
                      open({
                        title: "Clean this environment?",
                        description:
                          "Permanently erase all reminders, executions, webhook settings and logs in this sandbox. Its key and IAm binding remain. Type the environment name to confirm.",
                        submit: "Clean environment",
                        danger: true,
                        fields: [
                          {
                            name: "confirmation",
                            label: "Environment name",
                            required: true,
                          },
                        ],
                        run: async (v) => {
                          if (
                            v.confirmation !==
                            (testInfo.error ? undefined : testInfo())?.name
                          )
                            throw Error("The environment name does not match.");
                          await api(
                            "/testing-environment/cleanings",
                            "POST",
                            {},
                          );
                          setDetailId("");
                          setSelected([]);
                          setNotice(
                            "Environment cleaned. Webhook subscriptions were removed; reminders can be created without one.",
                          );
                        },
                      })
                    }
                  >
                    Clean environment
                  </button>
                  <button
                    class="text-button"
                    onClick={() =>
                      confirm(
                        "Forget this environment?",
                        "Remove its key and session from this browser. The server environment is retained.",
                        async () => {
                          await request("context", "POST", {
                            action: "forget",
                            id: active(),
                          });
                          setNotice("Environment forgotten.");
                        },
                        "Forget environment",
                      )
                    }
                  >
                    Forget
                  </button>
                </div>
                <p class="hint">
                  Inactive environments retire after 15 days, with a further 30
                  days to recover them.
                </p>
              </div>
            </Panel>
          </Show>
          <Show when={detailId()}>
            <Panel
              title="Reminder details"
              action={
                <button class="text-button" onClick={() => setDetailId("")}>
                  Close
                </button>
              }
            >
              <Show
                when={!detail.loading}
                fallback={<div class="loading">Loading reminder…</div>}
              >
                <Show
                  when={!detail.error}
                  fallback={<div class="error">{readError(detail.error)}</div>}
                >
                  <Show when={detail()}>
                    {(r) => (
                      <div class="detail-body">
                        <div class="detail-heading">
                          <h2 class="full-text">{r().text}</h2>
                          <Badge value={r().status} />
                        </div>
                        <dl>
                          <dt>ID</dt>
                          <dd class="mono break">{r().id}</dd>
                          <dt>Silicon</dt>
                          <dd>{r().silicon_id}</dd>
                          <dt>Schedule</dt>
                          <dd>
                            <code>{r().cron}</code> · {r().timezone} ·{" "}
                            {r().kind === "one_time" ? "One time" : "Recurring"}
                          </dd>
                          <dt>Next occurrence</dt>
                          <dd>
                            {date(r().next_run_at)}
                            <Show when={r().next_run_at}>
                              <small class="mono">{r().next_run_at} UTC</small>
                            </Show>
                          </dd>
                          <dt>Created</dt>
                          <dd>{date(r().created_at)}</dd>
                          <dt>Updated</dt>
                          <dd>{date(r().updated_at)}</dd>
                          <Show when={r().archived_at}>
                            <dt>Archived</dt>
                            <dd>{date(r().archived_at)}</dd>
                            <dt>Deletes after</dt>
                            <dd>{date(r().purge_after)}</dd>
                          </Show>
                        </dl>
                        <Show when={owner(r())}>
                          <div class="actions">
                            <button class="secondary" onClick={() => edit(r())}>
                              Edit reminder
                            </button>
                            <button
                              class="secondary"
                              onClick={() =>
                                void perform(() =>
                                  statusChange(
                                    [r().id],
                                    r().status === "paused"
                                      ? "active"
                                      : "paused",
                                  ),
                                )
                              }
                            >
                              {r().status === "paused" ? "Resume" : "Pause"}
                            </button>
                            <button
                              class="text-button"
                              onClick={() => archive(r())}
                            >
                              Archive
                            </button>
                          </div>
                        </Show>
                        <h3 class="section-heading">Execution history</h3>
                        <Show
                          when={!executions.loading}
                          fallback={<p>Loading history…</p>}
                        >
                          <Show
                            when={!executions.error}
                            fallback={
                              <p class="error">{readError(executions.error)}</p>
                            }
                          >
                            <Show
                              when={executions()?.items.length}
                              fallback={
                                <p class="muted">No occurrences yet.</p>
                              }
                            >
                              <div class="table-scroll">
                                <table>
                                  <thead>
                                    <tr>
                                      <th>Scheduled for</th>
                                      <th>Status</th>
                                      <th>Last attempt</th>
                                      <th>Receipt / failure</th>
                                    </tr>
                                  </thead>
                                  <tbody>
                                    <For each={executions()?.items}>
                                      {(e) => (
                                        <tr>
                                          <td>
                                            {date(e.scheduled_for)}
                                            <small class="mono">{e.id}</small>
                                          </td>
                                          <td>
                                            <Badge value={e.status} />
                                          </td>
                                          <td>
                                            {date(e.attempted_at)}
                                            <small>
                                              Delivered {date(e.delivered_at)}
                                            </small>
                                          </td>
                                          <td>
                                            <span class="mono break">
                                              {e.hook_event_id || "—"}
                                            </span>
                                            <Show when={e.failure_reason}>
                                              <small class="danger-text">
                                                {e.failure_reason}
                                              </small>
                                            </Show>
                                          </td>
                                        </tr>
                                      )}
                                    </For>
                                  </tbody>
                                </table>
                              </div>
                              <div class="pagination">
                                <span>webhook ingress receipts</span>
                                <button
                                  class="secondary"
                                  disabled={!execHistory().length}
                                  onClick={() => {
                                    const h = [...execHistory()];
                                    setExecCursor(h.pop() || "");
                                    setExecHistory(h);
                                  }}
                                >
                                  Previous
                                </button>
                                <button
                                  class="secondary"
                                  disabled={!executions()?.next_cursor}
                                  onClick={() => {
                                    setExecHistory((h) => [...h, execCursor()]);
                                    setExecCursor(executions()!.next_cursor!);
                                  }}
                                >
                                  Next
                                </button>
                              </div>
                            </Show>
                          </Show>
                        </Show>
                      </div>
                    )}
                  </Show>
                </Show>
              </Show>
            </Panel>
          </Show>
        </main>
        <footer class="app-footer">
          <span>Silicon Remind</span>
          <span>Reminders, on schedule.</span>
        </footer>
      </div>
      <Show when={dialog()}>
        {(spec) => (
          <Modal
            spec={spec()}
            busy={busy()}
            error={dialogError()}
            close={() => setDialog(undefined)}
            submit={(v) => void submit(v)}
          />
        )}
      </Show>
      <Show when={secret()}>
        <SecretDialog value={secret()} close={() => setSecret("")} />
      </Show>
    </div>
  );
}
function SecretDialog(p: { value: string; close: () => void }) {
  let d!: HTMLDialogElement;
  const [copied, setCopied] = createSignal(false);
  onMount(() => d.showModal());
  onCleanup(() => d.close());
  return (
    <dialog
      ref={d}
      aria-labelledby="key-title"
      onCancel={(e) => {
        e.preventDefault();
        p.close();
      }}
    >
      <div class="secret-dialog">
        <h2 id="key-title">Environment key</h2>
        <p class="muted">
          Anyone with this key can administer the sandbox. Keep it somewhere
          private.
        </p>
        <input
          aria-label="Environment root key"
          readonly
          value={p.value}
          onFocus={(e) => e.currentTarget.select()}
        />
        <div class="actions">
          <button
            class="secondary"
            onClick={async () => {
              try {
                await navigator.clipboard.writeText(p.value);
                setCopied(true);
              } catch {
                setCopied(false);
              }
            }}
          >
            {copied() ? "Copied" : "Copy key"}
          </button>
          <button class="primary" onClick={p.close}>
            Done
          </button>
        </div>
      </div>
    </dialog>
  );
}
