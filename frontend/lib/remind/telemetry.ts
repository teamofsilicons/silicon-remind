export function telemetryBatch(body: unknown) {
  const names: Record<string, string> = {
    page_view: "page_view",
    page_exit: "navigation",
    navigation: "navigation",
    click: "interaction",
    scroll: "interaction",
    error: "error",
    network_error: "error",
    network: "request_completed",
    timing: "performance",
  };
  const events =
    body &&
    typeof body === "object" &&
    "events" in body &&
    Array.isArray(body.events)
      ? body.events
      : [];
  return {
    table: "remindtelemetry",
    events: events.slice(0, 40).map((event: unknown) => {
      const e =
        event && typeof event === "object"
          ? (event as Record<string, unknown>)
          : {};
      const data =
        e.data && typeof e.data === "object"
          ? (e.data as Record<string, unknown>)
          : {};
      const kind = names[String(e.type)] || "interaction";
      const raw = Number(
        data.duration_ms ?? data.elapsed_ms ?? data.load_ms ?? 0,
      );
      const status = Number(data.status);
      return {
        source: "web",
        event: kind,
        step: "browser",
        success: kind !== "error" && !(status >= 400),
        duration_ms: Number.isFinite(raw)
          ? Math.min(86400000, Math.max(0, Math.round(raw)))
          : 0,
        status_code:
          Number.isInteger(status) && status >= 100 && status <= 599
            ? status
            : null,
      };
    }),
  };
}
