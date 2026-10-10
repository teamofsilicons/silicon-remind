# Silicon Remind web

Next.js 16, React 19 and Silicon UI. The workspace supports reminder creation and editing for Silicons; pause, resume and archive; delivery history; account-specific viewers and allowances; webhooks; and isolated test environments. Carbons see their custodial and shared Silicons and manage the authority the API grants them.

## Run locally

Use Node 24 and pnpm 10.33.0. Copy `.env.example` to `.env.local`, fill in the local Remind app secret registered with Accounts and a random session secret, then run:

```sh
pnpm install --frozen-lockfile
pnpm dev
```

The default frontend is `http://127.0.0.1:4180`. Start the real Remind stack from the repository root with `REMIND_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh --build --check-webhook`. The API is on 4181, worker health on 4182 and local webhook receiver on 4183. Accounts and its email fixture must already be running. Register `http://127.0.0.1:4180/auth/callback` and its logout origin for the Remind app in Accounts.

`APP_API_URL` includes `/api/v2`. Runtime settings are read by the server, so one build can serve local and production stacks. No app secret or bearer token is exposed to browser JavaScript. `.env.local`, browser auth fixtures and screenshots containing test data remain ignored.

## Sessions and test environments

The server performs public hosted Accounts sign-in with PKCE and keeps access and rotating refresh tokens in an authenticated, encrypted, httpOnly cookie. Secure deployments use `__Host-` cookies, same-origin mutation checks and a per-request CSP nonce. Refresh is single-flight inside one server process; use a single instance until refresh coordination is shared across replicas.

`/api/*` adds the bearer token and `X-Remind-API-Version: 2` on the server. Choosing a test environment retrieves its key with the signed-in account, seals it in an account-bound httpOnly cookie and reloads the page to discard drafts and cached results. The browser gets the environment name and id. The proxy injects the selected key; environment administration and sharing stay in Production. Sign-out clears the selection. Key rotation requires selecting the environment again. Explicit key reveal/copy remains available to authorized owners and custodians.

Mutation requests carry idempotency keys. Hold controls guard archive, purge, revoke, rotate and retire actions. Backend limit and validation errors stay visible next to the operation. The browser uses immutable Accounts UUIDs for authority and mutable public ids for display; both canonical UUIDs and transitional legacy identifiers are accepted.

Space Station telemetry starts only after sign-in and can be disabled in Settings. Its transport permits a small event vocabulary; it strips account ids, reminder text, destination URLs, credentials, raw errors and page queries.

## Verify

```sh
pnpm typecheck
pnpm lint
pnpm test
pnpm build
TEST_STACK_JSON=/path/to/test-stack.json REMIND_E2E_MINT=/path/to/mint.mts pnpm test:e2e
TEST_STACK_JSON=/path/to/test-stack.json REMIND_E2E_MINT=/path/to/mint.mts pnpm screens
```

The browser suite exercises hosted email sign-in, refresh/replay and cross-site protections, real Carbon test-environment administration and native Silicon reminder/share/webhook/archive flows. `mint.mts` is the shared isolated Accounts testkit. No production tokens are used. Screenshot runs seed real reminders, check populated pages with axe WCAG 2.2 AA and assert no horizontal overflow in light/dark desktop/phone views. Local artifacts are under `test-results/`; selected reviewed images are copied to `../docs/migration/screens/`.

## Deploy

From the repository root: `docker build -f frontend/Dockerfile -t silicon-remind-web frontend`. The image runs the standalone Next server as the unprivileged `node` account on port 3000. `.github/workflows/deployment-builds.yml` builds an ARM64 image from the exact revision. The existing AWS web installer consumes a digest-pinned image and persistent runtime secrets. Build and review the image before any separately authorized production cutover; local browser proof does not establish that the image was built or deployed.
