"use client";
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import { Switch } from "@/components/silicon-ui/switch/switch";
import { Page, PageHeader } from "@/components/foundation/layout/layout";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { api } from "@/lib/client/api";
import { EnvironmentBanner, useAction, useRemind } from "./common";
import { useTelemetryPreference } from "./telemetry";
import styles from "./remind.module.css";
export function Settings() {
  const { identity } = useRemind();
  const action = useAction();
  const [report, setReport] = useState("");
  const [pr, setPr] = useState("");
  const [receipt, setReceipt] = useState<{ id: string; status: string } | null>(
    null,
  );
  const [enabled, setEnabled] = useTelemetryPreference();
  const health = useQuery({
    queryKey: ["remind", "health"],
    queryFn: () => api.get<Record<string, unknown>>("/health/ready"),
  });
  return (
    <Page className={styles.page}>
      <PageHeader
        title="Settings"
        description="Your account, service status and a way to report a problem."
      />
      <EnvironmentBanner />
      {identity.error && <ErrorAlert error={identity.error} />}
      <div className={styles.grid}>
        <section data-sq="surface" className={styles.card}>
          <h2>Silicon Accounts</h2>
          <p>{identity.data?.display_name}</p>
          <p>{identity.data?.id}</p>
          <p>
            {identity.data?.can_manage_reminders
              ? "You can create and manage your reminders."
              : "You can view the reminders of your Silicons and accounts sharing with you."}
          </p>
          <a
            href="https://accounts.teamofsilicons.com"
            target="_blank"
            rel="noreferrer"
          >
            Manage account
          </a>
        </section>
        <section data-sq="surface" className={styles.card}>
          <h2>Service</h2>
          {health.error ? (
            <ErrorAlert error={health.error} />
          ) : (
            <p>{health.isPending ? "Checking service…" : "Remind is ready."}</p>
          )}
          <p>
            Silicons keep up to 1,000 retained reminders and 20 active
            subscriptions per environment. Each account may keep 5 active test
            environments.
          </p>
          <Button variant="ghost" onClick={() => void health.refetch()}>
            Check again
          </Button>
        </section>
      </div>
      <section className={styles.section}>
        <h2>Telemetry</h2>
        <Switch
          label="Share usage diagnostics"
          checked={enabled}
          onCheckedChange={setEnabled}
        />
        <p className={styles.muted}>
          Send bounded event counts, status and timing. Reminder text,
          destinations, credentials and account identifiers are excluded.
        </p>
      </section>
      <section className={styles.section}>
        <h2>Report a problem</h2>
        <p className={styles.muted}>
          Describe what happened and what you expected. Leave out keys and
          personal reminder contents.
        </p>
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            void action.act(async () => {
              setReceipt(
                await api.post<{ id: string; status: string }>("/reports", {
                  message: report,
                  ...(pr ? { pr } : {}),
                }),
              );
              setReport("");
              setPr("");
            }, "Report received");
          }}
        >
          {action.error ? <ErrorAlert error={action.error} /> : null}
          <Textarea
            label="What happened?"
            value={report}
            onChange={(e) => setReport(e.target.value)}
            required
            maxLength={16384}
          />
          <Input
            label="Related pull request (optional)"
            value={pr}
            onChange={(e) => setPr(e.target.value)}
            type="url"
            placeholder="https://github.com/teamofsilicons/silicon-remind/pull/123"
          />
          <Button type="submit" loading={action.busy}>
            Send report
          </Button>
        </form>
        {receipt && (
          <p role="status">
            Report {receipt.id} · {receipt.status}
          </p>
        )}
      </section>
    </Page>
  );
}
