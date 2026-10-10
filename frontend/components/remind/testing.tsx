"use client";
import { useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";
import { Button } from "@/components/arc/button/button";
import { Input } from "@/components/arc/input/input";
import { Checkbox } from "@/components/arc/checkbox/checkbox";
import { Dialog, DialogContent } from "@/components/arc/dialog/dialog";
import { HoldToConfirm } from "@/components/arc/hold-to-confirm/hold-to-confirm";
import { Page, PageHeader } from "@/components/foundation/layout/layout";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { api } from "@/lib/client/api";
import {
  accountName,
  date,
  type Environment,
  type PageResult,
  type Silicon,
} from "@/lib/remind/types";
import {
  chooseEnvironment,
  Empty,
  EnvironmentBanner,
  useAction,
  useRemind,
} from "./common";
import styles from "./remind.module.css";
export function Testing() {
  const { scope, environment, identity } = useRemind();
  const action = useAction();
  const [deleted, setDeleted] = useState(false);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [secret, setSecret] = useState<{ name: string; key: string } | null>(
    null,
  );
  const list = useInfiniteQuery({
    queryKey: ["remind", "production", "environments", deleted],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      api.get<PageResult<Environment>>("/test-environments", {
        query: { after: pageParam, include_deleted: deleted },
      }),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: !!environment.data,
  });
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
  const canManage = (e: Environment) =>
    e.owner_uuid === identity.data?.uuid ||
    silicons.data?.pages
      .flatMap((p) => p.items)
      .some((s) => s.uuid === e.owner_uuid && s.relation === "custodian");
  const showKey = async (
    e: Environment,
    actionName?: "key-rotations" | "restorations",
  ) => {
    const result = actionName
      ? await api.post<{ key: string }>(
          `/test-environments/${e.id}/${actionName}`,
          {},
        )
      : await api.get<{ key: string }>(`/test-environments/${e.id}/key`);
    setSecret({ name: e.name, key: result.key });
  };
  const rows = list.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <Page className={styles.page}>
      <PageHeader
        title="Test environments"
        description="Try reminders in an isolated space. Retired environments can be recovered for 30 days."
        actions={
          <Button onClick={() => setCreating(true)}>New environment</Button>
        }
      />
      <EnvironmentBanner />
      <Checkbox
        label="Include retired environments"
        checked={deleted}
        onCheckedChange={(v) => setDeleted(v === true)}
      />
      {action.error ? <ErrorAlert error={action.error} /> : null}
      {list.error && <ErrorAlert error={list.error} />}
      {rows.length === 0 && !list.isPending ? (
        <Empty>
          No test environments yet. Create one to try reminder schedules and
          delivery without affecting Production.
        </Empty>
      ) : (
        <ul className={styles.list}>
          {rows.map((e) => (
            <li key={e.id} data-sq="surface" className={styles.row}>
              <div>
                <h2 style={{ fontSize: "1.1rem" }}>{e.name}</h2>
                <p>{e.description || "An isolated reminder environment."}</p>
                <small>
                  Owned by {accountName(e.owner)} ·{" "}
                  {e.deleted_at
                    ? `Recover before ${date(e.purge_after)}`
                    : `Last active ${date(e.last_activity_at)}`}
                </small>
                <div className={styles.actions} style={{ marginTop: 18 }}>
                  {e.deleted_at ? (
                    canManage(e) && (
                      <Button
                        variant="secondary"
                        loading={action.busy}
                        onClick={() =>
                          void action.act(
                            () => showKey(e, "restorations"),
                            "Environment restored with a new key",
                          )
                        }
                      >
                        Restore environment
                      </Button>
                    )
                  ) : (
                    <>
                      <Button
                        variant={scope === e.id ? "secondary" : "primary"}
                        disabled={action.busy}
                        onClick={() =>
                          void action.act(
                            () => chooseEnvironment(e.id),
                            "Environment selected",
                          )
                        }
                      >
                        {scope === e.id
                          ? "Refresh selection"
                          : "Use environment"}
                      </Button>
                      <Button
                        variant="ghost"
                        disabled={action.busy}
                        onClick={() =>
                          void action.act(() => showKey(e), "Key revealed")
                        }
                      >
                        Reveal key
                      </Button>
                      {canManage(e) && (
                        <>
                          <HoldToConfirm
                            label="Hold to rotate key"
                            disabled={action.busy}
                            onConfirm={() =>
                              void action.act(
                                () => showKey(e, "key-rotations"),
                                "Key rotated; select the environment again before using it",
                              )
                            }
                          />
                          <HoldToConfirm
                            label="Hold to retire"
                            tone="danger"
                            disabled={action.busy}
                            onConfirm={() =>
                              void action.act(async () => {
                                await api.delete(`/test-environments/${e.id}`);
                                if (scope === e.id)
                                  await chooseEnvironment(null);
                              }, "Environment retired")
                            }
                          />
                        </>
                      )}
                    </>
                  )}
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
      {list.hasNextPage && (
        <Button onClick={() => void list.fetchNextPage()}>
          More environments
        </Button>
      )}
      {silicons.hasNextPage && (
        <Button variant="ghost" onClick={() => void silicons.fetchNextPage()}>
          Load more custodial management rights
        </Button>
      )}
      {scope !== "production" && (
        <section className={styles.section}>
          <h2>Clear selected environment</h2>
          <p className={styles.muted}>
            Permanently remove its reminders, execution history and webhook
            subscriptions. The environment and its key remain.
          </p>
          <HoldToConfirm
            label="Hold to clear all test data"
            tone="danger"
            disabled={action.busy}
            onConfirm={() =>
              void action.act(
                () => api.post("/testing-environment/cleanings", {}),
                "Test data cleared",
              )
            }
          />
        </section>
      )}
      {creating && (
        <Dialog
          open
          onOpenChange={(v) => {
            if (!v) setCreating(false);
          }}
        >
          <DialogContent
            title="New test environment"
            description="Give this isolated space a name you will recognize."
          >
            <form
              className={styles.form}
              onSubmit={(e) => {
                e.preventDefault();
                void action.act(async () => {
                  const result = await api.post<{
                    environment: Environment;
                    key: string;
                  }>("/test-environments", {
                    name,
                    description: description || null,
                  });
                  setCreating(false);
                  setSecret({ name: result.environment.name, key: result.key });
                  setName("");
                  setDescription("");
                }, "Environment created");
              }}
            >
              {action.error ? <ErrorAlert error={action.error} /> : null}
              <Input
                label="Environment name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                required
                maxLength={100}
              />
              <Input
                label="Description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                maxLength={10000}
              />
              <Button type="submit" loading={action.busy}>
                Create environment
              </Button>
            </form>
          </DialogContent>
        </Dialog>
      )}
      {secret && (
        <Dialog
          open
          onOpenChange={(v) => {
            if (!v) setSecret(null);
          }}
        >
          <DialogContent
            title={`Key for ${secret.name}`}
            description="Keep this key private. Remind stores the selected environment key in a sealed, httpOnly cookie."
          >
            <div className={styles.form}>
              <p data-sq="surface" className={styles.secret}>
                {secret.key}
              </p>
              <Button
                onClick={() => void navigator.clipboard.writeText(secret.key)}
              >
                Copy key
              </Button>
              <Button variant="secondary" onClick={() => setSecret(null)}>
                Done
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      )}
    </Page>
  );
}
