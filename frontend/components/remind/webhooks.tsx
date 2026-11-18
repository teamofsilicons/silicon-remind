"use client";
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import { Page, PageHeader } from "@/components/foundation/layout/layout";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { api } from "@/lib/client/api";
import { date, type Subscription } from "@/lib/remind/types";
import { Empty, EnvironmentBanner, useAction, useRemind } from "./common";
import styles from "./remind.module.css";
export function Webhooks() {
  const { scope, environment, identity } = useRemind();
  const action = useAction();
  const [url, setUrl] = useState("");
  const [secret, setSecret] = useState("");
  const list = useQuery({
    queryKey: ["remind", scope, "webhooks"],
    queryFn: () => api.get<{ items: Subscription[] }>("/webhooks"),
    enabled: !!environment.data,
  });
  const ting = useQuery({
    queryKey: ["remind", scope, "ting"],
    queryFn: () => api.get<{ configured: boolean; simulated: boolean; registered_at: string | null }>("/ting"),
    enabled: !!identity.data?.can_manage_reminders,
  });
  return (
    <Page className={styles.page}>
      <PageHeader
        title="Delivery"
        description="Due reminders arrive in the owning Silicon’s Ting inbox. Add a webhook to receive a copy at another endpoint."
      />
      <EnvironmentBanner />
      {identity.error && <ErrorAlert error={identity.error} />}{" "}
      {action.error ? <ErrorAlert error={action.error} /> : null}
      {ting.error && <ErrorAlert error={ting.error} />}
      {identity.data?.can_manage_reminders && ting.data && (
        <section className={styles.form}>
          <h2>Silicon Ting</h2>
          {ting.data.simulated ? <p>Test environments simulate Ting delivery.</p> :
            !ting.data.configured ? <p>Ting is unavailable on this deployment.</p> : <>
              <p>{ting.data.registered_at ? "Remind has registered this Silicon with Ting." : "Connect this Silicon to receive due reminders."} <a href="https://ting.teamofsilicons.com/">Open Ting</a> to see reminders and manage preferences.</p>
              <Button variant="secondary" loading={action.busy} onClick={() => void action.act(() => api.put("/ting"), "Ting connected")}>
                {ting.data.registered_at ? "Reconnect Ting" : "Connect Ting"}
              </Button>
            </>}
        </section>
      )}
      <h2>Optional webhooks</h2>
      {list.error && <ErrorAlert error={list.error} />}{" "}
      {identity.data?.can_manage_reminders && (
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            void action
              .act(
                () =>
                  api.post("/webhooks", {
                    endpoint_url: url,
                    ...(secret ? { signing_secret: secret } : {}),
                  }),
                "Webhook added",
              )
              .then((ok) => {
                if (ok) {
                  setUrl("");
                  setSecret("");
                }
              });
          }}
        >
          <div className={styles.two}>
            <Input
              label="Destination URL"
              type="url"
              placeholder="https://your-service.example/reminders"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              required
            />
            <Input
              label="Signing secret (optional)"
              type="password"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              autoComplete="new-password"
              maxLength={4096}
            />
          </div>
          <p className={styles.muted}>
            Use a public HTTPS endpoint in Production. The signing secret is
            stored securely and cannot be read back.
          </p>
          <Button type="submit" loading={action.busy}>
            Add webhook
          </Button>
        </form>
      )}
      {list.isPending ? (
        <p className={styles.muted}>Loading subscriptions…</p>
      ) : list.data?.items.length === 0 ? (
        <Empty>
          No webhook endpoints have been added.
        </Empty>
      ) : (
        <ul className={styles.list}>
          {list.data?.items.map((w) => (
            <li key={w.id} data-sq="surface" className={styles.row}>
              <div>
                <p>{w.endpoint_url}</p>
                <small>
                  {w.silicon_id} · Updated {date(w.updated_at)}
                </small>
              </div>
              {identity.data?.uuid === w.owner?.uuid &&
                identity.data?.can_manage_reminders && (
                  <HoldToConfirm
                    label="Hold to remove webhook"
                    tone="danger"
                    disabled={action.busy}
                    onConfirm={() =>
                      void action.act(
                        () => api.delete(`/webhooks/${w.id}`),
                        "Webhook removed",
                      )
                    }
                  />
                )}
            </li>
          ))}
        </ul>
      )}
    </Page>
  );
}
