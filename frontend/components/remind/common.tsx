"use client";
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/arc/button/button";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { api } from "@/lib/client/api";
import { ApiError, errorFromResponse } from "@/lib/errors";
import { notify } from "@/lib/notify";
import type { Identity } from "@/lib/remind/types";
import styles from "./remind.module.css";
export type EnvironmentChoice = { id: string; name: string } | null;
export async function chooseEnvironment(id: string | null) {
  const response = await fetch("/auth/environment", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id }),
  });
  const body = await response.json();
  if (!response.ok)
    throw errorFromResponse(response, body, "POST", "/auth/environment");
  window.location.assign(window.location.pathname);
}
export function useRemind() {
  const environment = useQuery({
    queryKey: ["environment"],
    queryFn: async () => {
      const response = await fetch("/auth/environment");
      const body = await response.json();
      if (!response.ok)
        throw errorFromResponse(response, body, "GET", "/auth/environment");
      return body as { environment: EnvironmentChoice };
    },
    staleTime: Infinity,
  });
  const scope = environment.data?.environment?.id ?? "production";
  const identity = useQuery({
    queryKey: ["remind", scope, "identity"],
    queryFn: () => api.get<Identity>("/auth/me"),
    enabled: !!environment.data,
  });
  return { scope, environment, identity };
}
export function useAction() {
  const cache = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  async function act(work: () => Promise<unknown>, success: string) {
    setBusy(true);
    setError(null);
    try {
      await work();
      await cache.invalidateQueries({ queryKey: ["remind"] });
      notify.success(success);
      return true;
    } catch (e) {
      setError(
        e instanceof ApiError
          ? e
          : new ApiError({
              status: 0,
              code: "client_error",
              message:
                e instanceof Error
                  ? e.message
                  : "Could not complete this action.",
            }),
      );
      return false;
    } finally {
      setBusy(false);
    }
  }
  return { act, busy, error, setError };
}
export function EnvironmentBanner() {
  const { environment } = useRemind();
  const action = useAction();
  return (
    <>
      {environment.error && <ErrorAlert error={environment.error} />}{" "}
      {environment.data?.environment && (
        <div data-sq="surface" className={styles.banner}>
          <div>
            <strong>Testing · {environment.data.environment.name}</strong>
            <p>Reminders and deliveries here are isolated from Production.</p>
          </div>
          <Button
            variant="secondary"
            loading={action.busy}
            onClick={() =>
              void action.act(
                () => chooseEnvironment(null),
                "Production selected",
              )
            }
          >
            Back to Production
          </Button>
        </div>
      )}
      {action.error ? <ErrorAlert error={action.error} /> : null}
    </>
  );
}
export function Empty({ children }: { children: React.ReactNode }) {
  return (
    <p data-sq="surface" className={styles.empty}>
      {children}
    </p>
  );
}
