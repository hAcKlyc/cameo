#!/usr/bin/env bash
# build_release.sh — production macOS build: signed, notarized .dmg(s) + auto-update payloads.
#
# Default = BOTH arches (Apple Silicon + Intel) as separate per-arch .dmgs, each
# with its .app.tar.gz + .sig auto-update payload. Updater artifacts are enabled
# by bundle.createUpdaterArtifacts in tauri.conf.json. This is the everyday
# release path that feeds publish_release.sh — running the script with no flags is
# the correct daily build. Cameo bundles no per-arch binaries (it drives the user's
# own `codex` CLI), so a single **universal** .dmg is also available via
# --universal for manual one-link distribution (universal carries NO updater payload).
#
# Usage:
#   ./build_release.sh                 # arm64 + Intel, per-arch, WITH updater  [default]
#   ./build_release.sh --universal     # one universal .dmg (manual download; no auto-update)
#   ./build_release.sh --arm           # Apple Silicon only
#   ./build_release.sh --intel         # Intel only
#   ./build_release.sh --clean         # cargo clean first
#   ./build_release.sh -o              # reveal the .dmg in Finder when done
#
# Signing / notarization (optional — read from .env or the environment; if absent
# the build is ad-hoc-signed and Gatekeeper will block it on other Macs):
#   APPLE_SIGNING_IDENTITY="Developer ID Application: NAME (TEAMID)"
#   # notarization, either:
#   APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID
#   # ...or App Store Connect API key:
#   APPLE_API_KEY / APPLE_API_ISSUER / APPLE_API_KEY_PATH
# Tauri performs the signing + notarization itself when these are present.
#
# Windows: use build_release.ps1 instead.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

info() { printf '→ %s\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
die()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }
size_of() { ls -lh "$1" | awk '{print $5}'; }
plist_short_version() { plutil -extract CFBundleShortVersionString raw "$1" 2>/dev/null || true; }
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

[[ "$(uname -s)" == "Darwin" ]] || die "this script is for macOS — on Windows run build_release.ps1"

MODE="both"   # both (default) | universal | arm | intel
CLEAN=0
OPEN=0
for arg in "$@"; do
  case "$arg" in
    --universal) MODE="universal" ;;
    --arm)       MODE="arm" ;;
    --intel)     MODE="intel" ;;
    --both)      MODE="both" ;;
    --clean)     CLEAN=1 ;;
    -o|--open)   OPEN=1 ;;
    -h|--help)   sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown flag: $arg" ;;
  esac
done

command -v pnpm  >/dev/null || die "pnpm not on PATH (run ./setup.sh)"
command -v cargo >/dev/null || die "cargo not on PATH (run ./setup.sh)"

# ── signing env (optional) ────────────────────────────────────────────────────
if [[ -f .env ]]; then
  info "loading .env"
  set -a; . ./.env; set +a
fi
# Empty vars in .env still count as "set" to Tauri — codesign will then try to
# sign with identity "" and fail. Clear anything that's exported-but-blank,
# EXCEPT TAURI_SIGNING_PRIVATE_KEY_PASSWORD — that one we want to explicitly
# remain set (potentially to "") so `tauri build` doesn't fall back to an
# interactive prompt that fails in non-TTY environments.
for v in APPLE_SIGNING_IDENTITY APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID TAURI_SIGNING_PRIVATE_KEY; do
  if [[ -z "${!v:-}" ]]; then unset "$v"; fi
done
# Ensure the password var is at least defined when the private key is set,
# even if the key was generated with --password "". Otherwise Tauri prompts.
if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD-}"
fi
if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  ok "signing as: ${APPLE_SIGNING_IDENTITY}"
  if [[ -n "${APPLE_API_KEY:-}" || -n "${APPLE_PASSWORD:-}" ]]; then
    ok "notarization credentials present — Tauri will notarize"
  else
    warn "no notarization creds (APPLE_API_KEY… or APPLE_ID/APPLE_PASSWORD/APPLE_TEAM_ID) — .dmg won't be notarized"
  fi
else
  warn "no APPLE_SIGNING_IDENTITY — building AD-HOC signed (won't pass Gatekeeper on other Macs)"
fi

# Tauri updater needs the signing private key in env to produce signed .sig
# files alongside the .app.tar.gz. Without it the build skips the .sig and
# publish_release.sh refuses to ship.
if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  ok "tauri update signing key present — updater artifacts (.app.tar.gz + .sig) will be generated"
else
  warn "no TAURI_SIGNING_PRIVATE_KEY — updater artifacts will NOT be signed. Auto-update payloads can't be published."
fi

# ── version sanity (package.json ↔ Cargo.toml ↔ tauri.conf.json) ─────────────
pkg_ver=$(node -p "require('./package.json').version" 2>/dev/null || echo "?")
conf_ver=$(node -p "require('./src-tauri/tauri.conf.json').version" 2>/dev/null || echo "?")
cargo_ver=$(grep -m1 '^version' src-tauri/Cargo.toml | sed -E 's/.*"(.*)".*/\1/')
if [[ "$pkg_ver" == "$conf_ver" && "$conf_ver" == "$cargo_ver" ]]; then
  ok "version $pkg_ver (package.json = tauri.conf.json = Cargo.toml)"
else
  die "version mismatch: package.json=$pkg_ver tauri.conf.json=$conf_ver Cargo.toml=$cargo_ver"
fi

# ── targets ───────────────────────────────────────────────────────────────────
ensure_target() {
  rustup target list --installed 2>/dev/null | grep -qx "$1" || { info "adding rust target $1"; rustup target add "$1"; }
}
case "$MODE" in
  universal) TARGETS=("universal-apple-darwin"); ensure_target aarch64-apple-darwin; ensure_target x86_64-apple-darwin ;;
  arm)       TARGETS=("aarch64-apple-darwin");   ensure_target aarch64-apple-darwin ;;
  intel)     TARGETS=("x86_64-apple-darwin");    ensure_target x86_64-apple-darwin ;;
  both)      TARGETS=("aarch64-apple-darwin" "x86_64-apple-darwin"); ensure_target aarch64-apple-darwin; ensure_target x86_64-apple-darwin ;;
esac

[[ -d node_modules ]] || { info "pnpm install (first run)"; pnpm install; }

if [[ "$CLEAN" -eq 1 ]]; then
  info "cargo clean + rm dist"
  ( cd src-tauri && cargo clean ); rm -rf dist
fi

info "typecheck"
pnpm typecheck

start_ts=$(date +%s)
dmgs=()
tarballs=()
# Auto-update artifacts (.app.tar.gz + .sig) are produced by Tauri 2 when
# bundle.createUpdaterArtifacts is true and TAURI_SIGNING_PRIVATE_KEY is set.
# The bundle CLI itself only accepts app/dmg on macOS.
for target in "${TARGETS[@]}"; do
  BUNDLES="app,dmg"
  bundle_dir="src-tauri/target/$target/release/bundle"
  if [[ -d "$bundle_dir" ]]; then
    info "removing stale bundle artifacts for $target"
    rm -rf "$bundle_dir"
  fi
  info "pnpm tauri build --target $target --bundles $BUNDLES"
  pnpm tauri build --target "$target" --bundles "$BUNDLES"

  app_plist="src-tauri/target/$target/release/bundle/macos/Cameo.app/Contents/Info.plist"
  [[ -f "$app_plist" ]] || die "no app bundle produced for $target"
  app_ver=$(plist_short_version "$app_plist")
  [[ "$app_ver" == "$conf_ver" ]] || die "app bundle version mismatch for $target: expected $conf_ver, got ${app_ver:-unknown}"

  expected_dmg=$(expected_dmg_name "$target" "$conf_ver")
  if [[ -n "$expected_dmg" ]]; then
    dmg="src-tauri/target/$target/release/bundle/dmg/$expected_dmg"
    [[ -f "$dmg" ]] || die "expected current-version .dmg missing for $target: $expected_dmg"
  else
    dmg=$(ls -t "src-tauri/target/$target/release/bundle/dmg/"*.dmg 2>/dev/null | head -1 || true)
  fi
  [[ -n "$dmg" ]] || die "no .dmg produced for $target"
  dmgs+=("$dmg")
  # Capture the updater tarball if Tauri produced one.
  if [[ "$target" != "universal-apple-darwin" ]]; then
    tarball=$(ls -t "src-tauri/target/$target/release/bundle/macos/"*.app.tar.gz 2>/dev/null | head -1 || true)
    [[ -n "$tarball" ]] || die "no .app.tar.gz produced for $target (set TAURI_SIGNING_PRIVATE_KEY and keep bundle.createUpdaterArtifacts=true)"
    tarball_ver=$(tarball_short_version "$tarball")
    [[ "$tarball_ver" == "$conf_ver" ]] || die "updater tarball version mismatch for $target: expected $conf_ver, got ${tarball_ver:-unknown}"
    [[ -s "$tarball.sig" ]] || die "missing updater signature for $target: $tarball.sig"
    tarballs+=("$tarball")
  fi
done

echo ""
ok "built in $(( $(date +%s) - start_ts ))s"
echo ""
echo "  ┌─ release artifact(s) ────────────────────────────────────"
for d in "${dmgs[@]}"; do
  printf '     dmg     : %s (%s)\n' "$ROOT/$d" "$(size_of "$d")"
done
for t in "${tarballs[@]}"; do
  printf '     update  : %s (%s)\n' "$ROOT/$t" "$(size_of "$t")"
  if [[ -f "$t.sig" ]]; then
    printf '             : %s (%s)\n' "$ROOT/$t.sig" "$(size_of "$t.sig")"
  else
    printf '             : %s\n' "$ROOT/$t.sig"
  fi
done
echo "  └───────────────────────────────────────────────────────────"
echo ""

if [[ "$OPEN" -eq 1 ]]; then
  open -R "${dmgs[0]}"
fi
