#!/usr/bin/env python3
"""Build Remind CLI release binaries on this machine and pack one Silicon Apps archive per target.

    python3 scripts/build-release.py                       # all six targets (macOS needed for the macOS ones)
    python3 scripts/build-release.py --targets linux-x86_64,linux-aarch64

The release workflow (.github/workflows/release.yml) is the portable path: it builds each target on its own runner.
This script is the same build from one machine, for when a release must be checked or rebuilt locally:

- macOS targets with Xcode's toolchain (`cargo build`; only on macOS);
- Linux targets with cargo-zigbuild at the glibc 2.28 baseline (`--target <triple>.2.28`), as the workflow does;
- Windows targets with cargo-xwin (needs clang-cl and lld-link, for example Homebrew's llvm and lld).

Every binary then goes through scripts/package-apps.sh's checks and `silicon-apps validate`/`pack`, and
dist/apps/SHA256SUMS lists the archives. Discovery commands run for the targets this machine can execute. Nothing is
uploaded or published.
"""

import argparse
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import package_apps  # noqa: E402  (the packager next to this script)

ROOT = package_apps.ROOT
TRIPLES = {
    "linux-x86_64": "x86_64-unknown-linux-gnu",
    "linux-aarch64": "aarch64-unknown-linux-gnu",
    "windows-x86_64": "x86_64-pc-windows-msvc",
    "windows-aarch64": "aarch64-pc-windows-msvc",
    "macos-x86_64": "x86_64-apple-darwin",
    "macos-aarch64": "aarch64-apple-darwin",
}
GLIBC = ".".join(map(str, package_apps.GLIBC_BASELINE))


def run(command, env=None):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def find_zig(explicit):
    for candidate in (explicit, os.environ.get("CARGO_ZIGBUILD_ZIG_PATH"), shutil.which("zig"), shutil.which("python-zig")):
        if candidate and Path(candidate).is_file():
            return candidate
    raise SystemExit("Linux targets need Zig 0.15: pass --zig PATH (the zig binary, or python-zig from `pip install "
                     "ziglang==0.15.2`)")


def default_bin(*candidates):
    return next((Path(c) for c in candidates if Path(c).is_dir()), None)


def build(family, targets, build_root, jobs, zig, llvm_bin, lld_bin):
    env = os.environ.copy()
    triples = [TRIPLES[target] for target in targets if target.startswith(family + "-")]
    if not triples:
        return
    if family == "linux":
        env["CARGO_ZIGBUILD_ZIG_PATH"] = find_zig(zig)
        command = ["cargo", "zigbuild"]
        triples = [f"{triple}.{GLIBC}" for triple in triples]
    elif family == "windows":
        tools = [str(path) for path in (llvm_bin, lld_bin) if path]
        env["PATH"] = os.pathsep.join(tools + [env.get("PATH", "")])
        command = ["cargo", "xwin", "build"]
    else:
        if platform.system() != "Darwin":
            raise SystemExit("macOS targets build only on macOS; use the release workflow or pick other --targets")
        command = ["cargo", "build"]
    command += ["--release", "--locked", "-p", "silicon-remind-cli", "--bin", "remind",
                "--target-dir", str(build_root / family), "--jobs", str(jobs)]
    for triple in triples:
        command += ["--target", triple]
    run(command, env)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--targets", default=",".join(TRIPLES),
                        help=f"comma-separated targets (default: all of {', '.join(TRIPLES)})")
    parser.add_argument("--package-only", action="store_true", help="pack binaries already built under --build-root")
    parser.add_argument("--build-root", type=Path, default=ROOT / "target" / "apps-release")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist" / "apps")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--zig", help="Zig executable for the Linux targets (zig, or python-zig)")
    parser.add_argument("--llvm-bin", type=Path, default=default_bin("/opt/homebrew/opt/llvm/bin", "/usr/local/opt/llvm/bin"),
                        help="directory with clang-cl (Windows targets)")
    parser.add_argument("--lld-bin", type=Path, default=default_bin("/opt/homebrew/opt/lld/bin", "/usr/local/opt/lld/bin"),
                        help="directory with lld-link (Windows targets)")
    parser.add_argument("--silicon-apps", help="the silicon-apps executable (default: SILICON_APPS, then PATH)")
    args = parser.parse_args()
    targets = [target.strip() for target in args.targets.split(",") if target.strip()]
    unknown = [target for target in targets if target not in TRIPLES]
    if unknown or not targets:
        raise SystemExit(f"unknown targets {unknown}; Remind ships {', '.join(TRIPLES)}")
    if args.jobs < 1:
        raise SystemExit("--jobs must be positive")
    build_root = args.build_root.resolve()
    if not args.package_only:
        for family in ("macos", "linux", "windows"):
            build(family, targets, build_root, args.jobs, args.zig, args.llvm_bin, args.lld_bin)
    version = package_apps.cli_version()
    output = args.output_dir.resolve()
    archives = []
    for target in targets:
        binary = build_root / target.split("-", 1)[0] / TRIPLES[target] / "release" / package_apps.binary_name(target)
        try:
            archives.append(package_apps.package(version, target, binary, output, "auto", args.silicon_apps))
        except package_apps.PackageError as error:
            raise SystemExit(f"package-apps: {error}")
    sums = output / "SHA256SUMS"
    sums.write_text("".join(f"{package_apps.sha256(path)}  {path.name}\n" for path in sorted(archives)),
                    encoding="utf-8", newline="\n")
    print(f"wrote {sums}:\n{sums.read_text()}", end="")


if __name__ == "__main__":
    main()
