#!/usr/bin/env bash
set -euo pipefail

# ------ variables ------

RH_REPO="DavidMANZI-093/rabbit-hole"
RH_PREFIX="${RH_PREFIX:-$HOME/.local}"
WITH_CLOUDFLARED="${WITH_CLOUDFLARED:-1}"
RH_DRY_RUN="${RH_DRY_RUN:-0}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"

# ------ helpers ------

log() { printf '  %s\n' "$*"; }
run() { if [ "$RH_DRY_RUN" = "1" ]; then printf '  + %s\n' "$*"; else "$@"; fi; }
need() { command -v "$1" >/dev/null 2>&1 || {
  echo "error: need '$1' on PATH" >&2
  exit 1
}; }

need curl
need unzip

# ------ platform detection ------

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS/$ARCH" in
Linux/x86_64)
  TARGET="x86_64-unknown-linux-gnu"
  CF_ASSET="cloudflared-linux-amd64"
  ;;
Linux/aarch64 | Linux/arm64)
  TARGET="aarch64-unknown-linux-gnu"
  CF_ASSET="cloudflared-linux-arm64"
  ;;
Darwin/x86_64)
  TARGET="x86_64-apple-darwin"
  CF_ASSET="cloudflared-darwin-amd64.tgz"
  ;;
Darwin/aarch64 | Darwin/arm64)
  TARGET="aarch64-apple-darwin"
  CF_ASSET="cloudflared-darwin-arm64.tgz"
  ;;
*)
  echo "error: unsupported platform $OS/$ARCH" >&2
  exit 1
  ;;
esac

# ------ version resolution ------

if [ -z "${RH_VERSION:-}" ]; then
  log "resolving latest rh release..."
  RH_VERSION="$(curl -fsSL "https://api.github.com/repos/$RH_REPO/releases/latest" | grep -m1 '"tag_name"' | cut -d'"' -f4)"
  [ -n "$RH_VERSION" ] || {
    echo "error: could not resolve latest release (set RH_VERSION=vX.Y.Z)" >&2
    exit 1
  }
fi
log "rh $RH_VERSION for $TARGET"
log "config: prefix=$RH_PREFIX cloudflared=$WITH_CLOUDFLARED dry-run=$RH_DRY_RUN"

# ------ tempory working directory ------

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ------ installation (rh & cloudflared) ------

# rh binary (skipped when the installed one already matches the target)
INSTALLED_RH=""
if [ -x "$RH_PREFIX/bin/rh" ]; then
  INSTALLED_RH="$("$RH_PREFIX/bin/rh" --version 2>/dev/null | awk '{print $2}')" || INSTALLED_RH=""
fi
if [ -n "$INSTALLED_RH" ] && [ "v$INSTALLED_RH" = "$RH_VERSION" ]; then
  log "rh $RH_VERSION already installed — skipping"
else
  log "downloading rh $RH_VERSION..."
  run curl -fsSL -o "$TMP/rh.zip" "https://github.com/$RH_REPO/releases/download/$RH_VERSION/rh-$RH_VERSION-$TARGET.zip"
  run mkdir -p "$RH_PREFIX/bin"
  run unzip -o -q "$TMP/rh.zip" -d "$TMP/rh-out"
  run install -m 755 "$TMP/rh-out/rh" "$RH_PREFIX/bin/rh"
  log "installed $RH_PREFIX/bin/rh"
fi

# pinned cloudflared (versions/hashes from the pin file)
if [ "$WITH_CLOUDFLARED" = "1" ]; then
  PIN_URL="https://raw.githubusercontent.com/$RH_REPO/$RH_VERSION/third-party/cloudflared.pin"
  log "fetching pin file..."
  if [ "$RH_DRY_RUN" = "1" ]; then
    printf '  + curl -fsSL -o PIN %s\n' "$PIN_URL"
    CF_VERSION="<from pin>"
    CF_SHA="<from pin>"
  else
    curl -fsSL -o "$TMP/cloudflared.pin" "$PIN_URL" ||
      {
        echo "error: could not fetch $PIN_URL" >&2
        exit 1
      }
    CF_VERSION="$(grep -m1 '^PINNED=' "$TMP/cloudflared.pin" | cut -d= -f2 | tr -d '[:space:]')"
    CF_SHA="$(grep -m1 "^$CF_ASSET " "$TMP/cloudflared.pin" | awk '{print $2}')"
    [ -n "$CF_VERSION" ] && [ -n "$CF_SHA" ] ||
      {
        echo "error: pin file lacks PINNED= or hash for $CF_ASSET" >&2
        exit 1
      }
  fi
  log "downloading cloudflared $CF_VERSION ($CF_ASSET)..."
  INSTALLED_CF=""
  if [ -x "$DATA_DIR/rh/bin/cloudflared" ]; then
    INSTALLED_CF="$("$DATA_DIR/rh/bin/cloudflared" --version 2>/dev/null | grep -m1 -oE '[0-9]{4}\.[0-9]+\.[0-9]+')" || INSTALLED_CF=""
  fi
  if [ -n "$INSTALLED_CF" ] && [ "$INSTALLED_CF" = "$CF_VERSION" ]; then
    log "cloudflared $CF_VERSION already installed — skipping"
  else
    run curl -fsSL -o "$TMP/cf-asset" "https://github.com/cloudflare/cloudflared/releases/download/$CF_VERSION/$CF_ASSET"
    case "$CF_ASSET" in
    *.tgz)
      CF_BIN="$TMP/cf-asset-bin"
      run mkdir -p "$CF_BIN"
      run tar -xzf "$TMP/cf-asset" -C "$CF_BIN"
      CF_SRC="$CF_BIN/cloudflared"
      ;;
    *) CF_SRC="$TMP/cf-asset" ;;
    esac
    if [ "$RH_DRY_RUN" = "1" ]; then
      printf '  + verify sha256 %s\n' "$CF_SHA"
    else
      if command -v sha256sum >/dev/null 2>&1; then
        echo "$CF_SHA  $CF_SRC" | sha256sum -c - ||
          {
            echo "error: cloudflared checksum mismatch" >&2
            exit 1
          }
      else
        echo "$CF_SHA  $CF_SRC" | shasum -a 256 -c - ||
          {
            echo "error: cloudflared checksum mismatch" >&2
            exit 1
          }
      fi
    fi
    run mkdir -p "$DATA_DIR/rh/bin"
    run install -m 755 "$CF_SRC" "$DATA_DIR/rh/bin/cloudflared"
    log "installed $DATA_DIR/rh/bin/cloudflared"
  fi
else
  log "skipping cloudflared (WITH_CLOUDFLARED=0); rh will use PATH or LAN-only"
fi

case ":$PATH:" in
*":$RH_PREFIX/bin:"*) ;;
*) log "add to PATH, e.g.:  export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac
log "to uninstall later: curl -fsSL https://raw.githubusercontent.com/$RH_REPO/$RH_VERSION/uninstall.sh | sh"
log "done. Run:  rh check"
