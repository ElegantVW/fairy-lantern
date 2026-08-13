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
  WRAP="$ROOT/scripts/fairy"
  chmod +x "$WRAP"
  mkdir -p "$ROOT/../bin" "$HOME/bin"
  for dest in "$ROOT/../bin" "$HOME/bin"; do
    cp -f "$WRAP" "$dest/fairy"
    cp -f "$WRAP" "$dest/fairy-lantern"
    chmod +x "$dest/fairy" "$dest/fairy-lantern"
  done
  if [[ -d "$HOME/faeos/bin" ]]; then
    cp -f "$WRAP" "$HOME/faeos/bin/fairy"
    cp -f "$WRAP" "$HOME/faeos/bin/fairy-lantern"
    chmod +x "$HOME/faeos/bin/fairy" "$HOME/faeos/bin/fairy-lantern"
  fi
  echo "installed wrapper → $HOME/bin/fairy (rebuilds $ROOT when src is newer)"
fi
