"use client";
import { useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";
import { Plus, Pause, Play, Bell } from "lucide-react";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import { Select } from "@/components/silicon-ui/select/select";
import { Checkbox } from "@/components/silicon-ui/checkbox/checkbox";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import { Page, PageHeader } from "@/components/foundation/layout/layout";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { api } from "@/lib/client/api";
import {
  date,
  type Execution,
  type PageResult,
  type Schedule,
  type Silicon,
} from "@/lib/remind/types";
import { Empty, EnvironmentBanner, useAction, useRemind } from "./common";
import styles from "./remind.module.css";
export function Reminders({
  archived = false,
  initialSilicon = "all",
}: {
  archived?: boolean;
  initialSilicon?: string;
}) {
  const { scope, environment, identity } = useRemind();
  const action = useAction();
  const [silicon, setSilicon] = useState(initialSilicon);
  const [status, setStatus] = useState("all");
  const [selected, setSelected] = useState<string[]>([]);
  const [detail, setDetail] = useState<Schedule | null>(null);
  const [creating, setCreating] = useState(false);
  const silicons = useInfiniteQuery({
    queryKey: ["remind", scope, "silicons"],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      api.get<PageResult<Silicon>>("/silicons", {
        query: { after: pageParam },
      }),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: !!environment.data,
  });
  const list = useInfiniteQuery({
    queryKey: ["remind", scope, "schedules", archived, silicon, status],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      api.get<PageResult<Schedule>>("/schedules", {
        query: {
          section: archived ? "archived" : "current",
          silicon_id: silicon === "all" ? undefined : silicon,
          status: status === "all" ? undefined : status,
          cursor: pageParam,
        },
      }),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: !!environment.data,
  });
  const rows = list.data?.pages.flatMap((p) => p.items) ?? [];
  const owner = (r: Schedule) =>
    identity.data?.can_manage_reminders &&
    r.owner?.uuid === identity.data.uuid &&
    !archived;
  return (
    <Page className={styles.page}>
      <PageHeader
        title={archived ? "Archive" : "Reminders"}
        description={
          archived
            ? "Archived reminders remain readable for 45 days, with their delivery history."
            : "The next thing to remember, on each Silicon’s clock."
        }
        actions={
          identity.data?.can_manage_reminders && !archived ? (
            <Button onClick={() => setCreating(true)}>
              <Plus size={17} />
              New reminder
            </Button>
          ) : undefined
        }
      />
      <EnvironmentBanner />
      {identity.error && <ErrorAlert error={identity.error} />}
      <div className={styles.toolbar}>
        <Select
          label="Silicon"
          value={silicon}
          onValueChange={(v) => {
            setSilicon(v);
            setSelected([]);
          }}
          options={[
            { value: "all", label: "All visible Silicons" },
            ...(silicons.data?.pages.flatMap((p) => p.items) ?? []).map(
              (s) => ({ value: s.uuid, label: s.display_name || s.silicon_id }),
            ),
          ]}
        />
        <Select
          label="Status"
          value={status}
          onValueChange={(v) => {
            setStatus(v);
            setSelected([]);
          }}
          options={[
            { value: "all", label: "Any status" },
            { value: "active", label: "Active" },
            { value: "paused", label: "Paused" },
            { value: "completed", label: "Completed" },
          ]}
        />
        {silicons.hasNextPage && (
          <Button onClick={() => void silicons.fetchNextPage()}>
            More Silicons
          </Button>
        )}
      </div>
      {action.error ? <ErrorAlert error={action.error} /> : null}
      {list.error && <ErrorAlert error={list.error} />}{" "}
      {selected.length > 0 && (
        <div className={styles.actions}>
          <span>{selected.length} selected</span>
          {(["paused", "active"] as const).map((value) => (
            <Button
              key={value}
              variant="secondary"
              loading={action.busy}
              onClick={() =>
                void action
                  .act(
                    () =>
                      api.patch("/schedules", {
                        schedule_ids: selected,
                        status: value,
                      }),
                    value === "paused"
                      ? "Reminders paused"
                      : "Reminders resumed",
                  )
                  .then((ok) => {
                    if (ok) setSelected([]);
                  })
              }
            >
              {value === "paused" ? <Pause size={16} /> : <Play size={16} />}{" "}
              {value === "paused" ? "Pause selected" : "Resume selected"}
            </Button>
          ))}
        </div>
      )}
      {list.isPending ? (
        <p className={styles.muted}>Loading reminders…</p>
      ) : rows.length === 0 ? (
        <Empty>
          {archived
            ? "No archived reminders in this view."
            : identity.data?.can_manage_reminders
              ? "Nothing scheduled yet. Create a reminder to get started."
              : "Your Silicons’ reminders will appear here. Silicons create and manage their own reminders."}
        </Empty>
      ) : (
        <ul className={styles.list}>
          {rows.map((r) => (
            <li key={r.id} data-sq="surface" className={styles.row}>
              {owner(r) && (
                <Checkbox
                  aria-label={`Select ${r.text.slice(0, 60)}`}
                  checked={selected.includes(r.id)}
                  onCheckedChange={(checked) =>
                    setSelected((values) =>
                      checked
                        ? [...values, r.id]
                        : values.filter((v) => v !== r.id),
                    )
                  }
                />
              )}
              <div className={styles.rowBody}>
                <button
                  className={styles.rowButton}
                  onClick={() => setDetail(r)}
                >
                  {r.text}
                </button>
                <small>
                  {r.silicon_id} ·{" "}
                  {r.kind === "one_time" ? "Once" : "Recurring"} · {r.timezone}
                </small>
                <small>
                  <Bell
                    size={12}
                    style={{ display: "inline", marginRight: 5 }}
                  />
                  {archived
                    ? `Archived ${date(r.archived_at)}`
                    : `Next ${date(r.next_run_at)}`}
                </small>
              </div>
              <span
                data-sq="surface"
                className={styles.status}
                data-active={r.status === "active"}
              >
                {r.status}
              </span>
              <Button variant="ghost" size="sm" onClick={() => setDetail(r)}>
                Details
              </Button>
            </li>
          ))}
        </ul>
      )}
      <div className={styles.footer}>
        <span className={styles.muted}>{rows.length} reminders</span>
        {list.hasNextPage && (
          <Button
            loading={list.isFetchingNextPage}
            onClick={() => void list.fetchNextPage()}
          >
            Load more
          </Button>
        )}
      </div>
      {creating && <ScheduleEditor onClose={() => setCreating(false)} />}{" "}
      {detail && (
        <ScheduleDetail
          schedule={detail}
          canManage={!!owner(detail)}
          scope={scope}
          onClose={() => setDetail(null)}
        />
      )}
    </Page>
  );
}
function ScheduleEditor({
  schedule,
  onClose,
}: {
  schedule?: Schedule;
  onClose: () => void;
}) {
  const action = useAction();
  const [text, setText] = useState(schedule?.text ?? "");
  const [cron, setCron] = useState(schedule?.cron ?? "0 9 * * *");
  const [timezone, setTimezone] = useState(
    schedule?.timezone ??
      Intl.DateTimeFormat().resolvedOptions().timeZone ??
      "UTC",
  );
  const [kind, setKind] = useState(schedule?.kind ?? "recurring");
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent
        className={styles.dialog}
        title={schedule ? "Edit reminder" : "New reminder"}
        description="Choose when to fire, using an explicit timezone. A one-time reminder runs at the next matching time."
      >
        <form
          className={styles.form}
          onSubmit={(event) => {
            event.preventDefault();
            void action
              .act(
                () =>
                  schedule
                    ? api.patch(`/schedules/${schedule.id}`, {
                        text,
                        cron,
                        timezone,
                        kind,
                      })
                    : api.post("/schedules", { text, cron, timezone, kind }),
                schedule ? "Reminder updated" : "Reminder created",
              )
              .then((ok) => {
                if (ok) onClose();
              });
          }}
        >
          {action.error ? <ErrorAlert error={action.error} /> : null}
          <Textarea
            label="Reminder"
            value={text}
            onChange={(e) => setText(e.target.value)}
            maxLength={100000}
            required
          />
          <div className={styles.two}>
            <Select
              label="Repeat"
              value={kind}
              onValueChange={(v) => setKind(v as Schedule["kind"])}
              options={[
                { value: "recurring", label: "Recurring" },
                { value: "one_time", label: "Once" },
              ]}
            />
            <Input
              label="Timezone"
              value={timezone}
              onChange={(e) => setTimezone(e.target.value)}
              placeholder="Asia/Kolkata"
              required
            />
          </div>
          <Input
            label="Schedule (cron)"
            description="Minute · hour · day of month · month · weekday. For example, 0 9 * * * runs at 9am."
            value={cron}
            onChange={(e) => setCron(e.target.value)}
            required
          />
          <div className={styles.actions}>
            <Button
              type="button"
              variant="ghost"
              onClick={() => setCron("0 9 * * *")}
            >
              Every day at 9am
            </Button>
            <Button
              type="button"
              variant="ghost"
              onClick={() => setCron("0 9 * * 1-5")}
            >
              Weekdays at 9am
            </Button>
          </div>
          <Button type="submit" loading={action.busy}>
            {schedule ? "Save changes" : "Create reminder"}
          </Button>
        </form>
      </DialogContent>
    </Dialog>
  );
}
function ScheduleDetail({
  schedule,
  canManage,
  scope,
  onClose,
}: {
  schedule: Schedule;
  canManage: boolean;
  scope: string;
  onClose: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const action = useAction();
  const history = useInfiniteQuery({
    queryKey: ["remind", scope, "executions", schedule.id],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      api.get<PageResult<Execution>>(`/schedules/${schedule.id}/executions`, {
        query: { cursor: pageParam },
      }),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
  if (editing)
    return (
      <ScheduleEditor
        schedule={schedule}
        onClose={() => {
          setEditing(false);
          onClose();
        }}
      />
    );
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent
        className={styles.dialog}
        title="Reminder details"
        description={schedule.silicon_id}
      >
        <div className={styles.form}>
          <p style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>
            {schedule.text}
          </p>
          <dl className={styles.details}>
            <dt>Schedule</dt>
            <dd>{schedule.cron}</dd>
            <dt>Timezone</dt>
            <dd>{schedule.timezone}</dd>
            <dt>Status</dt>
            <dd>{schedule.status}</dd>
            <dt>Next run</dt>
            <dd>{date(schedule.next_run_at)}</dd>
            {schedule.purge_after && (
              <>
                <dt>Retained until</dt>
                <dd>{date(schedule.purge_after)}</dd>
              </>
            )}
          </dl>
          {action.error ? <ErrorAlert error={action.error} /> : null}
          {canManage && (
            <div className={styles.actions}>
              <Button onClick={() => setEditing(true)}>Edit reminder</Button>
              <Button
                variant="secondary"
                loading={action.busy}
                onClick={() =>
                  void action
                    .act(
                      () =>
                        api.patch(`/schedules/${schedule.id}`, {
                          status:
                            schedule.status === "paused" ? "active" : "paused",
                        }),
                      "Reminder updated",
                    )
                    .then((ok) => {
                      if (ok) onClose();
                    })
                }
              >
                {schedule.status === "paused" ? "Resume" : "Pause"}
              </Button>
              <HoldToConfirm
                label="Hold to archive"
                tone="danger"
                disabled={action.busy}
                onConfirm={() =>
                  void action
                    .act(
                      () => api.delete(`/schedules/${schedule.id}`),
                      "Reminder archived",
                    )
                    .then((ok) => {
                      if (ok) onClose();
                    })
                }
              />
            </div>
          )}
          <h3>Delivery history</h3>
          {history.error && <ErrorAlert error={history.error} />}
          <ul className={styles.list}>
            {history.data?.pages
              .flatMap((p) => p.items)
              .map((e) => (
                <li key={e.id}>
                  <strong>{e.status}</strong>
                  <p className={styles.muted}>
                    Scheduled {date(e.scheduled_for)} · Delivered{" "}
                    {date(e.delivered_at)}
                  </p>
                  {e.failure_reason && (
                    <p className={styles.error}>{e.failure_reason}</p>
                  )}
                </li>
              ))}
          </ul>
          {history.data?.pages[0]?.items.length === 0 && (
            <p className={styles.muted}>This reminder has not fired yet.</p>
          )}
          {history.hasNextPage && (
            <Button onClick={() => void history.fetchNextPage()}>
              More executions
            </Button>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
