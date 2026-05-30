//! Auto-updater for the Cameo desktop client.
//!
//! Background flow (silent download, on-demand install):
//!   1. App boot + 60s delay → check the platform-specific manifest at
//!      `https://r.cameo.ink/update/<target>-<arch>.json`.
//!   2. If a newer version is published, the binary is downloaded silently —
//!      the user sees no banner, no modal, nothing.
//!   3. Download completes → frontend gets `updater:ready-to-restart` event →
//!      titlebar shows the "重启更新" button.
//!   4. User clicks → graceful Codex sidecar shutdown → apply update + relaunch
//!      (macOS) or fire the NSIS installer (Windows).
//!
//! Platform split (the install path is the only real difference):
//!   • macOS: `Update::download_and_install()` swaps `/Applications/Cameo.app`
//!     atomically; relaunch() picks up the new bundle. One-phase.
//!   • Windows: `Update::download()` streams bytes to `~/.cameo/pending_update.bin`
//!     with metadata in `pending_update.json`. Install runs only when the user
//!     clicks the titlebar button — at that point we re-resolve `Update` (cached
//!     from the original check, with 3-retry fallback) and call `install(bytes)`,
//!     which spawns the NSIS installer and `exit(0)`s.
//!
//! Why cache the `Update` object? `Update::install(bytes)` requires an `Update`,
//! and the only public way to obtain one is `updater.check().await` — a network
//! hop. On Windows that hop runs at click-time, and a flaky/blocked network
//! silently fails the install (the button does nothing). Caching the object at
//! `check()` time lets the click-handler skip the network entirely. Bytes are
//! signature-verified at download, so this is strictly safer than rechecking.

use crate::codex::CodexRegistry;
use crate::config;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[cfg(target_os = "windows")]
use crate::paths::{cameo_data_dir, ensure_data_layout};
#[cfg(target_os = "windows")]
use serde::Deserialize;

/// Global flag — prevents two concurrent checks from racing on the download path.
static UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// The version of the bytes that completed a silent background download.
/// Mirrors the disk-cached version on Windows; on macOS it's the version
/// that was just installed in-place and is awaiting a relaunch.
static DOWNLOADED_VERSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The most recent `Update` from a successful `check()` this session.
/// Used by the Windows install path to skip the network round-trip — see the
/// module doc above.
static LATEST_UPDATE: std::sync::Mutex<Option<Update>> = std::sync::Mutex::new(None);

fn cache_update(update: Update) {
    if let Ok(mut g) = LATEST_UPDATE.lock() {
        *g = Some(update);
    }
}

#[cfg(target_os = "windows")]
fn cached_update_for(wanted_version: &str) -> Option<Update> {
    let g = LATEST_UPDATE.lock().ok()?;
    let update = g.as_ref()?;
    if update.version == wanted_version {
        Some(update.clone())
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn pending_bin_path() -> std::path::PathBuf {
    cameo_data_dir().join("pending_update.bin")
}

#[cfg(target_os = "windows")]
fn pending_meta_path() -> std::path::PathBuf {
    cameo_data_dir().join("pending_update.json")
}

#[cfg(target_os = "windows")]
fn clear_pending_update() {
    let _ = std::fs::remove_file(pending_bin_path());
    let _ = std::fs::remove_file(pending_bin_path().with_extension("bin.tmp"));
    let _ = std::fs::remove_file(pending_meta_path());
    if let Ok(mut g) = DOWNLOADED_VERSION.lock() {
        *g = None;
    }
    if let Ok(mut g) = LATEST_UPDATE.lock() {
        *g = None;
    }
}

#[cfg(target_os = "windows")]
fn read_pending_meta() -> Option<PendingMeta> {
    let meta_path = pending_meta_path();
    let bin_path = pending_bin_path();
    if !meta_path.exists() || !bin_path.exists() {
        return None;
    }
    let bytes = std::fs::read(&meta_path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(target_os = "windows")]
fn pending_zip_compression_method() -> Result<Option<u16>, String> {
    let bytes = std::fs::read(pending_bin_path()).map_err(|e| e.to_string())?;
    if bytes.len() < 10 {
        return Err("pending update payload is too small".into());
    }
    if bytes.get(0..4) != Some(&[0x50, 0x4b, 0x03, 0x04]) {
        return Ok(None);
    }
    Ok(Some(u16::from_le_bytes([bytes[8], bytes[9]])))
}

#[cfg(target_os = "windows")]
fn clear_unsupported_pending_zip(app: &AppHandle, context: &str) -> bool {
    match pending_zip_compression_method() {
        Ok(Some(0)) | Ok(None) => false,
        Ok(Some(method)) => {
            tracing::warn!(
                module = "updater",
                method,
                context,
                "pending windows updater zip uses unsupported compression; clearing"
            );
            clear_pending_update();
            let _ = app.emit("updater:install-failed", "BAD_UPDATE_PAYLOAD");
            true
        }
        Err(e) => {
            tracing::warn!(
                module = "updater",
                context,
                "pending windows update payload is unreadable: {e}"
            );
            clear_pending_update();
            let _ = app.emit("updater:install-failed", "BAD_UPDATE_PAYLOAD");
            true
        }
    }
}

#[cfg(target_os = "windows")]
#[derive(Serialize, Deserialize)]
struct PendingMeta {
    version: String,
    notes: Option<String>,
}

// ── proxy injection ─────────────────────────────────────────────────────────
//
// The Codex sidecar uses the same proxy settings (proxy.rs::ProxySettings); the
// updater piggybacks on that config so a user with a proxy isn't stuck unable
// to fetch the manifest. 15s per-request timeout keeps us off the "hang on a
// blackholed connection" path.
fn build_updater_with_proxy(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let mut builder = app.updater_builder().timeout(Duration::from_secs(15));
    let cfg = config::load();
    if cfg.proxy.enabled {
        match cfg.proxy.proxy_url() {
            Ok(url_str) => match reqwest::Url::parse(&url_str) {
                Ok(url) => {
                    tracing::info!(module = "updater", proxy = %url_str, "applying proxy to updater HTTP client");
                    builder = builder.proxy(url);
                }
                Err(e) => {
                    tracing::warn!(
                        module = "updater",
                        "proxy url parse failed ({e}); continuing without"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    module = "updater",
                    "proxy enabled but invalid ({e}); continuing without"
                );
            }
        }
    }
    builder.build().map_err(|e| {
        tracing::warn!(module = "updater", "build updater failed: {e}");
        e.to_string()
    })
}

// ── startup check entry point ───────────────────────────────────────────────
//
// Called from `lib.rs` setup. Sleeps 60s so the app's first launch isn't burdened
// by a network round-trip, then runs the silent download path. Errors are
// swallowed — failed update checks are not the user's problem.
pub async fn check_update_on_startup(app: AppHandle) {
    tracing::info!(
        module = "updater",
        "scheduling startup update check (60s delay)"
    );
    tokio::time::sleep(Duration::from_secs(60)).await;
    if let Err(e) = check_and_download_silently(&app).await {
        tracing::warn!(module = "updater", "startup update check failed: {e}");
    }
}

// ── silent check + download ─────────────────────────────────────────────────
//
// One-shot worker. The three outcomes are distinct on purpose: a manual check
// from Settings must NOT tell the user "you're up to date" when in fact another
// check/download is already running (Busy) — only when the server genuinely
// reports nothing newer (UpToDate).
pub enum CheckOutcome {
    /// Another check/download already holds the lock — result unknown yet.
    Busy,
    /// Server reports no newer version.
    UpToDate,
    /// A newer version was downloaded (or already on disk this session).
    Downloaded(#[allow(dead_code)] String),
}

async fn check_and_download_silently(app: &AppHandle) -> Result<CheckOutcome, String> {
    if UPDATE_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        tracing::info!(module = "updater", "update already in progress, skipping");
        return Ok(CheckOutcome::Busy);
    }
    let result = check_and_download_silently_inner(app).await;
    UPDATE_IN_PROGRESS.store(false, Ordering::SeqCst);
    result.map(|opt| match opt {
        Some(v) => CheckOutcome::Downloaded(v),
        None => CheckOutcome::UpToDate,
    })
}

async fn check_and_download_silently_inner(app: &AppHandle) -> Result<Option<String>, String> {
    let updater = build_updater_with_proxy(app)?;
    let update_opt = updater.check().await.map_err(|e| e.to_string())?;
    let Some(update) = update_opt else {
        tracing::info!(module = "updater", "no update available");
        return Ok(None);
    };

    let new_version = update.version.clone();
    tracing::info!(module = "updater", version = %new_version, "update available — starting silent download");

    // Skip re-download if we already have these bytes this session.
    if let Ok(g) = DOWNLOADED_VERSION.lock() {
        if g.as_deref() == Some(new_version.as_str()) {
            tracing::info!(module = "updater", version = %new_version, "already downloaded this session — skipping");
            cache_update(update.clone());
            let _ = app.emit("updater:ready-to-restart", &new_version);
            return Ok(Some(new_version));
        }
    }

    let _ = app.emit("updater:download-started", &new_version);

    // Progress emitter shared by both platforms. Captured by closure into the
    // updater's on_chunk callback. Throttled to every ~2% to avoid IPC spam.
    let progress_app = app.clone();
    let last_pct = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let last_pct_cb = last_pct.clone();
    let on_chunk = move |downloaded: usize, content_length: Option<u64>| {
        if let Some(total) = content_length {
            let pct = (downloaded as u64 * 100) / total.max(1);
            let prev = last_pct_cb.load(Ordering::Relaxed);
            if pct >= prev + 2 || pct == 100 {
                last_pct_cb.store(pct, Ordering::Relaxed);
                let _ = progress_app.emit(
                    "updater:download-progress",
                    DownloadProgress {
                        downloaded: downloaded as u64,
                        total,
                    },
                );
            }
        }
    };

    #[cfg(target_os = "macos")]
    {
        // mac: one-phase — bytes stream straight into /Applications/Cameo.app
        // (atomic replacement). The running process keeps running on the old
        // bundle; relaunch picks up the new one.
        match update.download_and_install(on_chunk, || {}).await {
            Ok(_) => {
                tracing::info!(module = "updater", version = %new_version, "mac install complete; awaiting user relaunch");
                if let Ok(mut g) = DOWNLOADED_VERSION.lock() {
                    *g = Some(new_version.clone());
                }
                let _ = app.emit("updater:ready-to-restart", &new_version);
                Ok(Some(new_version))
            }
            Err(e) => {
                tracing::warn!(module = "updater", "mac download_and_install failed: {e}");
                let _ = app.emit("updater:download-failed", &e.to_string());
                Err(e.to_string())
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // win: two-phase — accumulate bytes, write atomically to disk, defer
        // the NSIS install until the user clicks "重启更新". This protects the
        // running app: NSIS does `exit(0)` to swap files, so kicking it off
        // mid-session would kill an unsaved chat.
        if let Some(meta) = read_pending_meta() {
            if meta.version == new_version {
                tracing::info!(
                    module = "updater",
                    version = %new_version,
                    "windows update already persisted; warming install cache"
                );
                cache_update(update.clone());
                if let Ok(mut g) = DOWNLOADED_VERSION.lock() {
                    *g = Some(new_version.clone());
                }
                let _ = app.emit("updater:ready-to-restart", &new_version);
                return Ok(Some(new_version));
            }
        }

        let bytes = update.download(on_chunk, || {}).await.map_err(|e| {
            tracing::warn!(module = "updater", "windows download failed: {e}");
            let _ = app.emit("updater:download-failed", &e.to_string());
            e.to_string()
        })?;

        // Persist bytes via tmp + rename so a crash mid-write doesn't leave a
        // half-baked installer. Meta file written second; readers tolerate it
        // being absent (treated as no pending update).
        ensure_data_layout().map_err(|e| e.to_string())?;
        let dst = pending_bin_path();
        let tmp = dst.with_extension("bin.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
        let meta = PendingMeta {
            version: new_version.clone(),
            notes: Some(update.body.clone().unwrap_or_default()),
        };
        std::fs::write(
            pending_meta_path(),
            serde_json::to_vec(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;

        // Keep the cached Update aligned with the exact bytes on disk. Caching
        // before the atomic write commits can leave cache=NEW/disk=OLD, which
        // makes a click during replacement look like a no-op.
        cache_update(update.clone());

        if let Ok(mut g) = DOWNLOADED_VERSION.lock() {
            *g = Some(new_version.clone());
        }

        tracing::info!(
            module = "updater",
            version = %new_version,
            bytes = bytes.len(),
            "windows update bytes persisted; awaiting user click"
        );
        let _ = app.emit("updater:ready-to-restart", &new_version);
        Ok(Some(new_version))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux / others: tauri-plugin-updater supports AppImage on linux but
        // we don't ship that today. No-op rather than failing.
        let _ = app.emit("updater:download-failed", "unsupported platform");
        Ok(None)
    }
}

#[derive(Serialize, Clone)]
struct DownloadProgress {
    downloaded: u64,
    total: u64,
}

// ── manual check (settings panel) ──────────────────────────────────────────
//
// Same as the startup path but without the 60s delay. Returns a status the
// frontend turns into the right feedback:
//   "found"    — a newer version is downloading / downloaded (show progress /
//                restart button; no toast).
//   "busy"     — a background check/download is already running; treated like
//                "found" so we never falsely toast "up to date".
//   "uptodate" — server genuinely reports nothing newer (toast "已是最新版本").
#[tauri::command]
pub async fn check_and_download_update(app: AppHandle) -> Result<String, String> {
    Ok(match check_and_download_silently(&app).await? {
        CheckOutcome::Downloaded(_) => "found",
        CheckOutcome::Busy => "busy",
        CheckOutcome::UpToDate => "uptodate",
    }
    .to_string())
}

// ── windows: pending bytes on startup ───────────────────────────────────────
//
// Read on app boot (from lib.rs setup) to detect a previously-downloaded update
// the user hasn't installed yet. Emits the version up to the frontend so the
// "重启更新" button reappears without re-checking the server.
#[tauri::command]
pub fn check_pending_update(app: AppHandle) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let meta = match read_pending_meta() {
            Some(meta) => meta,
            None => {
                clear_pending_update();
                return None;
            }
        };
        let current = app.package_info().version.to_string();
        if version_gt(&meta.version, &current) {
            if clear_unsupported_pending_zip(&app, "startup") {
                return None;
            }
            tracing::info!(module = "updater", version = %meta.version, "found pending update on disk");
            let app_for_warmup = app.clone();
            tauri::async_runtime::spawn(async move {
                if LATEST_UPDATE.lock().map(|g| g.is_some()).unwrap_or(false) {
                    return;
                }
                tracing::info!(
                    module = "updater",
                    "warming update cache for pending install"
                );
                let _ = check_and_download_silently(&app_for_warmup).await;
            });
            Some(meta.version)
        } else {
            // Pending update is older than what's installed (manual upgrade?) —
            // clear it so a stale install doesn't downgrade the user.
            clear_pending_update();
            None
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        None
    }
}

/// Naive semver comparison — sufficient for `0.0.2 > 0.0.1` style versions.
/// Doesn't understand pre-release suffixes; we don't ship channels.
#[cfg(target_os = "windows")]
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    let av = parse(a);
    let bv = parse(b);
    for i in 0..av.len().max(bv.len()) {
        let ax = av.get(i).copied().unwrap_or(0);
        let bx = bv.get(i).copied().unwrap_or(0);
        if ax != bx {
            return ax > bx;
        }
    }
    false
}

// ── windows: actually install ──────────────────────────────────────────────
//
// Triggered by the frontend "重启更新" click. Resolves the `Update` (cached if
// possible, else re-check with 3-attempt fallback), calls `install(bytes)`.
// NSIS takes over from there.
#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn install_pending_update(
    app: AppHandle,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<(), String> {
    let bin = pending_bin_path();
    let meta = pending_meta_path();
    if !bin.exists() || !meta.exists() {
        return Err("no pending update on disk".into());
    }
    if clear_unsupported_pending_zip(&app, "install") {
        return Err("BAD_UPDATE_PAYLOAD".into());
    }
    let bytes = std::fs::read(&bin).map_err(|e| e.to_string())?;
    let meta_bytes = std::fs::read(&meta).map_err(|e| e.to_string())?;
    let meta: PendingMeta = serde_json::from_slice(&meta_bytes).map_err(|e| e.to_string())?;

    // Resolve an Update object — cache hit avoids the network entirely.
    let update = match cached_update_for(&meta.version) {
        Some(u) => {
            tracing::info!(module = "updater", version = %meta.version, "using cached Update for install");
            u
        }
        None => {
            tracing::info!(module = "updater", version = %meta.version, "cache miss; re-checking server (with retry)");
            let mut last_err = String::from("unknown");
            let mut got = None;
            for attempt in 1..=3 {
                let updater = build_updater_with_proxy(&app)?;
                match updater.check().await {
                    Ok(Some(u)) if u.version == meta.version => {
                        cache_update(u.clone());
                        got = Some(u);
                        break;
                    }
                    Ok(Some(u)) => {
                        last_err = format!(
                            "server returned different version {} (expected {})",
                            u.version, meta.version
                        );
                    }
                    Ok(None) => {
                        last_err = "server reports no update available".into();
                    }
                    Err(e) => {
                        last_err = e.to_string();
                    }
                }
                if attempt < 3 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            if last_err.starts_with("server returned different version")
                || last_err == "server reports no update available"
            {
                clear_pending_update();
                let _ = app.emit("updater:install-failed", "VERSION_MISMATCH");
                return Err("VERSION_MISMATCH".into());
            }
            got.ok_or_else(|| {
                tracing::warn!(
                    module = "updater",
                    "update resolve failed after 3 attempts: {last_err}"
                );
                "NETWORK_ERROR".to_string()
            })?
        }
    };

    // Shut down the Codex sidecar BEFORE NSIS takes the process down with exit(0),
    // so we leave a clean process tree (no orphaned codex children).
    tracing::info!(
        module = "updater",
        "shutting down codex sidecar before install"
    );
    codex.kill_all_sync();

    // Hand off to NSIS. install() spawns the installer and calls exit(0).
    update.install(bytes).map_err(|e| {
        tracing::warn!(module = "updater", "windows install failed: {e}");
        if e.to_string().contains("Compression method not supported") {
            clear_pending_update();
        }
        let _ = app.emit("updater:install-failed", &e.to_string());
        e.to_string()
    })?;

    Ok(())
}

/// macOS install + relaunch (single command — bytes are already on disk in
/// `/Applications/Cameo.app` from the silent download). Cleanly stops the
/// codex sidecar before `relaunch()` so the new process can spawn its own.
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn install_pending_update(
    app: AppHandle,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<(), String> {
    tracing::info!(
        module = "updater",
        "shutting down codex sidecar before relaunch"
    );
    codex.kill_all_sync();
    app.restart();
}
