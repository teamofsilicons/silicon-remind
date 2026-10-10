"use client";
import { useState } from "react";
import Link from "next/link";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Button } from "@/components/arc/button/button";
import { Input } from "@/components/arc/input/input";
import { Select } from "@/components/arc/select/select";
import { HoldToConfirm } from "@/components/arc/hold-to-confirm/hold-to-confirm";
import { Page, PageHeader } from "@/components/foundation/layout/layout";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { api } from "@/lib/client/api";
import {
  accountName,
  type Silicon,
  type PageResult,
  type Grant,
  type Allowance,
} from "@/lib/remind/types";
import { Empty, EnvironmentBanner, useAction, useRemind } from "./common";
import styles from "./remind.module.css";
export function Sharing() {
  const { scope, environment, identity } = useRemind();
  const action = useAction();
  const [chosen, setChosen] = useState("");
  const [recipient, setRecipient] = useState("");
  const [allow, setAllow] = useState("");
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
  const all = silicons.data?.pages.flatMap((p) => p.items) ?? [];
  const managed = all.filter(
    (s) => s.relation === "self" || s.relation === "custodian",
  );
  const selected = chosen || managed[0]?.uuid || "";
  const grants = useQuery({
    queryKey: ["remind", "production", "viewers"],
    queryFn: () => api.get<{ granted: Grant[]; received: Grant[] }>("/viewers"),
    enabled: scope === "production",
  });
  const allowances = useQuery({
    queryKey: ["remind", "production", "allowed", selected],
    queryFn: () =>
      api.get<{ items: Allowance[] }>("/allowed-accounts", {
        query: { silicon_id: selected },
      }),
    enabled: scope === "production" && !!selected,
  });
  return (
    <Page className={styles.page}>
      <PageHeader
        title="Silicons & sharing"
        description="See whose reminders you can read, and choose exactly who may read yours."
      />
      <EnvironmentBanner />
      {silicons.error && <ErrorAlert error={silicons.error} />}
      <div className={styles.grid}>
        {all.map((s) => (
          <div key={s.uuid} data-sq="surface" className={styles.card}>
            <h2>{s.display_name || s.silicon_id}</h2>
            <p>{s.silicon_id}</p>
            <strong>{s.reminder_count}</strong>
            <p>
              {s.relation === "custodian"
                ? "You look after this Silicon"
                : s.relation === "sibling"
                  ? "Same custodian"
                  : s.relation === "self"
                    ? "Your reminders"
                    : "Shared with you"}
            </p>
            <Link href={`/reminders?silicon=${encodeURIComponent(s.uuid)}`}>
              View reminders
            </Link>
          </div>
        ))}
      </div>
      {all.length === 0 && !silicons.isPending && (
        <Empty>
          No Silicons are visible yet. The Silicons you look after, and those
          sharing with you, appear here.
        </Empty>
      )}
      {silicons.hasNextPage && (
        <Button onClick={() => void silicons.fetchNextPage()}>
          More Silicons
        </Button>
      )}
      {scope !== "production" ? (
        <Empty>
          Account sharing is managed in Production. Switch back to manage
          viewers and allowed accounts.
        </Empty>
      ) : (
        <>
          {action.error ? <ErrorAlert error={action.error} /> : null}
          {grants.error && <ErrorAlert error={grants.error} />}{" "}
          {managed.length > 0 && (
            <section className={styles.section}>
              <h2>Share reminders</h2>
              <Select
                label="Silicon to manage"
                value={selected}
                onValueChange={setChosen}
                options={managed.map((s) => ({
                  value: s.uuid,
                  label: s.display_name || s.silicon_id,
                }))}
              />
              <form
                className={styles.toolbar}
                onSubmit={(e) => {
                  e.preventDefault();
                  void action
                    .act(
                      () =>
                        api.post("/viewers", {
                          id: recipient.trim(),
                          silicon_id: selected,
                        }),
                      "Viewer added",
                    )
                    .then((ok) => {
                      if (ok) setRecipient("");
                    });
                }}
              >
                <Input
                  label="Account to share with"
                  placeholder="c:ada or si:scout"
                  value={recipient}
                  onChange={(e) => setRecipient(e.target.value)}
                  maxLength={64}
                  required
                />
                <Button type="submit" loading={action.busy}>
                  Add viewer
                </Button>
              </form>
              <p className={styles.muted}>
                Viewers can read reminders and delivery history. Only the owner
                Silicon changes reminders.
              </p>
              <ul className={styles.list}>
                {grants.data?.granted
                  .filter((g) => g.owner.uuid === selected)
                  .map((g) => (
                    <li key={g.id} data-sq="surface" className={styles.row}>
                      <div>{accountName(g.viewer)}</div>
                      <HoldToConfirm
                        label="Hold to remove viewer"
                        tone="danger"
                        disabled={action.busy}
                        onConfirm={() =>
                          void action.act(
                            () =>
                              api.delete(
                                `/viewers/${encodeURIComponent(g.viewer.uuid)}`,
                                { query: { silicon_id: selected } },
                              ),
                            "Viewer removed",
                          )
                        }
                      />
                    </li>
                  ))}
              </ul>
              <h2>Allowed accounts</h2>
              <p className={styles.muted}>
                Accounts outside this Silicon’s circle need permission before
                sharing their reminders with it.
              </p>
              {allowances.error && <ErrorAlert error={allowances.error} />}
              <form
                className={styles.toolbar}
                onSubmit={(e) => {
                  e.preventDefault();
                  void action
                    .act(
                      () =>
                        api.post("/allowed-accounts", {
                          id: allow.trim(),
                          silicon_id: selected,
                        }),
                      "Account allowed",
                    )
                    .then((ok) => {
                      if (ok) setAllow("");
                    });
                }}
              >
                <Input
                  label="Account allowed to share"
                  placeholder="c:ada or si:scout"
                  value={allow}
                  onChange={(e) => setAllow(e.target.value)}
                  maxLength={64}
                  required
                />
                <Button type="submit" loading={action.busy}>
                  Allow account
                </Button>
              </form>
              <ul className={styles.list}>
                {allowances.data?.items.map((a) => (
                  <li key={a.id} data-sq="surface" className={styles.row}>
                    <div>{accountName(a.allowed)}</div>
                    <HoldToConfirm
                      label="Hold to disallow"
                      tone="danger"
                      disabled={action.busy}
                      onConfirm={() =>
                        void action.act(
                          () =>
                            api.delete(
                              `/allowed-accounts/${encodeURIComponent(a.allowed.uuid)}`,
                              { query: { silicon_id: selected } },
                            ),
                          "Account disallowed",
                        )
                      }
                    />
                  </li>
                ))}
              </ul>
            </section>
          )}
          <section className={styles.section}>
            <h2>Shared with you</h2>
            {grants.data?.received.length === 0 ? (
              <p className={styles.muted}>
                No explicit viewer grants yet. Your{" "}
                {identity.data?.kind === "carbon" ? "custodial" : "circle"}{" "}
                access is shown above.
              </p>
            ) : (
              <ul className={styles.list}>
                {grants.data?.received.map((g) => (
                  <li key={g.id} data-sq="surface" className={styles.row}>
                    <div>
                      {accountName(g.owner)}
                      <small>Read reminders and delivery history</small>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </>
      )}
    </Page>
  );
}
