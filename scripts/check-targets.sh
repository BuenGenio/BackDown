#!/usr/bin/env bash
# Type-check and lint the workspace for the systems this machine can't run BackDown on: Windows
# (x86-64, the GNU target through zig: the same Rust code and Win32 calls as CI's MSVC build) and
# macOS (arm64), with the same `-D warnings` as a native `cargo clippy --all-targets`. Tests are
# compiled but nothing is linked or run. scripts/build-local.sh runs this before it builds.
#
#   scripts/check-targets.sh              # both
#   scripts/check-targets.sh windows      # or: macos
#
# Needs rustup, cargo-zigbuild and zig, as build-local.sh does (zig compiles the bundled SQLite for
# each target). On a machine short of memory, set CARGO_BUILD_JOBS (e.g. 3).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
for d in "$HOME/.cargo/bin" "$HOME/.local/bin"; do
  [ -d "$d" ] && case ":$PATH:" in *":$d:"*) ;; *) PATH="$d:$PATH" ;; esac
done
export PATH

die() { echo "check-targets: $*" >&2; exit 1; }
command -v cargo-zigbuild >/dev/null || die "cargo-zigbuild not found: cargo install --locked cargo-zigbuild"
command -v zig >/dev/null || die "zig not found (see scripts/build-local.sh)"

want=("$@")
[ ${#want[@]} -gt 0 ] || want=(windows macos)
for w in "${want[@]}"; do
  case $w in
    windows) target=x86_64-pc-windows-gnu ;;
    macos) target=aarch64-apple-darwin ;;
    *) die "unknown system $w (windows or macos)" ;;
  esac
  rustup target add "$target" 2>&1 | { grep -v 'is up to date' || true; }
  printf '\n==> cargo-zigbuild clippy --all-targets --target %s -- -D warnings\n' "$target"
  cargo-zigbuild clippy --locked --all-targets --target "$target" -- -D warnings
done
echo "check-targets: clean for ${want[*]}"
