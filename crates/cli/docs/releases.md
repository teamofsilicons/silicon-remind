# Build a Remind release

Install the CLI with `honeycomb install 'tos>remind'`, then sign in with `remind login '<IAM-SLT>'`. Honeycomb owns installation and updates. The Rust library remains an ordinary Cargo dependency.

The app release version is the CLI version in `crates/cli/Cargo.toml`; `honeycomb.yaml` must match it. The manifest maps `remind` to prebuilt executables for Linux, Windows and macOS on x86_64 and aarch64.

The release workflow builds and tests each target on a native runner. It stages only the six binaries and root manifest, runs `honeycomb validate`, then `honeycomb pack`, and validates the final archive. It produces one `remind-<version>.tar.gz` plus SHA-256 checksums as reviewable workflow artifacts. It does not publish them automatically.

For local packaging, place release binaries under `target/<Rust-triple>/release/remind[.exe]`, or supply downloaded CI artifacts:

```sh
python3 scripts/package-release.py --binaries-dir release-binaries
```

The script checks native formats and architectures and refuses incomplete releases. It never substitutes scripts, source archives or one platform's binary for another. Run all six native jobs before publishing a release; a local build only verifies the current platform. See [Honeycomb package documentation](https://docs.honeycomb.teamofsilicons.com/).
