#!/usr/bin/env bash
set -euo pipefail

RH_PREFIX="${RH_PREFIX:-$HOME/.local}"
RH_DRY_RUN="${RH_DRY_RUN:-0}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"

log() { printf '  %s\n' "$*"; }
run() { if [ "$RH_DRY_RUN" = "1" ]; then printf '  + %s\n' "$*"; else "$@"; fi; }

if [ -e "$RH_PREFIX/bin/rh" ] || [ -L "$RH_PREFIX/bin/rh" ]; then
  run rm -f "$RH_PREFIX/bin/rh"
  log "removed $RH_PREFIX/bin/rh"
else
  log "no rh binary at $RH_PREFIX/bin/rh"
fi

if [ -d "$DATA_DIR/rh" ]; then
  run rm -rf "$DATA_DIR/rh"
  log "removed $DATA_DIR/rh (pinned cloudflared)"
else
  log "no data dir at $DATA_DIR/rh"
fi

log "done. (Installers never touch system paths, nothing else to clean.)"
