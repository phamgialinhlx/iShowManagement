#!/usr/bin/env bash
#
# Cross-compile the rmux agent for every target rmux can install it on.
#
# The agent is what makes terminals survive: it holds the PTY on the far side, so
# a session outlives the SSH connection, the app, and the laptop lid. rmux uploads
# it to a host on first use, which means these builds have to exist before a
# remote terminal can be persistent.
#
# Static musl, deliberately. A glibc build carries a minimum-version requirement
# that shows up as "GLIBC_2.34 not found" on exactly the older servers most worth
# keeping a session alive on. A static binary has no such dependency and runs on
# every Linux from a decade-old CentOS to current Debian.
#
# Needs: `cargo install cargo-zigbuild` and zig (`brew install zig`).

set -euo pipefail

cd "$(dirname "$0")/.."

# Windows is `gnu`, not `msvc`, so it cross-compiles from a Mac with no Visual
# Studio — and it is a *native* Win32 binary rather than an MSYS one, because the
# daemon has to outlive the SSH connection that started it and a process tied to
# the MSYS runtime is a worse bet than a plain Win32 one.
TARGETS=(
  x86_64-unknown-linux-musl
  aarch64-unknown-linux-musl
  x86_64-pc-windows-gnu
)

OUT="src-tauri/agents"
mkdir -p "$OUT"

if ! cargo zigbuild --help >/dev/null 2>&1; then
  echo "cargo-zigbuild is not installed." >&2
  echo "  cargo install cargo-zigbuild && brew install zig" >&2
  exit 1
fi

for target in "${TARGETS[@]}"; do
  echo "==> $target"
  rustup target add "$target" >/dev/null 2>&1 || true
  cargo zigbuild -p rmux-agent --bin rmux-agent --release --target "$target"
  # Windows produces an `.exe`; the resource keeps the plain name so
  # `provision::agent_for` can look every target up the same way, and the
  # extension is added when it is installed on the host — Windows will not
  # execute a file without one.
  if [ -f "target/$target/release/rmux-agent.exe" ]; then
    cp "target/$target/release/rmux-agent.exe" "$OUT/rmux-agent-$target"
  else
    cp "target/$target/release/rmux-agent" "$OUT/rmux-agent-$target"
  fi
done

# The agent for *this* machine, used for local sessions. Native build — no zig
# involved, and no cross-compilation to get wrong.
echo "==> host"
cargo build -p rmux-agent --bin rmux-agent --release
cp target/release/rmux-agent "$OUT/rmux-agent"

echo
ls -la "$OUT"
