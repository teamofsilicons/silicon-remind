# Build a Remind release

Install the CLI with `honeycomb install 'tos>remind'`, then sign in with `remind login '<IAM-SLT>'`. Honeycomb owns installation and updates. The Rust library remains an ordinary Cargo dependency.

The app release version is the CLI version in `crates/cli/Cargo.toml`; `honeycomb.yaml` must match it. The manifest maps `remind` to prebuilt executables for Linux, Windows and macOS on x86_64 and aarch64.

The release workflow builds and tests each target on a native runner. It stages only the six binaries and root manifest, runs `honeycomb validate`, then `honeycomb pack`, and validates the final archive. It produces one `remind-<version>.tar.gz` plus SHA-256 checksums as reviewable workflow artifacts. It does not publish them automatically.

On macOS, the local builder can compile all six targets using Xcode, Rust's six target libraries, `cargo-zigbuild` plus Zig, and `cargo-xwin` plus LLVM/LLD:

```sh
python3 scripts/build-honeycomb-release.py --honeycomb /path/to/honeycomb \
  --zig /path/to/zig --llvm-bin /path/to/llvm/bin --lld-bin /path/to/lld/bin
```

It populates `targets/<platform>/bin/remind[.exe]` beside the root manifest, validates that populated directory, and creates the single release archive. `--stage-only` reuses completed builds in `target/honeycomb`. Generated executables and archives stay out of Git; the manifest, build scripts, and verification record are committed.

To pack an already populated target tree:

```sh
python3 scripts/package-release.py --target-tree targets --honeycomb /path/to/honeycomb
```

For other local packaging, place release binaries under `target/<Rust-triple>/release/remind[.exe]`, or supply downloaded CI artifacts:

```sh
python3 scripts/package-release.py --binaries-dir release-binaries
```

The script checks native formats and architectures and refuses incomplete releases. It never substitutes scripts, source archives or one platform's binary for another. Cross-compilation verifies each target's build and executable format. Run native smoke tests for each platform before publishing; cross-compilation alone does not prove runtime behavior on Windows. See [Honeycomb package documentation](https://docs.honeycomb.teamofsilicons.com/).
