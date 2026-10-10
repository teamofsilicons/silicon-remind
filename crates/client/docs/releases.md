# Release Remind

Silicon Apps distributes Remind and keeps every installation up to date; `remind` never updates
itself. Silicons install it with `silicon-apps install remind` (development releases:
`silicon-apps install 'remind>dev'`), and the Silicon Apps updater moves installed copies to new
releases within about a minute. The Rust client is an ordinary Cargo dependency, published to
crates.io separately.

## What a release is

The release version is the CLI version in `crates/cli/Cargo.toml` (0.6.0 at the move to
Silicon Accounts). Each target ships as its own archive,
`dist/apps/remind-<version>-<target>.tar.gz`, holding the binary at `bin/remind`
(`bin/remind.exe` on Windows) and an `apps.yaml` that lists only that target:

```yaml
schema_version: 1
app_id: remind
version: 0.6.0
command: remind
targets:
  linux-x86_64:
    binary: bin/remind
```

Remind ships linux-x86_64 and linux-aarch64 (what the Silicon fleet runs; built with a glibc
2.28 baseline), macos-x86_64, macos-aarch64, windows-x86_64 and windows-aarch64. Only the Linux
validation workers are live on Silicon Apps today; the macOS and Windows archives are built and
kept for when their workers go live.

## Every binary must answer three commands

Silicon Apps runs these signed out, in a clean environment, on every target, and refuses a
package whose binary fails one of them. Check them yourself in an empty home:

```sh
mkdir -p /tmp/empty && env -i HOME=/tmp/empty SILICON_HOME=/tmp/empty ./remind --help
env -i HOME=/tmp/empty SILICON_HOME=/tmp/empty ./remind accounts --json      # contains "app_id":"remind"
env -i HOME=/tmp/empty SILICON_HOME=/tmp/empty ./remind login status --json  # {"authenticated":false}
```

All three exit 0, need no network, and write nothing.

## Build and pack

The release workflow (`.github/workflows/release.yml`) builds and tests each target on a native
runner, then packs one archive per target with `scripts/package-apps.sh <version> <target>
<binary>`. The script stages `apps.yaml` and the binary, refuses a binary that fails the three
commands above (where the runner can execute it), and runs `silicon-apps validate` and
`silicon-apps pack` (the packer comes from `cargo install --locked silicon-apps-cli@0.2.0`).
The archives and their `SHA256SUMS` are uploaded as the workflow artifact
`remind-silicon-apps-release`. The workflow publishes nothing.

## Publish (a Carbon's step)

An author of the `remind` app uploads each archive to Silicon Apps, creates a development
release from the packages, checks it, then promotes it to production:

```sh
silicon-apps upload remind --target linux-x86_64 dist/apps/remind-0.6.0-linux-x86_64.tar.gz
silicon-apps upload remind --target linux-aarch64 dist/apps/remind-0.6.0-linux-aarch64.tar.gz
silicon-apps packages remind
silicon-apps release remind --version 0.6.0 --package <package_id> --package <package_id>
silicon-apps promote remind <release_id> --version 0.6.0
```

Publish the crates in dependency order: `silicon-remind-client`, then `silicon-remind-cli`.
