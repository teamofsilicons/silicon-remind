# Remind documentation site

Run `npm ci`, `npm run build` and `npm run check`. The build renders the manuals in `docs/` (except
`docs/history/` and `docs/migration/`, which stay in the repository), `openapi.yaml`, the search index, the installer
(`install.sh`, which installs Remind with Silicon Apps) and the public client and CLI source bundle into `dist/`; the
check verifies every page's canonical host and every local link and anchor. No application credentials are needed.

`dist/` is served with directory indexes at https://docs.remind.teamofsilicons.com from the Remind host: a static
container serves `/var/lib/remind-docs/current`, a relative symlink to `releases/<git sha>` holding one build.
Publishing is copying a new build into `releases/<git sha>` and moving the symlink; moving it back rolls back. There
is no checked-in publishing script: the 0.3.0 [deployment record](../docs/history/deploy/timezone-2026-09-20.md)
shows how the last build was carried to the host and selected.
