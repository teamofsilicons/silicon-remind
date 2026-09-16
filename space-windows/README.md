# Remind Space Windows

Published in the `tos` Space Station organization on 2026-09-13.

- [Remind Operations](https://spacestation.teamofsilicons.com/o/tos/windows/0f48cac8-eb65-4ca2-8401-78b0a468e88e), version `v2`: API request counts, 4xx/5xx responses, mean and P95 latency, hourly traffic, busiest routes, recent HTTP errors, and worker heartbeat.
- [Remind Activity](https://spacestation.teamofsilicons.com/o/tos/windows/72cafdf2-8022-4790-ad55-806c25ec28ee), version `v1`: browser, CLI, Rust client, and updater event counts, failures, last-seen timestamps, event types, and a source-filtered recent activity stream.

Both use `tos.remindtelemetry` and initialize from existing data before subscribing to new rows. Queries cover the last 24 hours by ingestion time and refresh when a record arrives. Heartbeat freshness and connection state refresh in the renderer every 15 seconds. The hourly chart includes the partial first and current hours.

These are operational observations from opted-in clients, not complete usage, reminder delivery, or billing records. Sandbox events remain isolated. Activity displays an explicit empty state until client observations arrive. The recent activity filter applies to the latest 50 events across sources.

Each directory contains the exact published `processor.js` and `renderer.html`. To update, paste both files into the window's **View code** page and publish a named version. No credentials are embedded. Access is initially the creating user, `@saket`; existing access to other windows or tables was not changed.

Verified both versions publish successfully and render through the live Space Station runtime. Operations loaded actual requests/errors and its heartbeat advanced with new production observations. Activity initialized successfully with the empty state; no synthetic production events were inserted.
