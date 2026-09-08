# Silicon Remind frontend

SolidJS application styled from the hosted Silicon IAm application: Plex Sans and Plex Mono, pale navigation, compact white panels, blue actions and restrained status badges. Brand assets come from the sibling Silicon IAm frontend.

## Run locally

Requires Node.js 24 or newer. From this directory:

```sh
npm ci
npm run dev
```

Open http://127.0.0.1:4330. The default upstream is the public Remind backend. There is no sample data: actions affect the selected real organization or sandbox. Choose **Continue with IAm**. Remind sends you to the configured IAm login page with `app_id=tos>remind`, `org_id=tos` (or your selected organization), and its own callback URI. IAm returns an SLT that the gateway exchanges server-side, then redirects to the signed-in workspace. Settings lets you change organization or use a short-lived token for a Silicon identity. A Carbon can browse reminders; a Silicon can manage its own reminders and webhook. Test identities must use the IAm sandbox linked to the selected Remind test environment.

## Supported workflows

- Browser redirect sign-in through IAm, plus independently authenticated test sessions; advanced SLT login, automatic refresh, identity verification, logout and organization changes.
- Current and archived reminders, Silicon/status filters, ID lookup and cursor pagination.
- One-time and recurring cron schedules with IANA timezones, full-text details, editing, individual and atomic bulk pause/resume, and 45-day archiving.
- Execution history with scheduled, attempted and delivered times, webhook receipt IDs and failure reasons.
- Organization Silicon discovery and reminder counts.
- Silicon webhook subscription management: zero, one, or many independent endpoints can be added or removed. Secrets are never read back.
- Production-authorized test environment creation, listing, root-key retrieval and rotation, deletion and restoration.
- Root-key import and workspace switching, linked IAm configuration, environment cleaning and forgetting local access.
- Backend readiness and version, explicit empty/loading/error states, keyboard-accessible dialogs and responsive navigation.

Internal service/admin APIs and CLI package installation/updating are not browser workflows.

## Build and host later

```sh
npm run build
# Supply FRONTEND_ORIGIN, SESSION_KEY and optionally SESSION_DIRECTORY through
# your host's secret/environment configuration, then:
npm start
```

`npm start` serves both `dist/client` and the `/ui/` session gateway. A static-only host is insufficient: the backend does not expose browser CORS and application tokens must stay on the server. Production startup reads process environment variables, not `.env` files. `.env.example` documents names; Vite loads local `.env` files for development only.

Generate `SESSION_KEY` with `node -e "process.stdout.write(require('node:crypto').randomBytes(32).toString('base64url'))"` directly into your secret manager or a protected file. Never commit it or place it in a `VITE_` variable. Configure `FRONTEND_ORIGIN` as the exact HTTPS public origin. Preserve that origin's Host header through the reverse proxy. Bind `HOST=0.0.0.0` only when your container/platform needs it.

Sessions use opaque HttpOnly, SameSite=Strict cookies (Secure on HTTPS). Access/refresh tokens and imported root keys are AES-256-GCM encrypted in `SESSION_DIRECTORY`, with private directory/file permissions. The encryption key and directory must survive server restarts. Losing the key invalidates stored sessions. Development creates a private persistent `.sessions/dev.key` automatically; production requires an explicit key. Never expose the session directory as static files.

The file store is intended for one server process. Refresh exchanges are serialized per session and their idempotency keys are persisted before exchange, allowing safe recovery after an ambiguous refresh response. Multiple processes or replicas require a shared session store with distributed locking first. A separate ten-minute HttpOnly SameSite=Lax correlation cookie permits the cross-site IAm callback. The server binds a one-use random state to the initiating browser, organization and production context; a new attempt or context change invalidates the old handoff. Failed callbacks return a clean error URL. Configure reverse-proxy access logs to omit callback query strings, since IAm delivers an SLT there. The hosted IAm login is production-only; sandbox and Silicon token sign-in remain available without putting test keys in redirect URLs. Cookies and idle sessions expire after seven days; expired records are rejected on access. Encrypted expired files remain on disk until removed by host retention maintenance.

The gateway allows only public product routes, validates request origin/Host and requires a same-origin custom header for mutations. Backend permission checks remain authoritative. Test lifecycle operations always use the production context; sandbox root operations do not require a user login. Root keys are returned only when explicitly requested/created, and can be copied from a dismissible dialog. No application secret is needed in the browser or frontend host.

## Verification

`npm test` verifies browser callback correlation, missing/forged state, cross-browser rejection, replay rejection, superseded attempts, CSRF checks, clean redirects and token confidentiality. Real IAm token callback exchange and the hosted login HTML entry also passed HTTP verification. The full interactive browser flow was verified on 2026-09-06: Continue with IAm, continue as the signed-in Carbon, return to Remind with a clean URL and authenticated organization view. The IAM edge template now contains an exact local callback exception for the SSRF/RFI false positives; unrelated callback shapes remain blocked.

`npm run build` includes TypeScript checking and creates the optimized client and production Node server. Manual HTTP checks passed against the public backend using a real production Carbon and a linked test Silicon: login/logout, organization reads, sandbox isolation, root access without login, webhook configuration, reminder create/edit/bulk pause/resume/archive and history reads. The mutation smoke used a future-only test schedule and removed its webhook afterward; it did not test delivery to webhook. Production-server checks passed for HTML/assets, SPA routing, security headers and private-file isolation. Cross-origin requests and internal API paths were rejected. The browser workflow E2E results, two UI fixes and the unresolved webhook DNS delivery blocker are recorded in [the browser test report](docs/browser-e2e-2026-09-06.md). See the parent project's existing backend delivery E2E evidence for worker-to-webhook delivery.

This frontend has not been deployed by the build task.
