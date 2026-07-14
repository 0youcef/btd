#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
BIN_DIR="$PREFIX/bin"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

echo "building release binary..."
cargo build --release --locked

echo "installing to $BIN_DIR (needs sudo)..."
sudo install -Dm755 target/release/btd "$BIN_DIR/btd"

echo
echo "installed: $BIN_DIR/btd"
echo
echo "btd needs CAP_SYS_ADMIN to talk to the kernel's btrfs ioctls. Either:"
echo "  - run it with sudo each time: sudo btd /path/to/btrfs"
echo "  - or grant the capability once instead of using sudo:"
echo "      sudo setcap cap_sys_admin+ep $BIN_DIR/btd"
echo "    (be aware this lets any user who can run the binary use that capability)"
