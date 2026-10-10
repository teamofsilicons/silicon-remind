"use client";
import { useEffect, useSyncExternalStore } from "react";
import { telemetryBatch } from "@/lib/remind/telemetry";
import { createSpaceStationWeb } from "@teamofsilicons/space-station-web";
const KEY = "remind.telemetry";
const subscribe = (changed: () => void) => {
  window.addEventListener("storage", changed);
  window.addEventListener("remind-telemetry", changed);
  return () => {
    window.removeEventListener("storage", changed);
    window.removeEventListener("remind-telemetry", changed);
  };
};
const snapshot = () => localStorage.getItem(KEY) !== "off";
export function useTelemetryPreference(): [boolean, (value: boolean) => void] {
  const enabled = useSyncExternalStore(subscribe, snapshot, () => false);
  return [
    enabled,
    (value) => {
      localStorage.setItem(KEY, value ? "on" : "off");
      window.dispatchEvent(new Event("remind-telemetry"));
    },
  ];
}
export function RemindTelemetry() {
  const [enabled] = useTelemetryPreference();
  useEffect(() => {
    if (!enabled) return;
    const sender = createSpaceStationWeb({
      analyticsTable: "remindtelemetry",
      eventsTable: "remindtelemetry",
      endpoint: "/api/telemetry/events",
      fetch: (input, init) =>
        fetch(input, {
          ...init,
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(
            telemetryBatch(JSON.parse(String(init?.body ?? "{}"))),
          ),
        }),
    });
    sender.track("navigation");
    return () => {
      sender.setEnabled(false);
      void sender.destroy();
    };
  }, [enabled]);
  return null;
}
