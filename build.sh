#!/usr/bin/env bash
# build.sh — release Fairy Lantern
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
cargo build --release
BIN="$ROOT/target/release/fairy-lantern"
echo "built: $BIN"
ls -la "$BIN"
if [[ "${1:-}" == "install" ]]; then
  mkdir -p "$ROOT/../bin" "$HOME/bin"
  for name in fairy-lantern fairy; do
    SRC="$ROOT/target/release/$name"
    # both bins share the same code; fairy is a second Cargo bin
    if [[ ! -x $SRC ]]; then
      SRC="$BIN"
    fi
    cp -f "$SRC" "$ROOT/../bin/$name"
    cp -f "$SRC" "$HOME/bin/$name"
    chmod +x "$ROOT/../bin/$name" "$HOME/bin/$name"
  done
  echo "installed → $HOME/bin/fairy-lantern and $HOME/bin/fairy"
fi
