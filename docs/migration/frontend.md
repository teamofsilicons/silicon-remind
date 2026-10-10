# Remind frontend migration proof — 2026-10-10

The SolidJS/IAM frontend has been replaced in `frontend/` with Next.js 16, React 19 and the shared Arc UI. Hosted Accounts sign-in uses PKCE and sealed httpOnly sessions. The version-2 BFF injects bearer tokens and environment keys on the server; no app secret or token is bundled for the browser.

Implemented: Silicon reminder create/edit, current/archive filters and pagination, batch pause/resume, hold-to-archive, execution history, visible Silicon directory, viewer grants/revocation and outside-circle allowances, webhook subscriptions, test environment creation/selection/key reveal/rotation/retirement/restore/data clear, readiness, reports and sanitized Space Station telemetry with opt-out. Backend validation and cap errors remain visible. Canonical 36-character account UUIDs and legacy subjects are accepted at session/environment boundaries.

Verification against real Accounts 9590/9589 and Remind 4181:

- Typecheck, strict ESLint, production build, and all 49 unit checks pass.
- All 26 browser checks pass, including hosted email sign-in, session refresh and replay, cross-site protections, Carbon environment lifecycle and native Silicon reminder/share/webhook/archive journeys. The selected-key rotation scenario confirms an expired environment key returns recoverable 409 and preserves the Accounts sign-in.
- All five screenshot checks pass (setup plus four light/dark desktop/phone variants). Each variant checks seven populated pages/dialogs with axe WCAG 2.2 AA, horizontal overflow and modal viewport containment. The four images in `screens/` were visually reviewed.
- Local logs: `.mig/frontend-unit.log`, `frontend-build.log`, `frontend-final-browser.log` (26 functional successes; its initial screenshot attempt stopped on the backend UUID filter defect), and final `frontend-screens.log` (5/5). Full image set: `frontend/test-results/screens/`.

The populated suite found and led to fixes for row spacing, avatar initials accessibility and the backend's UUID reminder filter. The root backend change supplies the filter fix; no backend source is part of the frontend implementation.

Deployment preparation includes an unprivileged standalone Next Docker image, pnpm quality gates and ARM64 deployment-image workflow pointing to `frontend/Dockerfile`. Docker is not available locally, so the image build remains a CI gate. Nothing was pushed, published or deployed. Production credentials and browser auth fixtures are not committed.
