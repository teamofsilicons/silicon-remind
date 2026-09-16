// Remind production telemetry. Rolling 24h, refreshed when observations arrive.
const subscriptions = {
  sources: { triggers: [{ table: "remindtelemetry" }], mode: "snapshot", sql: "SELECT record.source::String AS source, toInt32(count()) AS events, toInt32(countIf(record.success::Nullable(Bool) = false OR record.status_code::Nullable(Int32) >= 400 OR record.event::String = 'error')) AS failures, max(event_ts_ms) AS last_seen FROM remindtelemetry WHERE record.application::String = 'remind' AND event_ts_ms >= toUnixTimestamp(now()) * 1000 - 86400000 AND record.source::String IN ('web','cli','rust_client','daemon') GROUP BY source ORDER BY events DESC", onTrigger: (json, rows) => ({ ...json, sources: rows }) },
  kinds: { triggers: [{ table: "remindtelemetry" }], mode: "snapshot", sql: "SELECT record.source::String AS source, record.event::String AS event, toInt32(count()) AS events FROM remindtelemetry WHERE record.application::String = 'remind' AND event_ts_ms >= toUnixTimestamp(now()) * 1000 - 86400000 AND record.source::String IN ('web','cli','rust_client','daemon') GROUP BY source,event ORDER BY events DESC LIMIT 20", onTrigger: (json, rows) => ({ ...json, kinds: rows }) },
  recent: { triggers: [{ table: "remindtelemetry" }], mode: "snapshot", sql: "SELECT record_id, event_ts_ms AS ts, record.source::String AS source, record.event::String AS event, record.step::String AS step, record.route::String AS route, record.method::String AS method, record.status_code::Nullable(Int32) AS status, record.duration_ms::Nullable(Float64) AS duration, record.success::Nullable(Bool) AS success, record.service_version::String AS version FROM remindtelemetry WHERE record.application::String = 'remind' AND event_ts_ms >= toUnixTimestamp(now()) * 1000 - 86400000 AND record.source::String IN ('web','cli','rust_client','daemon') ORDER BY event_ts_ms DESC LIMIT 50", onTrigger: (json, rows) => ({ ...json, recent: rows }) },
};
export default defineProcessor({
  init: async () => { let json = {}; for (const sub of Object.values(subscriptions)) json = await sub.onTrigger(json, await mission_control.query(sub.sql)); return json; },
  subscriptions,
  tools: {}
});
