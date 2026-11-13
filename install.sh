#!/usr/bin/env bash

if [[ -z "${BASH_VERSION:-}" ]]; then
  echo "error: run this installer with bash" >&2
  exit 1
fi

set -euo pipefail

# ------ variables ------

RH_REPO="DavidMANZI-093/rabbit-hole"
RH_PREFIX="${RH_PREFIX:-$HOME/.local}"
WITH_CLOUDFLARED="${WITH_CLOUDFLARED:-1}"
RH_DRY_RUN="${RH_DRY_RUN:-0}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"

# ------ helpers ------

log() { printf '  %s\n' "$*"; }
run() { if [[ "$RH_DRY_RUN" = "1" ]]; then printf '  + %s\n' "$*"; else "$@"; fi; }
need() { command -v "$1" >/dev/null 2>&1 || {
  echo "error: need '$1' on PATH" >&2
  exit 1
}; }

need curl

# Every request: https only, bounded connect time, retries on transient errors,
# so a blocked or flaky host fails in seconds instead of hanging for minutes.
CURL=(curl -fsSL --proto '=https' --proto-redir '=https' --connect-timeout 8 --retry 2 --retry-delay 1)

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "error: need 'sha256sum' or 'shasum' on PATH" >&2
    return 1
  fi
}

verify_sha256() {
  local actual
  actual="$(sha256_of "$1")" || exit 1
  [[ "$actual" = "$2" ]] || {
    echo "error: $3 checksum mismatch (expected $2, got $actual)" >&2
    exit 1
  }
}

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

# github.com, api.github.com and the release-asset CDN are IPv4-only, so an
# IPv6-only host (or broken IPv4 NAT) can fetch this script yet nothing else.
GH_UNREACHABLE="cannot reach github.com:443. GitHub release downloads are IPv4-only: check this machine's IPv4 connectivity, or set https_proxy"

# Primary: the github.com "latest release" redirect (no API, no rate limit).
# Fallback: the REST API. Either way the result must look like a tag.
resolve_latest() {
  local url tag="" rc=0
  url="$(curl -sS --proto '=https' --connect-timeout 8 --retry 2 --retry-delay 1 \
    -o /dev/null -w '%{redirect_url}' "https://github.com/$RH_REPO/releases/latest")" || rc=$?

  if [[ $rc -eq 6 || $rc -eq 7 || $rc -eq 28 ]]; then
    echo "error: $GH_UNREACHABLE" >&2
    return 2
  fi
  [[ "$url" = */releases/tag/* ]] && tag="${url##*/releases/tag/}"
  if [[ ! "$tag" =~ ^v[0-9] ]]; then
    tag="$("${CURL[@]}" "https://api.github.com/repos/$RH_REPO/releases/latest" |
      awk -F'"' '/"tag_name"/ && !seen { tag = $4; seen = 1 } END { print tag }')" || tag=""
  fi
  [[ "$tag" =~ ^v[0-9] ]] && printf '%s' "$tag"
}

if [[ -z "${RH_VERSION:-}" ]]; then
  log "resolving latest rh release..."
  rc=0
  RH_VERSION="$(resolve_latest)" || rc=$?
  [[ $rc -ne 2 ]] || exit 1
  [[ -n "$RH_VERSION" ]] || {
    echo "error: could not resolve latest release from github.com (set RH_VERSION=vX.Y.Z)" >&2
    exit 1
  }
fi

[[ "$RH_VERSION" = v* ]] || RH_VERSION="v$RH_VERSION"
log "rh $RH_VERSION for $TARGET"
log "config: prefix=$RH_PREFIX cloudflared=$WITH_CLOUDFLARED dry-run=$RH_DRY_RUN"

# ------ temporary working directory ------

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ------ installation (rh & cloudflared) ------

# rh binary (skipped when the installed one already matches the target)
INSTALLED_RH=""
if [[ -x "$RH_PREFIX/bin/rh" ]]; then
  INSTALLED_RH="$("$RH_PREFIX/bin/rh" --version 2>/dev/null | awk '{print $2}')" || INSTALLED_RH=""
fi
if [[ -n "$INSTALLED_RH" ]] && [[ "v$INSTALLED_RH" = "$RH_VERSION" ]]; then
  log "rh $RH_VERSION already installed — skipping"
else
  log "downloading rh $RH_VERSION..."
  REL_URL="https://github.com/$RH_REPO/releases/download/$RH_VERSION"
  ASSET="rh-$RH_VERSION-$TARGET"
  if [[ "$RH_DRY_RUN" = "1" ]]; then
    printf '  + curl %s/%s.tar.gz\n' "$REL_URL" "$ASSET"
    printf '  + verify sha256 from %s/SHA256SUMS (when published)\n' "$REL_URL"
    printf '  + install -m 755 rh %s/bin/rh\n' "$RH_PREFIX"
  else
    ARCHIVE="$ASSET.tar.gz"
    rc=0
    "${CURL[@]}" -o "$TMP/$ARCHIVE" "$REL_URL/$ARCHIVE" || rc=$?
    if [[ $rc -eq 6 || $rc -eq 7 || $rc -eq 28 ]]; then
      echo "error: $GH_UNREACHABLE" >&2
      exit 1
    elif [[ $rc -ne 0 ]]; then
      echo "error: no rh $RH_VERSION build for $TARGET at $REL_URL (does the release exist?)" >&2
      exit 1
    fi
    if "${CURL[@]}" -o "$TMP/SHA256SUMS" "$REL_URL/SHA256SUMS" 2>/dev/null; then
      RH_SHA="$(awk -v f="$ARCHIVE" '$2 == f || $2 == "*" f { print $1; exit }' "$TMP/SHA256SUMS")"
      [[ -n "$RH_SHA" ]] || {
        echo "error: SHA256SUMS has no entry for $ARCHIVE" >&2
        exit 1
      }
      verify_sha256 "$TMP/$ARCHIVE" "$RH_SHA" "rh"
    else
      log "warning: $RH_VERSION publishes no SHA256SUMS; skipping rh checksum"
    fi
    mkdir -p "$TMP/rh-out" "$RH_PREFIX/bin"
    tar -xzf "$TMP/$ARCHIVE" -C "$TMP/rh-out"
    install -m 755 "$TMP/rh-out/rh" "$RH_PREFIX/bin/rh"
  fi
  log "installed $RH_PREFIX/bin/rh"
fi

# pinned cloudflared (versions/hashes from the pin file)
if [[ "$WITH_CLOUDFLARED" = "1" ]]; then
  PIN_URL="https://raw.githubusercontent.com/$RH_REPO/$RH_VERSION/third-party/cloudflared.pin"
  log "fetching pin file..."
  if [[ "$RH_DRY_RUN" = "1" ]]; then
    printf '  + curl -fsSL --proto "=https" --proto-redir "=https" -o PIN %s\n' "$PIN_URL"
    CF_VERSION="<from pin>"
    CF_SHA="<from pin>"
  else
    "${CURL[@]}" -o "$TMP/cloudflared.pin" "$PIN_URL" ||
      {
        echo "error: could not fetch $PIN_URL" >&2
        exit 1
      }
    CF_VERSION="$(grep -m1 '^PINNED=' "$TMP/cloudflared.pin" | cut -d= -f2 | tr -d '[:space:]')"
    CF_SHA="$(grep -m1 "^$CF_ASSET " "$TMP/cloudflared.pin" | awk '{print $2}')"
    [[ -n "$CF_VERSION" ]] && [[ -n "$CF_SHA" ]] ||
      {
        echo "error: pin file lacks PINNED= or hash for $CF_ASSET" >&2
        exit 1
      }
  fi
  log "downloading cloudflared $CF_VERSION ($CF_ASSET)..."
  INSTALLED_CF=""
  if [[ -x "$DATA_DIR/rh/bin/cloudflared" ]]; then
    INSTALLED_CF="$("$DATA_DIR/rh/bin/cloudflared" --version 2>/dev/null | grep -m1 -oE '[0-9]{4}\.[0-9]+\.[0-9]+')" || INSTALLED_CF=""
  fi
  if [[ -n "$INSTALLED_CF" ]] && [[ "$INSTALLED_CF" = "$CF_VERSION" ]]; then
    log "cloudflared $CF_VERSION already installed — skipping"
  else
    run "${CURL[@]}" -o "$TMP/cf-asset" "https://github.com/cloudflare/cloudflared/releases/download/$CF_VERSION/$CF_ASSET"
    case "$CF_ASSET" in
    *.tgz)
      CF_BIN="$TMP/cf-asset-bin"
      run mkdir -p "$CF_BIN"
      run tar -xzf "$TMP/cf-asset" -C "$CF_BIN"
      CF_SRC="$CF_BIN/cloudflared"
      ;;
    *) CF_SRC="$TMP/cf-asset" ;;
    esac
    if [[ "$RH_DRY_RUN" = "1" ]]; then
      printf '  + verify sha256 %s\n' "$CF_SHA"
    else
      verify_sha256 "$CF_SRC" "$CF_SHA" "cloudflared"
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
*) log "add to PATH, e.g.:  export PATH=\"$RH_PREFIX/bin:\$PATH\"" ;;
esac
log "to uninstall later: curl -fsSL --proto '=https' --proto-redir '=https' https://raw.githubusercontent.com/$RH_REPO/$RH_VERSION/uninstall.sh | bash"
log "done. Run:  rh check"
