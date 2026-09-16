#!/bin/sh
set -eu
case "$(uname -s)" in Darwin|Linux) ;; *) echo 'Use the source bundle and supervise `remind daemon run` on this platform.' >&2; exit 1;; esac
if ! command -v curl >/dev/null 2>&1; then echo 'Install curl first.' >&2; exit 1; fi
if ! command -v cc >/dev/null 2>&1; then
  if [ "$(uname -s)" = Darwin ]; then
    xcode-select --install
    echo 'Complete Command Line Tools installation, then rerun this installer.' >&2
    exit 1
  fi
  elevate=""
  if [ "$(id -u)" != 0 ]; then elevate=sudo; fi
  if command -v apt-get >/dev/null 2>&1; then
    $elevate apt-get update
    $elevate apt-get install -y build-essential pkg-config cmake ca-certificates
  elif command -v dnf >/dev/null 2>&1; then
    $elevate dnf install -y gcc gcc-c++ make pkgconf-pkg-config cmake ca-certificates
  elif command -v apk >/dev/null 2>&1; then
    $elevate apk add build-base pkgconf cmake ca-certificates
  else
    echo 'Install a C/C++ build toolchain for this Linux distribution, then rerun.' >&2
    exit 1
  fi
fi
if ! command -v rustup >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "$HOME/.cargo/env"
fi
rustup toolchain install 1.98.0 --profile minimal
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
curl --proto '=https' --tlsv1.2 -fsSL https://docs.remind.teamofsilicons.com/remind-client-source.tar.gz -o "$work/source.tar.gz"
curl --proto '=https' --tlsv1.2 -fsSL https://docs.remind.teamofsilicons.com/remind-client-source.sha256 -o "$work/source.sha256"
expected=$(cut -d ' ' -f 1 "$work/source.sha256")
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$work/source.tar.gz" | cut -d ' ' -f 1); else actual=$(shasum -a 256 "$work/source.tar.gz" | cut -d ' ' -f 1); fi
[ "$actual" = "$expected" ] || { echo 'Source checksum mismatch; installation stopped.' >&2; exit 1; }
tar -xzf "$work/source.tar.gz" -C "$work"
install_root=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}
cargo +1.98.0 install --path "$work/remind-client-source/crates/cli" --locked --root "$install_root" --force
"$install_root/bin/remind" daemon install
printf '\nInstalled Remind. Ensure %s/bin is on PATH. Next: remind iam --json, then remind login <SLT>.\n' "$install_root"
