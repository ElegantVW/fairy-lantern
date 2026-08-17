#!/usr/bin/env bash
# build.sh — release Fairy Lantern; install into faeOS engine paths
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
cargo build --release
BIN="$ROOT/target/release/fairy"
BIN_ALT="$ROOT/target/release/fairy-lantern"
echo "built: $BIN"
ls -la "$BIN" "$BIN_ALT" 2>/dev/null || ls -la "$BIN"

if [[ "${1:-}" == "install" ]]; then
  LIB="$HOME/.local/lib/faeos"
  WRAP_SRC="$ROOT/scripts/fairy"
  mkdir -p "$LIB" "$HOME/bin"
  cp -f "$BIN" "$LIB/fairy"
  chmod +x "$LIB/fairy"
  # same binary dual-named for fairy-lantern CLI entry
  cp -f "$BIN_ALT" "$LIB/fairy-lantern" 2>/dev/null || cp -f "$BIN" "$LIB/fairy-lantern"
  chmod +x "$LIB/fairy-lantern"

  install_launcher() {
    local dest="$1"
    mkdir -p "$(dirname "$dest")"
    if [[ ! -e "$dest" ]]; then
      cp -f "$WRAP_SRC" "$dest"
      chmod +x "$dest"
      return
    fi
    if file -b "$dest" 2>/dev/null | grep -q ELF; then
      cp -f "$WRAP_SRC" "$dest"
      chmod +x "$dest"
    fi
  }

  install_launcher "$HOME/bin/fairy"
  install_launcher "$HOME/bin/fairy-lantern"
  if [[ -d "$HOME/faeos/bin" ]]; then
    install_launcher "$HOME/faeos/bin/fairy"
    install_launcher "$HOME/faeos/bin/fairy-lantern"
  fi

  echo "installed engine → $LIB/fairy (+ fairy-lantern)"
  echo "launcher        → $HOME/bin/fairy (thin script; rebuilds tree when sources newer)"
fi
