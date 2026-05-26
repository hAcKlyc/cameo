#!/usr/bin/env bash
# publish_release.sh — sign + upload the macOS auto-update release to R2.
#
# macOS half of the release. Windows publishes independently from a Windows box
# via publish_release.ps1 — the two write DIFFERENT manifest files on R2
# (darwin-*.json here, windows-x86_64.json there), so independent, async uploads
# never clobber each other.
#
# Prerequisite:  ./build_release.sh            (default = arm + Intel, with updater)
#
# Usage:
#   ./publish_release.sh                       # sign + upload mac artifacts
#   ./publish_release.sh --dry-run             # print what would happen, upload nothing
#
# What it does:
#   1. Reads version from src-tauri/tauri.conf.json
#   2. For each arch (mac-arm, mac-intel):
#      - Locates the auto-update payload (.app.tar.gz)
#      - Reads/creates its .sig (signed at build time via TAURI_SIGNING_PRIVATE_KEY)
#      - Writes a manifest JSON ({version, notes, pub_date, signature, url})
#   3. rclone uploads to R2:
#      - .app.tar.gz + .sig + .dmg → release/v<version>/
#      - manifests → update/{darwin-aarch64,darwin-x86_64}.json
#
# R2 credentials come from .env (R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY /
# R2_ENDPOINT / R2_BUCKET). The same .env that hosts TAURI_SIGNING_PRIVATE_KEY
# also hosts these — re-uses the gallery upload creds.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

info() { printf '→ %s\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
die()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }
size_of() { ls -lh "$1" | awk '{print $5}'; }
tarball_short_version() {
  tar -xOzf "$1" Cameo.app/Contents/Info.plist 2>/dev/null \
    | plutil -extract CFBundleShortVersionString raw - 2>/dev/null || true
}
expected_dmg_name() {
  case "$1" in
    aarch64-apple-darwin) echo "Cameo_${2}_aarch64.dmg" ;;
    x86_64-apple-darwin) echo "Cameo_${2}_x64.dmg" ;;
    *) echo "" ;;
  esac
}

[[ "$(uname -s)" == "Darwin" ]] || die "macOS only — on Windows run publish_release.ps1"

DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run)   DRY_RUN=1 ;;
    -h|--help)   sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown flag: $arg" ;;
  esac
done

# ── load .env ────────────────────────────────────────────────────────────────
[[ -f .env ]] || die ".env not found — needed for R2 creds and signing key"
info "loading .env"
set -a; . ./.env; set +a

for v in R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY R2_ENDPOINT R2_BUCKET; do
  [[ -n "${!v:-}" ]] || die "$v missing from .env"
done

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  warn "TAURI_SIGNING_PRIVATE_KEY not set — publish will fail unless non-empty .sig files already exist"
else
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD-}"
fi

command -v rclone >/dev/null || die "rclone not on PATH — brew install rclone"

# ── version ─────────────────────────────────────────────────────────────────
pkg_ver=$(node -p "require('./package.json').version" 2>/dev/null || echo "?")
conf_ver=$(node -p "require('./src-tauri/tauri.conf.json').version" 2>/dev/null || echo "?")
cargo_ver=$(grep -m1 '^version' src-tauri/Cargo.toml | sed -E 's/.*"(.*)".*/\1/')
if [[ "$pkg_ver" == "$conf_ver" && "$conf_ver" == "$cargo_ver" ]]; then
  VERSION="$conf_ver"
  ok "version: $VERSION (package.json = tauri.conf.json = Cargo.toml)"
else
  die "version mismatch: package.json=$pkg_ver tauri.conf.json=$conf_ver Cargo.toml=$cargo_ver"
fi
PUB_DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
NOTES="Cameo v${VERSION}"
DOWNLOAD_BASE="https://r.cameo.ink/release/v${VERSION}"

# ── locate artifacts ────────────────────────────────────────────────────────
TARGET_DIR="${ROOT}/src-tauri/target"

# Manifests are written into a temp dir then uploaded.
MANIFEST_DIR=$(mktemp -d)
trap 'rm -rf "$MANIFEST_DIR"' EXIT

# Track files we'll upload. Each entry: <local_path>|<r2_dest_path>
declare -a UPLOADS=()

queue() {
  UPLOADS+=("$1|$2")
}

# Sign a payload when the signing key is available; otherwise require a
# pre-existing non-empty .sig. Re-signing avoids pairing a fresh payload with a
# stale signature that happens to have the same filename.
ensure_sig() {
  local payload="$1"
  local sig="${payload}.sig"
  if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    info "  signing $(basename "$payload")" >&2
    rm -f "$sig"
    pnpm tauri signer sign "$payload" >/dev/null
  fi
  [[ -s "$sig" ]] || return 1
  echo "$sig"
}

write_manifest() {
  local manifest_name="$1"  # e.g. darwin-aarch64
  local payload_filename="$2"  # filename on R2, e.g. Cameo_aarch64.app.tar.gz
  local sig_file="$3"  # local path to .sig (or empty)
  local signature=""
  if [[ -n "$sig_file" && -f "$sig_file" ]]; then
    signature=$(cat "$sig_file")
  fi
  [[ -n "$signature" ]] || die "signature empty for ${payload_filename}"
  local out="${MANIFEST_DIR}/${manifest_name}.json"
  cat > "$out" <<EOF
{
  "version": "${VERSION}",
  "notes": "${NOTES}",
  "pub_date": "${PUB_DATE}",
  "signature": "${signature}",
  "url": "${DOWNLOAD_BASE}/${payload_filename}"
}
EOF
  ok "manifest: ${manifest_name}.json"
  queue "$out" "update/${manifest_name}.json"
}

# GitHub release assets. R2 (above) carries the Tauri auto-UPDATER (.app.tar.gz +
# darwin-*.json). The open-source DOWNLOAD channel is GitHub Releases: the .dmg
# installers + a website manifest (latest.json) are tagged + uploaded there, and
# cameo_web serves them via cameo.ink/update/ (proxying releases/latest/download).
GH_REPO="hAcKlyc/cameo"
GH_DL_BASE="https://github.com/${GH_REPO}/releases/download/v${VERSION}"
declare -a GH_FILES=()
DMG_ARM64=""; DMG_X64=""

# ── macOS scan (arm + intel) ────────────────────────────────────────────────
for arch_pair in "aarch64-apple-darwin:aarch64:darwin-aarch64" "x86_64-apple-darwin:x86_64:darwin-x86_64"; do
  IFS=":" read -r RUST_TARGET ARCH MANIFEST_NAME <<< "$arch_pair"
  BUNDLE="$TARGET_DIR/$RUST_TARGET/release/bundle"
  if [[ ! -d "$BUNDLE" ]]; then
    die "bundle missing for $RUST_TARGET — run ./build_release.sh before publishing"
  fi
  info "$RUST_TARGET"

  # .app.tar.gz for the updater, .dmg for manual download. Both must match the
  # release version; otherwise auto-update and manual installs can diverge.
  TAR=$(ls -t "$BUNDLE/macos/"*.app.tar.gz 2>/dev/null | head -1 || true)
  EXPECTED_DMG=$(expected_dmg_name "$RUST_TARGET" "$VERSION")
  DMG=""
  [[ -n "$EXPECTED_DMG" && -f "$BUNDLE/dmg/$EXPECTED_DMG" ]] && DMG="$BUNDLE/dmg/$EXPECTED_DMG"

  if [[ -z "$TAR" && -z "$DMG" ]]; then
    warn "skipping $RUST_TARGET — no current release artifacts"
    continue
  fi

  [[ -n "$DMG" ]] || die "current-version .dmg missing for $RUST_TARGET: expected ${EXPECTED_DMG}"
  [[ -n "$TAR" ]] || die "updater .app.tar.gz missing for $RUST_TARGET"
  TAR_VERSION=$(tarball_short_version "$TAR")
  [[ "$TAR_VERSION" == "$VERSION" ]] || die "updater tarball version mismatch for $RUST_TARGET: expected $VERSION, got ${TAR_VERSION:-unknown}"

  SIG=$(ensure_sig "$TAR") || die "updater signature missing for $RUST_TARGET: ${TAR}.sig"
  # Add arch suffix to the uploaded filename so ARM and Intel don't clash on
  # R2 (Tauri names both "Cameo.app.tar.gz"; we rename on upload).
  base=$(basename "$TAR" .app.tar.gz)
  UPLOAD_NAME="${base}_${ARCH}.app.tar.gz"
  queue "$TAR" "release/v${VERSION}/${UPLOAD_NAME}"
  queue "$SIG" "release/v${VERSION}/${UPLOAD_NAME}.sig"
  write_manifest "$MANIFEST_NAME" "$UPLOAD_NAME" "$SIG"

  if [[ -n "$DMG" ]]; then
    queue "$DMG" "release/v${VERSION}/$(basename "$DMG")"
    GH_FILES+=("$DMG")  # also publish the installer to the GitHub release
    case "$ARCH" in
      aarch64) DMG_ARM64=$(basename "$DMG") ;;
      x86_64)  DMG_X64=$(basename "$DMG") ;;
    esac
    ok "  dmg: $(basename "$DMG")"
  fi
done

# ── website download manifest (latest.json) ──────────────────────────────────
# cameo_web's download buttons fetch this (cameo.ink/update/latest.json proxies
# releases/latest/download/latest.json). URLs point at the GitHub release assets.
if [[ -n "$DMG_ARM64" || -n "$DMG_X64" ]]; then
  LATEST_JSON="${MANIFEST_DIR}/latest.json"
  {
    printf '{\n  "version": "%s",\n  "pub_date": "%s",\n  "release_notes": "%s",\n  "downloads": {\n' \
      "$VERSION" "$PUB_DATE" "$NOTES"
    sep=""
    [[ -n "$DMG_ARM64" ]] && { printf '%s    "mac_arm64": { "name": "Apple Silicon", "url": "%s/%s" }' "$sep" "$GH_DL_BASE" "$DMG_ARM64"; sep=$',\n'; }
    [[ -n "$DMG_X64" ]]   && printf '%s    "mac_intel": { "name": "Intel Mac", "url": "%s/%s" }' "$sep" "$GH_DL_BASE" "$DMG_X64"
    printf '\n  }\n}\n'
  } > "$LATEST_JSON"
  GH_FILES+=("$LATEST_JSON")
  ok "manifest: latest.json (website downloads → GitHub release)"
fi

# ── upload ──────────────────────────────────────────────────────────────────
if [[ ${#UPLOADS[@]} -eq 0 ]]; then
  die "no artifacts to upload — did you run ./build_release.sh first?"
fi

echo ""
info "preparing to upload ${#UPLOADS[@]} file(s) to R2 bucket '$R2_BUCKET':"
for entry in "${UPLOADS[@]}"; do
  IFS="|" read -r SRC DST <<< "$entry"
  printf '    %s (%s)\n         → r2:%s/%s\n' "$(basename "$SRC")" "$(size_of "$SRC")" "$R2_BUCKET" "$DST"
done
echo ""

if [[ ${#GH_FILES[@]} -gt 0 ]]; then
  info "GitHub release v${VERSION} will receive ${#GH_FILES[@]} asset(s):"
  for f in "${GH_FILES[@]}"; do printf '    %s (%s)\n' "$(basename "$f")" "$(size_of "$f")"; done
  echo ""
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
  ok "DRY RUN — no upload performed."
  exit 0
fi

read -r -p "Proceed with upload? [y/N] " yn
[[ "$yn" =~ ^[Yy]$ ]] || { warn "aborted by user"; exit 0; }

# Configure rclone's ad-hoc `:s3:` backend via env vars. Do NOT use an inline
# connection string (`:s3,endpoint=…:`) — R2_ENDPOINT contains "://" and rclone
# truncates an unquoted value at the first colon, yielding endpoint="https".
export RCLONE_S3_PROVIDER="Cloudflare"
export RCLONE_S3_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID"
export RCLONE_S3_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY"
export RCLONE_S3_ENDPOINT="$R2_ENDPOINT"
export RCLONE_S3_REGION="auto"
# R2 tokens can't create buckets; skip rclone's pre-upload bucket check/create
# (the bucket already exists) — otherwise the CreateBucket probe 403s.
export RCLONE_S3_NO_CHECK_BUCKET="true"

for entry in "${UPLOADS[@]}"; do
  IFS="|" read -r SRC DST <<< "$entry"
  info "rclone copyto $(basename "$SRC") → r2:$R2_BUCKET/$DST"
  rclone --config /dev/null --progress --stats=1s copyto "$SRC" ":s3:${R2_BUCKET}/${DST}"
done

echo ""
ok "macOS release v${VERSION} published"
echo ""
echo "  ┌─ live manifest URLs ──────────────────────────────────────"
for entry in "${UPLOADS[@]}"; do
  IFS="|" read -r SRC DST <<< "$entry"
  if [[ "$DST" == update/* ]]; then
    printf '     https://r.cameo.ink/%s\n' "$DST"
  fi
done
echo "  └───────────────────────────────────────────────────────────"
echo ""

# ── GitHub Release (open-source distribution + website download source) ──────
# Tag + create the release (idempotent) and upload the installers + latest.json.
# cameo_web reads releases/latest/download/latest.json for its download buttons.
# Windows publishes to the SAME release from publish_release.ps1 (whichever runs
# first creates it; the other uploads with --clobber).
if command -v gh >/dev/null 2>&1 && [[ ${#GH_FILES[@]} -gt 0 ]]; then
  TAG="v${VERSION}"
  info "GitHub release: ${GH_REPO}@${TAG}"
  if ! git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
    git tag -a "$TAG" -m "Cameo $TAG" && git push origin "$TAG" && ok "tagged $TAG"
  else
    ok "tag $TAG already exists"
  fi
  if ! gh release view "$TAG" >/dev/null 2>&1; then
    gh release create "$TAG" --title "Cameo $TAG" --notes "$NOTES" --verify-tag && ok "created GitHub release $TAG"
  fi
  info "uploading ${#GH_FILES[@]} GitHub release asset(s) — this may take a while"
  gh release upload "$TAG" "${GH_FILES[@]}" --clobber \
    && ok "uploaded ${#GH_FILES[@]} asset(s) → download links live at cameo.ink"
  echo ""
else
  warn "gh CLI missing or no installers — skipped GitHub release; cameo_web download"
  warn "buttons stay on the GitHub releases page until a release with latest.json exists."
  echo ""
fi

warn "REMINDER: bump src-tauri/tauri.conf.json's version BEFORE running this"
warn "script for the next release, or you'll re-publish v${VERSION}."
warn "Windows publishes separately — run publish_release.ps1 on the Windows box."
