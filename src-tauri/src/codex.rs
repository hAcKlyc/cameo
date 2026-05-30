//! Codex runtime — drives `codex app-server` as a persistent JSON-RPC 2.0 /
//! stdio sidecar (one process per Board). Ported from a sibling TS
//! implementation (`codex.ts`, TS→Rust). The adapter translates Codex
//! notifications into `UnifiedEvent`s.
//!
//! Flow: spawn → initialize (+initialized) → thread/start | thread/resume →
//! turn/start (new active turn) / turn/steer (same active turn) /
//! turn/interrupt (cancel) → stream items.
//! Image output (`imageGeneration` item: savedPath or base64 result) is copied
//! into the Board folder, minted as an Asset, and placed right-of-source.
//!
//! References are passed as **file paths in the prompt** for the agent to
//! self-read (decision D4) — not attached bytes.

use crate::board::{self, BoardRegistry};
use crate::prompt;
use crate::runtime::{CodexEventEnvelope, PlanStep, UnifiedEvent, CODEX_EVENT};
use crate::session;
use crate::{assets, storage};
use base64::Engine;
use parking_lot::Mutex as PlMutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(not(windows))]
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::oneshot;
use tokio::sync::Mutex as TokioMutex;

// ── PATH resolution (GUI apps get a minimal PATH; augment + `which`) ─────────

#[cfg(not(windows))]
const SHELL_PATH_TIMEOUT: Duration = Duration::from_secs(3);
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const AUTH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const AUTH_PROBE_INIT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
struct ResolvedCodex {
    path: PathBuf,
    search_path: String,
}

fn push_unique(parts: &mut Vec<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() || parts.iter().any(|p| p == &path) {
        return;
    }
    parts.push(path);
}

fn push_split_path(parts: &mut Vec<PathBuf>, path: &OsStr) {
    for p in std::env::split_paths(path) {
        push_unique(parts, p);
    }
}

#[cfg(not(windows))]
fn push_unix_fallback_paths(parts: &mut Vec<PathBuf>) {
    if let Some(home) = dirs::home_dir() {
        for d in [
            ".local/bin",
            ".cargo/bin",
            ".bun/bin",
            ".deno/bin",
            ".npm-global/bin",
            ".volta/bin",
            "Library/pnpm",
            ".asdf/shims",
            ".local/share/mise/shims",
            ".local/share/fnm/aliases/default/bin",
        ] {
            push_unique(parts, home.join(d));
        }
        push_nvm_paths(parts, &home);
    }

    for d in [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        push_unique(parts, PathBuf::from(d));
    }
}

#[cfg(not(windows))]
fn push_nvm_paths(parts: &mut Vec<PathBuf>, home: &Path) {
    let node_versions = home.join(".nvm").join("versions").join("node");
    let Ok(entries) = std::fs::read_dir(node_versions) else {
        return;
    };
    let mut versions: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    versions.sort_by(|a, b| {
        node_version_key(b)
            .cmp(&node_version_key(a))
            .then_with(|| b.cmp(a))
    });
    for version in versions {
        push_unique(parts, version.join("bin"));
    }
}

#[cfg(not(windows))]
fn node_version_key(path: &Path) -> Vec<u64> {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|p| p.parse::<u64>().ok())
        .collect()
}

#[cfg(windows)]
fn push_windows_fallback_paths(parts: &mut Vec<PathBuf>) {
    if let Some(home) = dirs::home_dir() {
        for d in [
            ".local/bin",
            ".cargo/bin",
            ".bun/bin",
            ".deno/bin",
            "AppData/Roaming/npm",
            "AppData/Local/pnpm",
            "AppData/Local/Volta/bin",
            "scoop/shims",
        ] {
            push_unique(parts, home.join(d));
        }
    }
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(var).map(PathBuf::from) {
            if var == "APPDATA" {
                push_unique(parts, root.join("npm"));
            } else {
                push_unique(parts, root.join("pnpm"));
                push_unique(parts, root.join("Volta").join("bin"));
                push_unique(parts, root.join("Programs").join("nodejs"));
            }
        }
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(var).map(PathBuf::from) {
            push_unique(parts, root.join("nodejs"));
        }
    }
}

fn build_augmented_path(include_shell_path: bool) -> String {
    #[cfg(windows)]
    let _ = include_shell_path;
    let mut parts: Vec<PathBuf> = Vec::new();
    if let Some(existing) = std::env::var_os("PATH") {
        push_split_path(&mut parts, &existing);
    }
    #[cfg(not(windows))]
    if include_shell_path {
        if let Some(shell_path) = detect_shell_path() {
            push_split_path(&mut parts, OsStr::new(&shell_path));
        }
    }
    #[cfg(not(windows))]
    push_unix_fallback_paths(&mut parts);
    #[cfg(windows)]
    push_windows_fallback_paths(&mut parts);

    std::env::join_paths(&parts)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| std::env::var("PATH").unwrap_or_default())
}

#[cfg(not(windows))]
fn detect_shell_path() -> Option<String> {
    static CACHED: OnceLock<Option<String>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            let marker = format!("__CAMEO_PATH_{}__", std::process::id());
            let script = format!("echo \"{marker}${{PATH}}{marker}\"");
            let mut cmd = std::process::Command::new(shell);
            crate::process::hide_console_window(&mut cmd);
            let mut child = match cmd
                .args(["-i", "-l", "-c", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(_) => return None,
            };
            let deadline = Instant::now() + SHELL_PATH_TIMEOUT;
            let output = loop {
                match child.try_wait() {
                    Ok(Some(_)) => break child.wait_with_output().ok(),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return None;
                    }
                }
            };
            output.filter(|o| o.status.success()).and_then(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                extract_marked_path(&stdout, &marker)
            })
        })
        .clone()
}

#[cfg(not(windows))]
fn extract_marked_path(stdout: &str, marker: &str) -> Option<String> {
    let start = stdout.find(marker)? + marker.len();
    let end = stdout[start..].find(marker)? + start;
    let path = stdout[start..end].trim();
    (path.len() > 10).then(|| path.to_string())
}

fn resolve_codex_in(search_path: String) -> Result<ResolvedCodex, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    which::which_in("codex", Some(&search_path), cwd)
        .map(|path| ResolvedCodex { path, search_path })
        .map_err(|e| format!("codex not found in augmented PATH: {e}"))
}

fn resolve_codex() -> Result<ResolvedCodex, String> {
    let fallback_path = build_augmented_path(false);
    match resolve_codex_in(fallback_path.clone()) {
        Ok(found) => Ok(found),
        Err(fallback_err) => {
            #[cfg(not(windows))]
            {
                let shell_path = build_augmented_path(true);
                if shell_path != fallback_path {
                    return resolve_codex_in(shell_path).map_err(|shell_err| {
                        format!("{shell_err}; fallback search also failed: {fallback_err}")
                    });
                }
            }
            Err(fallback_err)
        }
    }
}

fn command_output_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> Option<std::process::Output> {
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Whether the Codex CLI is detected on this machine, and if so where + which
/// version. Drives the agent-status panel (no session needed).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexInfo {
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
}

/// Structured auth status from Codex app-server `getAuthStatus`. Never includes
/// tokens; Cameo only needs to know whether the user's local Codex can run.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAuthStatus {
    pub auth_method: Option<String>,
    pub requires_openai_auth: bool,
    pub requires_login: bool,
    pub auth_supported: bool,
}

pub fn detect() -> CodexInfo {
    match resolve_codex() {
        Ok(resolved) => {
            let mut cmd = std::process::Command::new(&resolved.path);
            crate::process::hide_console_window(&mut cmd);
            cmd.arg("--version")
                .env("PATH", &resolved.search_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let version = command_output_with_timeout(cmd, VERSION_TIMEOUT)
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty());
            CodexInfo {
                found: true,
                path: Some(resolved.path.to_string_lossy().to_string()),
                version,
            }
        }
        Err(_) => CodexInfo {
            found: false,
            path: None,
            version: None,
        },
    }
}

fn parse_auth_status(res: &Value) -> CodexAuthStatus {
    let auth_method = res
        .get("authMethod")
        .and_then(|v| v.as_str())
        .map(String::from);
    let requires_openai_auth = res
        .get("requiresOpenaiAuth")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let auth_supported = is_supported_auth_method(auth_method.as_deref());
    CodexAuthStatus {
        requires_login: auth_method.is_none() && requires_openai_auth,
        auth_supported,
        auth_method,
        requires_openai_auth,
    }
}

fn is_supported_auth_method(method: Option<&str>) -> bool {
    matches!(
        method,
        Some("chatgpt" | "chatgptAuthTokens" | "agentIdentity" | "apikey")
    )
}

fn is_rpc_response_for(msg: &Value, id: u64) -> bool {
    msg.get("id").and_then(|v| v.as_u64()) == Some(id) && msg.get("method").is_none()
}

async fn probe_call(
    stdin: &mut ChildStdin,
    lines: &mut Lines<BufReader<ChildStdout>>,
    id: u64,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let line = format!(
        "{}\n",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|e| e.to_string())?
    );
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.flush().await.map_err(|e| e.to_string())?;

    let wait_for_response = async {
        loop {
            let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? else {
                return Err("codex app-server exited during auth probe".to_string());
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if !is_rpc_response_for(&msg, id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                return Err(compact_json(err, 500));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    };

    tokio::time::timeout(timeout, wait_for_response)
        .await
        .map_err(|_| format!("rpc '{method}' timed out after {}ms", timeout.as_millis()))?
}

async fn probe_notify(stdin: &mut ChildStdin, method: &str, params: Value) {
    let Ok(line) = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })) else {
        return;
    };
    let _ = stdin.write_all(format!("{line}\n").as_bytes()).await;
    let _ = stdin.flush().await;
}

async fn cleanup_probe_child(child: &mut Child, pid: u32) {
    #[cfg(unix)]
    {
        kill_tree(pid, 15);
        tokio::time::sleep(Duration::from_millis(300)).await;
        kill_tree(pid, 9);
    }
    #[cfg(not(unix))]
    kill_tree(pid, 9);
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Probe local Codex auth without requiring an open Board session. This is used
/// by the status popover before `thread/start`, so "installed but not logged in"
/// can be shown as its own product state.
pub async fn probe_auth() -> Result<CodexAuthStatus, String> {
    let codex = resolve_codex()?;
    let mut cmd = tokio::process::Command::new(&codex.path);
    crate::process::hide_tokio_console_window(&mut cmd);
    cmd.arg("app-server")
        .env("PATH", &codex.search_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::proxy::apply_to_subprocess(&mut cmd, Some(&crate::config::load().proxy));
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn codex app-server for auth probe: {e}"))?;
    let pid = child.id().unwrap_or(0);
    let Some(mut stdin) = child.stdin.take() else {
        cleanup_probe_child(&mut child, pid).await;
        return Err("auth probe: no stdin".into());
    };
    let Some(stdout) = child.stdout.take() else {
        cleanup_probe_child(&mut child, pid).await;
        return Err("auth probe: no stdout".into());
    };
    let mut lines = BufReader::new(stdout).lines();

    let result = async {
        probe_call(
            &mut stdin,
            &mut lines,
            1,
            "initialize",
            json!({
                "clientInfo": { "name": "Cameo", "title": null, "version": env!("CARGO_PKG_VERSION") },
                "capabilities": null
            }),
            AUTH_PROBE_INIT_TIMEOUT,
        )
        .await?;
        probe_notify(&mut stdin, "initialized", json!({})).await;
        let auth = probe_call(
            &mut stdin,
            &mut lines,
            2,
            "getAuthStatus",
            json!({ "includeToken": false, "refreshToken": false }),
            AUTH_PROBE_TIMEOUT,
        )
        .await?;
        Ok(parse_auth_status(&auth))
    }
    .await;

    cleanup_probe_child(&mut child, pid).await;
    result
}

#[cfg(not(windows))]
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn batch_escape(value: &str) -> String {
    value.replace('%', "%%").replace('"', "")
}

fn terminal_script_dir() -> Result<PathBuf, String> {
    let dir = crate::paths::cameo_data_dir().join("terminal");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    prune_terminal_scripts(&dir);
    Ok(dir)
}

fn prune_terminal_scripts(dir: &Path) {
    const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_terminal_script = name.starts_with("cameo-codex-")
            && matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("command" | "cmd")
            );
        if !is_terminal_script {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .map(|age| age > MAX_AGE)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(not(windows))]
fn write_unix_terminal_script(stem: &str, body: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let path = terminal_script_dir()?.join(format!("{stem}-{}.command", uuid::Uuid::new_v4()));
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    let mut perms = std::fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(&path, perms).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(windows)]
fn write_windows_terminal_script(stem: &str, body: &str) -> Result<PathBuf, String> {
    let path = terminal_script_dir()?.join(format!("{stem}-{}.cmd", uuid::Uuid::new_v4()));
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(path)
}

fn open_terminal_script(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string());
    }

    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string())
    }
}

/// Open a visible terminal that installs the user's local Codex CLI.
pub fn open_install_terminal() -> Result<(), String> {
    let search_path = build_augmented_path(true);

    #[cfg(not(windows))]
    let script = {
        let path = sh_quote(&search_path);
        format!(
            r#"#!/bin/zsh
set +e
export PATH={path}
echo "Cameo is installing Codex CLI..."
echo
npm install -g @openai/codex
status=$?
echo
if [ "$status" -eq 0 ]; then
  echo "Verifying installation..."
  hash -r
  codex --version
  echo
  echo "Next: return to Cameo and click Re-detect."
else
  echo "Install failed with exit code $status."
  echo "Check the terminal output above, then retry."
fi
echo
echo "Press any key to close this window."
read -rsn 1
"#
        )
    };

    #[cfg(windows)]
    let script = {
        let path = batch_escape(&search_path);
        format!(
            r#"@echo off
setlocal
set "PATH={path}"
echo Cameo is installing Codex CLI...
echo.
call npm install -g @openai/codex
set "STATUS=%ERRORLEVEL%"
echo.
if "%STATUS%"=="0" (
  echo Verifying installation...
  call codex --version
  echo.
  echo Next: return to Cameo and click Re-detect.
) else (
  echo Install failed with exit code %STATUS%.
  echo Check the terminal output above, then retry.
)
echo.
pause
"#
        )
    };

    #[cfg(not(windows))]
    let path = write_unix_terminal_script("cameo-codex-install", &script)?;
    #[cfg(windows)]
    let path = write_windows_terminal_script("cameo-codex-install", &script)?;
    open_terminal_script(&path)
}

/// Open a visible terminal that runs the user's local `codex login`.
pub fn open_login_terminal() -> Result<(), String> {
    let codex = resolve_codex()?;

    #[cfg(not(windows))]
    let script = {
        let search_path = sh_quote(&codex.search_path);
        let codex_path = sh_quote(&codex.path.to_string_lossy());
        format!(
            r#"#!/bin/zsh
set +e
export PATH={search_path}
echo "Cameo is opening Codex login..."
echo
{codex_path} login
echo
echo "After login finishes, return to Cameo and click Re-detect."
echo
echo "Press any key to close this window."
read -rsn 1
"#
        )
    };

    #[cfg(windows)]
    let script = {
        let search_path = batch_escape(&codex.search_path);
        let codex_path = batch_escape(&codex.path.to_string_lossy());
        format!(
            r#"@echo off
setlocal
set "PATH={search_path}"
echo Cameo is opening Codex login...
echo.
call "{codex_path}" login
echo.
echo After login finishes, return to Cameo and click Re-detect.
echo.
pause
"#
        )
    };

    #[cfg(not(windows))]
    let path = write_unix_terminal_script("cameo-codex-login", &script)?;
    #[cfg(windows)]
    let path = write_windows_terminal_script("cameo-codex-login", &script)?;
    open_terminal_script(&path)
}

// ── Per-Board session state ──────────────────────────────────────────────────

type Pending = PlMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

#[derive(Debug, Clone)]
struct QueuedSteer {
    input: Value,
    source_placement_ids: Vec<String>,
    overlays: Vec<String>,
}

pub struct CodexSessionInner {
    app: AppHandle,
    registry: Arc<BoardRegistry>,
    board_id: String,
    folder: PathBuf,
    stdin: TokioMutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    /// The active session's Codex thread (used for turn/start).
    thread_id: PlMutex<String>,
    /// Active session id (v0.0.2 multi-session).
    active_session_id: PlMutex<String>,
    /// True from accepted `turn/start` dispatch until `turn/completed` (or process
    /// teardown). While true, additional user input is delivered to Codex via
    /// `turn/steer` instead of being rejected.
    turn_in_flight: PlMutex<bool>,
    current_turn_id: PlMutex<Option<String>>,
    /// User inputs submitted after `turn/start` but before Codex has returned /
    /// emitted the concrete turn id. Flushed in order via `turn/steer`.
    pending_steers: PlMutex<VecDeque<QueuedSteer>>,
    /// Serializes `turn/steer` RPCs so queued user inputs keep their UI order.
    steer_flush: TokioMutex<()>,
    /// Placement ids referenced by the in-flight turn (drives output placement).
    current_sources: PlMutex<Vec<String>>,
    /// Overlay temp files written for the in-flight turn. Deleted only after a
    /// terminal runtime event or failed turn/start, so Codex can read them while
    /// the turn is active.
    current_overlays: PlMutex<Vec<String>>,
    /// Generated outputs so far in the current turn (vertical stacking index).
    output_index: AtomicU64,
    /// Accumulated agentMessage text per item id, for tail backfill.
    agent_accum: PlMutex<HashMap<String, String>>,
    /// imageGeneration item id → (loading placeholder id, layout index), set at
    /// item/started so the final image lands where the placeholder showed.
    pending_gen: PlMutex<HashMap<String, (String, i64)>>,
    /// Set before intentional teardown paths so the reader EOF doesn't emit a
    /// stale SessionComplete into a board that is already restarting/switching.
    intentional_shutdown: PlMutex<bool>,
    /// Per-Board generation knobs (model/effort/serviceTier). Seeded from meta at
    /// session start, updated by `set_gen_settings`, and read on EVERY `turn/start`
    /// so all dispatch paths (composer / preset / context menu) apply them without
    /// each caller having to thread params through. See CODEX_PROTOCOL.md §4.
    gen: PlMutex<crate::model::GenSettings>,
    /// Assistant chat blocks accumulated during the in-flight turn, in arrival
    /// order (text segments + generated images). Flushed to the active session's
    /// timeline at turn end. This makes the message timeline AUTHORITATIVELY
    /// persisted by the runtime — bound to the turn's session, independent of
    /// which Board the UI happens to be focused on — instead of relying on a
    /// best-effort frontend call that the active-board / turn-state gates could
    /// silently drop (the v0.1.6 data-loss fix).
    turn_blocks: PlMutex<Vec<Value>>,
}

pub struct CodexSession {
    pub inner: Arc<CodexSessionInner>,
    child: TokioMutex<Child>,
    pid: u32,
}

#[derive(Default)]
pub struct CodexRegistry {
    inner: PlMutex<HashMap<String, Arc<CodexSession>>>,
}

impl CodexRegistry {
    pub fn get(&self, board_id: &str) -> Option<Arc<CodexSession>> {
        self.inner.lock().get(board_id).cloned()
    }
    fn insert(&self, board_id: String, session: Arc<CodexSession>) {
        self.inner.lock().insert(board_id, session);
    }
    fn remove(&self, board_id: &str) -> Option<Arc<CodexSession>> {
        self.inner.lock().remove(board_id)
    }
    fn remove_if_same(&self, board_id: &str, target: &Arc<CodexSession>) -> bool {
        let mut inner = self.inner.lock();
        let Some(current) = inner.get(board_id) else {
            return false;
        };
        if !Arc::ptr_eq(current, target) {
            return false;
        }
        inner.remove(board_id);
        true
    }
    pub fn board_ids(&self) -> Vec<String> {
        self.inner.lock().keys().cloned().collect()
    }

    /// Synchronous teardown for app exit — SIGKILL each session's process group
    /// directly (no async RPC / sleep). Used from the RunEvent handler, where
    /// blocking on the async runtime during shutdown is deadlock-prone.
    pub fn kill_all_sync(&self) {
        let sessions: Vec<Arc<CodexSession>> = self.inner.lock().drain().map(|(_, s)| s).collect();
        for s in sessions {
            // SIGKILL the group (unix) / taskkill /T /F the tree (windows).
            kill_tree(s.pid, 9);
        }
    }
}

impl CodexSessionInner {
    fn emit(&self, event: UnifiedEvent) {
        let env = CodexEventEnvelope {
            board_id: self.board_id.clone(),
            event,
        };
        let _ = self.app.emit(CODEX_EVENT, env);
    }

    async fn write(&self, msg: &Value) -> Result<(), String> {
        let line = format!(
            "{}\n",
            serde_json::to_string(msg).map_err(|e| e.to_string())?
        );
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn call(&self, method: &str, params: Value, timeout_ms: u64) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        if let Err(e) = self
            .write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await
        {
            self.pending.lock().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(Duration::from_millis(timeout_ms), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => {
                self.pending.lock().remove(&id);
                Err("rpc channel closed (process exited?)".into())
            }
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(format!("rpc '{method}' timed out after {timeout_ms}ms"))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) {
        let _ = self
            .write(&json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await;
    }

    async fn respond(&self, id: u64, result: Value) {
        let _ = self
            .write(&json!({"jsonrpc":"2.0","id":id,"result":result}))
            .await;
    }
}

fn is_stale_thread(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("no rollout found")
        || e.contains("thread not found")
        || e.contains("conversation not found")
}

async fn discard_session(
    codex_reg: &Arc<CodexRegistry>,
    board_id: &str,
    session: &Arc<CodexSession>,
) {
    if !codex_reg.remove_if_same(board_id, session) {
        return;
    }
    *session.inner.intentional_shutdown.lock() = true;
    session.inner.pending.lock().clear();
    #[cfg(unix)]
    {
        kill_tree(session.pid, 15);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    kill_tree(session.pid, 9);
    let mut child = session.child.lock().await;
    let _ = child.start_kill();
}

fn is_overlay_temp_path(path: &str) -> bool {
    let p = Path::new(path);
    p.components().count() == 1 && path.starts_with(".overlay-") && path.ends_with(".png")
}

fn cleanup_overlay_paths(folder: &Path, overlays: Vec<String>) {
    for rel in overlays {
        if !is_overlay_temp_path(&rel) {
            tracing::warn!(module = "codex", overlay = %rel, "skip unsafe overlay cleanup path");
            continue;
        }
        if let Err(e) = std::fs::remove_file(folder.join(&rel)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(module = "codex", overlay = %rel, "overlay cleanup failed: {e}");
            }
        }
    }
}

fn cleanup_dispatch_overlays(inner: &Arc<CodexSessionInner>) {
    let overlays = {
        let mut current = inner.current_overlays.lock();
        std::mem::take(&mut *current)
    };
    cleanup_overlay_paths(&inner.folder, overlays);
}

fn clear_active_turn(inner: &Arc<CodexSessionInner>) {
    *inner.turn_in_flight.lock() = false;
    *inner.current_turn_id.lock() = None;
}

fn clear_pending_steers(inner: &Arc<CodexSessionInner>) {
    let overlays = {
        let mut queued = inner.pending_steers.lock();
        queued.drain(..).flat_map(|q| q.overlays).collect()
    };
    cleanup_overlay_paths(&inner.folder, overlays);
}

fn clear_stream_state(inner: &Arc<CodexSessionInner>) {
    inner.agent_accum.lock().clear();
    inner.current_sources.lock().clear();
    inner.pending_gen.lock().clear();
    clear_pending_steers(inner);
}

// ── Public API (used by commands) ────────────────────────────────────────────

/// Start (or no-op if already running) the Codex session for a Board.
pub async fn start_session(
    app: AppHandle,
    board_reg: Arc<BoardRegistry>,
    codex_reg: Arc<CodexRegistry>,
    board_id: String,
) -> Result<String, String> {
    if let Some(existing) = codex_reg.get(&board_id) {
        let thread_id = existing.inner.thread_id.lock().clone();
        if !thread_id.is_empty() {
            return Ok(thread_id);
        }
        tracing::warn!(module = "codex", board = %board_id, "discarding uninitialized codex session");
        discard_session(&codex_reg, &board_id, &existing).await;
    }
    let folder = board_reg.folder(&board_id).ok_or("unknown board")?;
    let codex = resolve_codex()?;

    let mut cmd = tokio::process::Command::new(&codex.path);
    crate::process::hide_tokio_console_window(&mut cmd);
    cmd.arg("app-server")
        .current_dir(&folder)
        .env("PATH", &codex.search_path)
        .env("PWD", &folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        // Own process group → tree-kill can take down the whole model/tool tree.
        cmd.process_group(0);
    }
    // Inject the user-configured network proxy into the sidecar env. Config is
    // re-read per spawn, so a Settings change applies on the next session start
    // (the frontend restarts the active session on save).
    crate::proxy::apply_to_subprocess(&mut cmd, Some(&crate::config::load().proxy));
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn codex app-server: {e}"))?;
    let pid = child.id().unwrap_or(0);
    let Some(stdin) = child.stdin.take() else {
        kill_tree(pid, 9);
        let _ = child.start_kill();
        return Err("no stdin".into());
    };
    let Some(stdout) = child.stdout.take() else {
        kill_tree(pid, 9);
        let _ = child.start_kill();
        return Err("no stdout".into());
    };
    let Some(stderr) = child.stderr.take() else {
        kill_tree(pid, 9);
        let _ = child.start_kill();
        return Err("no stderr".into());
    };

    let inner = Arc::new(CodexSessionInner {
        app,
        registry: board_reg,
        board_id: board_id.clone(),
        folder: folder.clone(),
        stdin: TokioMutex::new(stdin),
        pending: PlMutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        thread_id: PlMutex::new(String::new()),
        active_session_id: PlMutex::new(String::new()),
        turn_in_flight: PlMutex::new(false),
        current_turn_id: PlMutex::new(None),
        pending_steers: PlMutex::new(VecDeque::new()),
        steer_flush: TokioMutex::new(()),
        current_sources: PlMutex::new(Vec::new()),
        current_overlays: PlMutex::new(Vec::new()),
        output_index: AtomicU64::new(0),
        agent_accum: PlMutex::new(HashMap::new()),
        pending_gen: PlMutex::new(HashMap::new()),
        intentional_shutdown: PlMutex::new(false),
        gen: PlMutex::new(gen_from_meta(&storage::load_meta(&folder))),
        turn_blocks: PlMutex::new(Vec::new()),
    });

    tauri::async_runtime::spawn(reader_loop(inner.clone(), stdout));
    tauri::async_runtime::spawn(stderr_drain(inner.clone(), stderr));

    let session = Arc::new(CodexSession {
        inner: inner.clone(),
        child: TokioMutex::new(child),
        pid,
    });

    codex_reg.insert(board_id.clone(), session.clone());

    // Handshake.
    if let Err(e) = inner
        .call(
            "initialize",
            json!({
                "clientInfo": { "name": "Cameo", "title": null, "version": env!("CARGO_PKG_VERSION") },
                "capabilities": null
            }),
            15_000,
        )
        .await
    {
        discard_session(&codex_reg, &board_id, &session).await;
        return Err(e);
    }
    inner.notify("initialized", json!({})).await;

    match get_auth_status(&inner).await {
        Ok(auth) => {
            if auth.requires_login {
                discard_session(&codex_reg, &board_id, &session).await;
                return Err("Codex is installed but not logged in. Run `codex login`.".into());
            }
            if auth.auth_method.is_some() && !auth.auth_supported {
                discard_session(&codex_reg, &board_id, &session).await;
                return Err("Cameo requires Codex signed in with ChatGPT. Run `codex login`.".into());
            }
        }
        Err(e) => {
            tracing::warn!(
                module = "codex",
                error = %e,
                "could not preflight codex auth; continuing with thread start"
            );
        }
    }

    // Resume (or migrate from legacy meta.threadId) the ACTIVE session's thread.
    let legacy = storage::load_meta(&folder).thread_id.clone();
    let sessions = session::ensure_initial(&folder, legacy);
    let active = sessions.active_session_id.clone().unwrap_or_default();
    let prev = sessions
        .sessions
        .iter()
        .find(|s| s.id == active)
        .and_then(|s| s.thread_id.clone());

    let thread_id = match ensure_thread(&inner, &folder, &active, prev).await {
        Ok(thread_id) => thread_id,
        Err(e) => {
            discard_session(&codex_reg, &board_id, &session).await;
            return Err(e);
        }
    };
    *inner.active_session_id.lock() = active.clone();
    *inner.thread_id.lock() = thread_id.clone();

    let mut meta = storage::load_meta(&folder);
    meta.runtime = Some("codex".into());
    meta.active_session_id = Some(active.clone());
    storage::save_meta(&folder, &meta);

    inner.emit(UnifiedEvent::SessionInit {
        thread_id: thread_id.clone(),
        model: String::new(),
    });
    tracing::info!(module = "codex", board = %board_id, session = %active, thread = %thread_id, "codex session ready");
    Ok(thread_id)
}

/// Resume the given thread (or thread/start a fresh one on stale / none) and
/// persist the resolved threadId onto the session. Returns the threadId.
async fn ensure_thread(
    inner: &Arc<CodexSessionInner>,
    folder: &Path,
    session_id: &str,
    prev: Option<String>,
) -> Result<String, String> {
    let dev = prompt::build_developer_instructions();
    let (approval, sandbox) = ("never", "workspace-write");
    let id = if let Some(prev) = prev {
        match inner
            .call(
                "thread/resume",
                json!({ "threadId": prev, "model": null, "approvalPolicy": approval, "sandbox": sandbox, "developerInstructions": dev }),
                30_000,
            )
            .await
        {
            Ok(res) => thread_id_of(&res).unwrap_or(prev),
            Err(e) if is_stale_thread(&e) => {
                tracing::warn!(module = "codex", "stale thread {prev}; starting fresh: {e}");
                let res = inner.call("thread/start", new_thread_params(folder, approval, sandbox, &dev), 30_000).await?;
                thread_id_of(&res).ok_or("thread/start: no thread id")?
            }
            Err(e) => return Err(e),
        }
    } else {
        let res = inner
            .call(
                "thread/start",
                new_thread_params(folder, approval, sandbox, &dev),
                30_000,
            )
            .await?;
        thread_id_of(&res).ok_or("thread/start: no thread id")?
    };
    session::set_thread(folder, session_id, &id);
    Ok(id)
}

/// Create a new session (fresh thread) and make it active.
pub async fn new_session(
    codex_reg: Arc<CodexRegistry>,
    board_id: String,
) -> Result<String, String> {
    let session = codex_reg.get(&board_id).ok_or("session not started")?;
    let inner = &session.inner;
    if *inner.turn_in_flight.lock() {
        return Err("Cannot create a new session while Codex is working.".into());
    }
    let folder = inner.folder.clone();
    let meta = session::new_session(&folder);
    let thread_id = ensure_thread(inner, &folder, &meta.id, None).await?;
    *inner.active_session_id.lock() = meta.id.clone();
    *inner.thread_id.lock() = thread_id;
    let mut m = storage::load_meta(&folder);
    m.active_session_id = Some(meta.id.clone());
    storage::save_meta(&folder, &m);
    Ok(meta.id)
}

/// Switch the active session (resume its thread).
pub async fn switch_session(
    codex_reg: Arc<CodexRegistry>,
    board_id: String,
    session_id: String,
) -> Result<(), String> {
    let session = codex_reg.get(&board_id).ok_or("session not started")?;
    let inner = &session.inner;
    if *inner.turn_in_flight.lock() {
        return Err("Cannot switch sessions while Codex is working.".into());
    }
    let folder = inner.folder.clone();
    let prev = session::thread_of(&folder, &session_id);
    let thread_id = ensure_thread(inner, &folder, &session_id, prev).await?;
    session::set_active(&folder, &session_id);
    *inner.active_session_id.lock() = session_id.clone();
    *inner.thread_id.lock() = thread_id;
    let mut m = storage::load_meta(&folder);
    m.active_session_id = Some(session_id);
    storage::save_meta(&folder, &m);
    Ok(())
}

fn new_thread_params(folder: &Path, approval: &str, sandbox: &str, dev: &str) -> Value {
    json!({
        "cwd": folder.to_string_lossy(),
        "model": null,
        "approvalPolicy": approval,
        "sandbox": sandbox,
        "developerInstructions": dev,
        "ephemeral": false,
    })
}

/// Product defaults for generation knobs when the Board has no saved choice.
/// `serviceTier` has no default constant — `None` is sent as JSON null (standard).
pub const DEFAULT_GEN_MODEL: &str = "gpt-5.5";
pub const DEFAULT_GEN_EFFORT: &str = "medium";

fn gen_from_meta(m: &crate::model::BoardMeta) -> crate::model::GenSettings {
    crate::model::GenSettings {
        model: m.gen_model.clone(),
        effort: m.gen_effort.clone(),
        service_tier: m.gen_service_tier.clone(),
    }
}

/// A model from `model/list`, projected to what the composer menu needs. Field
/// names mirror the wire `Model` (camelCase), NOT `~/.codex/models_cache.json`'s
/// snake_case — see CODEX_PROTOCOL.md §6.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceTierInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// Identifier to pass back as `turn/start.model` (wire `id`, fallback `model`).
    pub id: String,
    pub display_name: String,
    pub default_reasoning_effort: Option<String>,
    pub supported_efforts: Vec<String>,
    pub service_tiers: Vec<ServiceTierInfo>,
    pub default_service_tier: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInputRef {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub path: String,
    pub description: String,
    pub enabled: bool,
    pub scope: String,
    pub display_name: Option<String>,
    pub short_description: Option<String>,
}

fn parse_model(m: &Value) -> Option<ModelInfo> {
    let id = m
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| m.get("model").and_then(|v| v.as_str()))?
        .to_string();
    let display_name = m
        .get("displayName")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let supported_efforts = m
        .get("supportedReasoningEfforts")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| o.get("reasoningEffort").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let service_tiers = m
        .get("serviceTiers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(ServiceTierInfo {
                        id: o.get("id").and_then(|v| v.as_str())?.to_string(),
                        name: o.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        description: o
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ModelInfo {
        id,
        display_name,
        default_reasoning_effort: m
            .get("defaultReasoningEffort")
            .and_then(|v| v.as_str())
            .map(String::from),
        supported_efforts,
        service_tiers,
        default_service_tier: m
            .get("defaultServiceTier")
            .and_then(|v| v.as_str())
            .map(String::from),
    })
}

/// Enumerate models via `model/list` (paginated). Drives the composer menu.
pub async fn list_models(
    codex_reg: Arc<CodexRegistry>,
    board_id: String,
) -> Result<Vec<ModelInfo>, String> {
    let session = codex_reg.get(&board_id).ok_or("session not started")?;
    let inner = &session.inner;
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    // Page cap guards a malformed server whose `nextCursor` never advances
    // (would otherwise loop forever). Always read `nextCursor` BEFORE deciding to
    // stop, so an empty-but-paginated page doesn't truncate the list.
    for _ in 0..20 {
        let params = match &cursor {
            Some(c) => json!({ "includeHidden": false, "cursor": c }),
            None => json!({ "includeHidden": false }),
        };
        let res = inner.call("model/list", params, 15_000).await?;
        if let Some(arr) = res.get("data").and_then(|d| d.as_array()) {
            for m in arr {
                if let Some(info) = parse_model(m) {
                    out.push(info);
                }
            }
        }
        match res.get("nextCursor").and_then(|c| c.as_str()) {
            // Advance only on a genuinely new cursor; stop on null or non-advancing.
            Some(next) if Some(next) != cursor.as_deref() => cursor = Some(next.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

fn parse_skill(s: &Value) -> Option<SkillInfo> {
    let interface = s.get("interface");
    Some(SkillInfo {
        name: s.get("name")?.as_str()?.to_string(),
        path: s.get("path")?.as_str()?.to_string(),
        description: s
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        enabled: s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        scope: s
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        display_name: interface
            .and_then(|i| i.get("displayName"))
            .and_then(|v| v.as_str())
            .map(String::from),
        short_description: interface
            .and_then(|i| i.get("shortDescription"))
            .and_then(|v| v.as_str())
            .or_else(|| s.get("shortDescription").and_then(|v| v.as_str()))
            .map(String::from),
    })
}

/// Enumerate native Codex Skills for the Board's working directory.
pub async fn list_skills(
    codex_reg: Arc<CodexRegistry>,
    board_id: String,
    force_reload: bool,
) -> Result<Vec<SkillInfo>, String> {
    let session = codex_reg.get(&board_id).ok_or("session not started")?;
    let inner = &session.inner;
    let res = inner
        .call(
            "skills/list",
            json!({
                "cwds": [inner.folder.to_string_lossy()],
                "forceReload": force_reload,
            }),
            15_000,
        )
        .await?;
    Ok(parse_skills_response(&res))
}

pub async fn list_skills_for_folder(
    folder: PathBuf,
    force_reload: bool,
) -> Result<Vec<SkillInfo>, String> {
    let codex = match resolve_codex() {
        Ok(codex) => codex,
        Err(e) => {
            tracing::warn!(
                module = "codex",
                error = %e,
                "codex executable not found; scanning local SKILL.md files"
            );
            return Ok(list_local_skills());
        }
    };
    let mut cmd = tokio::process::Command::new(&codex.path);
    crate::process::hide_tokio_console_window(&mut cmd);
    cmd.arg("app-server")
        .env("PATH", &codex.search_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::proxy::apply_to_subprocess(&mut cmd, Some(&crate::config::load().proxy));
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(
                module = "codex",
                error = %e,
                "spawn codex app-server for skills/list failed; scanning local SKILL.md files"
            );
            return Ok(list_local_skills());
        }
    };
    let pid = child.id().unwrap_or(0);
    let Some(mut stdin) = child.stdin.take() else {
        cleanup_probe_child(&mut child, pid).await;
        tracing::warn!(
            module = "codex",
            "codex app-server for skills/list had no stdin; scanning local SKILL.md files"
        );
        return Ok(list_local_skills());
    };
    let Some(stdout) = child.stdout.take() else {
        cleanup_probe_child(&mut child, pid).await;
        tracing::warn!(
            module = "codex",
            "codex app-server for skills/list had no stdout; scanning local SKILL.md files"
        );
        return Ok(list_local_skills());
    };
    let mut lines = BufReader::new(stdout).lines();

    let result: Result<Vec<SkillInfo>, String> = async {
        probe_call(
            &mut stdin,
            &mut lines,
            1,
            "initialize",
            json!({
                "clientInfo": { "name": "Cameo", "title": null, "version": env!("CARGO_PKG_VERSION") },
                "capabilities": null
            }),
            AUTH_PROBE_INIT_TIMEOUT,
        )
        .await?;
        probe_notify(&mut stdin, "initialized", json!({})).await;
        let res = probe_call(
            &mut stdin,
            &mut lines,
            2,
            "skills/list",
            json!({
                "cwds": [folder.to_string_lossy()],
                "forceReload": force_reload,
            }),
            Duration::from_secs(15),
        )
        .await?;
        Ok(parse_skills_response(&res))
    }
    .await;
    cleanup_probe_child(&mut child, pid).await;
    match result {
        Ok(skills) => Ok(skills),
        Err(e) => {
            tracing::warn!(
                module = "codex",
                error = %e,
                "codex skills/list failed; scanning local SKILL.md files"
            );
            Ok(list_local_skills())
        }
    }
}

fn parse_skills_response(res: &Value) -> Vec<SkillInfo> {
    let mut out = Vec::new();
    if let Some(entries) = res.get("data").and_then(|v| v.as_array()) {
        for entry in entries {
            if let Some(skills) = entry.get("skills").and_then(|v| v.as_array()) {
                out.extend(skills.iter().filter_map(parse_skill));
            }
        }
    }
    out.sort_by(|a, b| {
        let an = a.display_name.as_deref().unwrap_or(&a.name).to_lowercase();
        let bn = b.display_name.as_deref().unwrap_or(&b.name).to_lowercase();
        an.cmp(&bn).then_with(|| a.path.cmp(&b.path))
    });
    out.dedup_by(|a, b| a.name == b.name && a.path == b.path);
    out
}

pub fn list_local_skills() -> Vec<SkillInfo> {
    let mut out = Vec::new();
    for home in local_skill_homes() {
        for root in [
            home.join(".codex").join("skills"),
            home.join(".agents").join("skills"),
            home.join(".claude").join("skills"),
        ] {
            scan_skill_root(&root, &mut out, 0);
        }
    }
    out.sort_by(|a, b| {
        let an = a.display_name.as_deref().unwrap_or(&a.name).to_lowercase();
        let bn = b.display_name.as_deref().unwrap_or(&b.name).to_lowercase();
        an.cmp(&bn).then_with(|| a.path.cmp(&b.path))
    });
    dedup_local_skill_names(&mut out);
    out.truncate(3_000);
    tracing::info!(
        module = "codex",
        count = out.len(),
        "local Codex Skills scanned"
    );
    out
}

fn dedup_local_skill_names(skills: &mut Vec<SkillInfo>) {
    skills.sort_by(|a, b| {
        canonical_skill_name(a)
            .cmp(&canonical_skill_name(b))
            .then_with(|| local_skill_priority(&a.path).cmp(&local_skill_priority(&b.path)))
            .then_with(|| a.path.cmp(&b.path))
    });
    skills.dedup_by(|a, b| canonical_skill_name(a) == canonical_skill_name(b));
    skills.sort_by(|a, b| {
        let an = a.display_name.as_deref().unwrap_or(&a.name).to_lowercase();
        let bn = b.display_name.as_deref().unwrap_or(&b.name).to_lowercase();
        an.cmp(&bn).then_with(|| a.path.cmp(&b.path))
    });
}

fn canonical_skill_name(skill: &SkillInfo) -> String {
    skill.name.trim().to_ascii_lowercase()
}

fn local_skill_priority(path: &str) -> u8 {
    if path.contains("/.codex/skills/") {
        0
    } else if path.contains("/.agents/skills/") {
        1
    } else if path.contains("/.claude/skills/") {
        2
    } else {
        3
    }
}

fn local_skill_homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(home) = dirs::home_dir() {
        push_unique(&mut homes, home);
    }
    if let Some(home) = std::env::var_os("HOME") {
        push_unique(&mut homes, PathBuf::from(home));
    }
    for key in ["USER", "LOGNAME"] {
        if let Some(user) = std::env::var_os(key).and_then(|v| v.into_string().ok()) {
            push_unique(&mut homes, PathBuf::from("/Users").join(user));
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(entries) = std::fs::read_dir("/Users") {
        for entry in entries.flatten() {
            let home = entry.path();
            if home.join(".codex").join("skills").exists()
                || home.join(".agents").join("skills").exists()
                || home.join(".claude").join("skills").exists()
            {
                push_unique(&mut homes, home);
            }
        }
    }
    homes
}

fn scan_skill_root(root: &Path, out: &mut Vec<SkillInfo>, depth: usize) {
    if depth > 8 || out.len() >= 3_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_skill_root(&path, out, depth + 1);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
            if let Some(skill) = parse_skill_file(&path) {
                out.push(skill);
            }
        }
        if out.len() >= 3_000 {
            return;
        }
    }
}

fn parse_skill_file(path: &Path) -> Option<SkillInfo> {
    let body = std::fs::read_to_string(path).ok()?;
    let meta = parse_skill_frontmatter(&body);
    let interface = parse_skill_openai_interface(path);
    let fallback_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("skill")
        .to_string();
    let name = meta
        .get("name")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_name);
    let description = meta
        .get("description")
        .cloned()
        .or_else(|| meta.get("short_description").cloned())
        .or_else(|| interface.get("short_description").cloned())
        .unwrap_or_default();
    let path_str = path.to_string_lossy().to_string();
    let scope = if path_str.contains("/.system/") || path_str.contains("/plugins/cache/") {
        "system"
    } else {
        "user"
    };
    Some(SkillInfo {
        name: name.clone(),
        path: path_str,
        description: description.clone(),
        enabled: true,
        scope: scope.into(),
        display_name: meta
            .get("display_name")
            .cloned()
            .or_else(|| interface.get("display_name").cloned()),
        short_description: meta
            .get("short_description")
            .cloned()
            .or_else(|| interface.get("short_description").cloned())
            .or_else(|| (!description.is_empty()).then_some(description)),
    })
}

fn parse_skill_frontmatter(body: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut lines = body.lines();
    if lines.next() != Some("---") {
        return out;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        out.insert(key.trim().to_ascii_lowercase(), value);
    }
    out
}

fn parse_skill_openai_interface(skill_path: &Path) -> HashMap<String, String> {
    let Some(dir) = skill_path.parent() else {
        return HashMap::new();
    };
    let Ok(body) = std::fs::read_to_string(dir.join("agents").join("openai.yaml")) else {
        return HashMap::new();
    };
    parse_simple_yaml_fields(&body, &["display_name", "short_description", "default_prompt"])
}

fn parse_simple_yaml_fields(body: &str, wanted: &[&str]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in body.lines() {
        let trimmed = line.trim();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if !wanted.iter().any(|wanted| *wanted == key) {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if !value.is_empty() {
            out.insert(key, value);
        }
    }
    out
}

/// Read the Board's saved generation knobs (no live session needed).
pub fn get_gen_settings(folder: &Path) -> crate::model::GenSettings {
    gen_from_meta(&storage::load_meta(folder))
}

/// Persist the Board's generation knobs to meta.json AND update the live session
/// (if any) so the next `turn/start` uses them. `service_tier == None` = standard.
pub fn set_gen_settings(
    codex_reg: &Arc<CodexRegistry>,
    folder: &Path,
    board_id: &str,
    settings: crate::model::GenSettings,
) {
    let mut meta = storage::load_meta(folder);
    meta.gen_model = settings.model.clone();
    meta.gen_effort = settings.effort.clone();
    meta.gen_service_tier = settings.service_tier.clone();
    storage::save_meta(folder, &meta);
    if let Some(session) = codex_reg.get(board_id) {
        *session.inner.gen.lock() = settings;
    }
}

// ── Authoritative message-timeline persistence (v0.1.6 data-loss fix) ─────────
//
// The runtime — not the frontend — owns writing the session timeline. It is the
// only component that reliably sees every event for the turn's session
// regardless of which Board the UI is focused on. Records use the SAME
// `ChatMessage` JSON the frontend renders, so `loadSession` is unchanged and old
// timelines stay compatible. Durable content only: user text + assistant text +
// generated images (thinking / tool steps are live-only process detail).

/// Append the user's message to the active session timeline at turn start.
fn persist_user_record(
    inner: &CodexSessionInner,
    text: &str,
    refs: &[String],
    skills: &[SkillInputRef],
) {
    let session_id = inner.active_session_id.lock().clone();
    if session_id.is_empty() {
        return;
    }
    let display_text = if skills.is_empty() {
        text.to_string()
    } else {
        let skill_line = format!(
            "Skills: {}",
            skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if text.trim().is_empty() {
            skill_line
        } else {
            format!("{text}\n\n{skill_line}")
        }
    };
    let msg = json!({
        "id": nanoid::nanoid!(12),
        "role": "user",
        "text": display_text,
        "refs": refs,
    });
    session::append_message(&inner.folder, &session_id, &msg);
}

/// Flush the accumulated assistant blocks for the just-finished turn. Idempotent
/// via `std::mem::take` (a later teardown flush after a clean turn/completed is a
/// no-op); skips turns with no durable content.
fn flush_turn_timeline(inner: &CodexSessionInner, status: &str, error: Option<&str>) {
    let blocks: Vec<Value> = {
        let mut b = inner.turn_blocks.lock();
        if b.is_empty() {
            return;
        }
        std::mem::take(&mut *b)
    };
    let session_id = inner.active_session_id.lock().clone();
    if session_id.is_empty() {
        return;
    }
    let mut msg = json!({
        "id": nanoid::nanoid!(12),
        "role": "assistant",
        "blocks": blocks,
        "status": if status == "completed" { "done" } else { "error" },
    });
    if let Some(e) = error {
        msg["error"] = json!(e);
    }
    session::append_message(&inner.folder, &session_id, &msg);
}

fn thread_id_of(res: &Value) -> Option<String> {
    res.get("thread")?.get("id")?.as_str().map(String::from)
}

fn turn_id_of(res: &Value) -> Option<String> {
    res.get("turn")
        .and_then(|t| t.get("id"))
        .or_else(|| res.get("turnId"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn turn_steer_params(thread_id: &str, turn_id: &str, input: Value) -> Value {
    json!({
        "threadId": thread_id,
        "input": input,
        "expectedTurnId": turn_id,
    })
}

fn resolve_refs(
    inner: &Arc<CodexSessionInner>,
    board_id: &str,
    source_placement_ids: &[String],
    overlay_map: &HashMap<String, String>,
) -> Result<Vec<(String, Option<String>)>, String> {
    let Some(entry) = inner.registry.get(board_id) else {
        return Err("unknown board".into());
    };
    let doc = entry.doc.lock();
    Ok(source_placement_ids
        .iter()
        .filter_map(|pid| {
            let p = doc.placements.iter().find(|p| &p.id == pid)?;
            let a = doc.assets.iter().find(|a| a.id == p.asset_id)?;
            Some((a.path.clone(), overlay_map.get(pid).cloned()))
        })
        .collect())
}

fn queue_steer(
    inner: &Arc<CodexSessionInner>,
    input: Value,
    source_placement_ids: Vec<String>,
    overlays: Vec<String>,
) -> usize {
    let mut pending = inner.pending_steers.lock();
    pending.push_back(QueuedSteer {
        input,
        source_placement_ids,
        overlays,
    });
    pending.len()
}

async fn dispatch_steer_unlocked(
    inner: &Arc<CodexSessionInner>,
    thread_id: &str,
    turn_id: &str,
    input: Value,
    source_placement_ids: Vec<String>,
    overlays: Vec<String>,
) -> Result<(), String> {
    match inner
        .call(
            "turn/steer",
            turn_steer_params(thread_id, turn_id, input),
            15_000,
        )
        .await
    {
        Ok(_) => {
            if !source_placement_ids.is_empty() {
                *inner.current_sources.lock() = source_placement_ids;
            }
            if !overlays.is_empty() {
                inner.current_overlays.lock().extend(overlays);
            }
            tracing::info!(module = "codex", turn = %turn_id, "turn/steer ok");
            Ok(())
        }
        Err(e) => {
            cleanup_overlay_paths(&inner.folder, overlays);
            Err(e)
        }
    }
}

async fn send_steer(
    inner: &Arc<CodexSessionInner>,
    thread_id: String,
    turn_id: String,
    input: Value,
    source_placement_ids: Vec<String>,
    overlays: Vec<String>,
) -> Result<(), String> {
    let _guard = inner.steer_flush.lock().await;
    dispatch_steer_unlocked(
        inner,
        &thread_id,
        &turn_id,
        input,
        source_placement_ids,
        overlays,
    )
    .await
}

async fn flush_pending_steers(inner: &Arc<CodexSessionInner>) {
    let _guard = inner.steer_flush.lock().await;
    loop {
        let Some(turn_id) = inner.current_turn_id.lock().clone() else {
            return;
        };
        let thread_id = inner.thread_id.lock().clone();
        if thread_id.is_empty() {
            return;
        }
        let Some(queued) = inner.pending_steers.lock().pop_front() else {
            return;
        };
        if let Err(e) = dispatch_steer_unlocked(
            inner,
            &thread_id,
            &turn_id,
            queued.input,
            queued.source_placement_ids,
            queued.overlays,
        )
        .await
        {
            emit_runtime_log(
                inner,
                "error",
                format!("Could not send queued message to active Codex turn: {e}"),
            );
        }
    }
}

/// Send a user message: resolve referenced placements → file paths in the
/// prompt (D4), record sources for output placement, then either start a new
/// Codex turn or steer the current active turn.
pub async fn send_message(
    codex_reg: Arc<CodexRegistry>,
    board_id: String,
    text: String,
    source_placement_ids: Vec<String>,
    overlays: Vec<(String, String)>,
    skills: Vec<SkillInputRef>,
) -> Result<(), String> {
    let session = codex_reg.get(&board_id).ok_or("session not started")?;
    let inner = &session.inner;
    let overlay_map: HashMap<String, String> = overlays.into_iter().collect();
    let new_overlays: Vec<String> = overlay_map.values().cloned().collect();

    let thread_id = inner.thread_id.lock().clone();
    if thread_id.is_empty() {
        cleanup_overlay_paths(&inner.folder, new_overlays);
        return Err("session has no thread yet".into());
    }

    // Resolve placement ids → (clean asset path, optional marking-overlay path).
    let refs = match resolve_refs(inner, &board_id, &source_placement_ids, &overlay_map) {
        Ok(refs) => refs,
        Err(e) => {
            cleanup_overlay_paths(&inner.folder, new_overlays);
            return Err(e);
        }
    };
    let prompt = build_turn_prompt(&text, &refs);
    let input = build_turn_input(&prompt, &skills);

    // Authoritatively persist the user message to the active session timeline at
    // submit time (covers both a fresh turn and a steered input). `text` is the
    // original instruction (not the agent-only reference preamble); refs are the
    // referenced placement ids — same shape the frontend renders. Skill names
    // are persisted for chat history display only; the native skill inputs stay
    // separate in `input[]`.
    persist_user_record(inner, &text, &source_placement_ids, &skills);

    let active_turn_id = { inner.current_turn_id.lock().clone() };
    if let Some(turn_id) = active_turn_id {
        let has_pending_steers = { !inner.pending_steers.lock().is_empty() };
        if !has_pending_steers {
            send_steer(
                inner,
                thread_id,
                turn_id,
                input,
                source_placement_ids,
                new_overlays,
            )
            .await?;
        } else {
            let depth = queue_steer(inner, input, source_placement_ids, new_overlays);
            tracing::info!(
                module = "codex",
                depth,
                "queued turn/steer behind pending inputs"
            );
            flush_pending_steers(inner).await;
        }
        return Ok(());
    }

    let claimed_start = {
        let mut in_flight = inner.turn_in_flight.lock();
        if *in_flight {
            false
        } else {
            *in_flight = true;
            true
        }
    };
    if !claimed_start {
        let depth = queue_steer(inner, input, source_placement_ids, new_overlays);
        tracing::info!(
            module = "codex",
            depth,
            "queued turn/steer until Codex reports active turn id"
        );
        return Ok(());
    }

    *inner.current_overlays.lock() = new_overlays;
    *inner.current_sources.lock() = source_placement_ids;
    inner.output_index.store(0, Ordering::SeqCst);
    // Fresh turn → start a clean assistant-block accumulator (steered inputs keep
    // appending to the in-flight turn's accumulator instead).
    inner.turn_blocks.lock().clear();

    // Every turn explicitly carries the Board's generation knobs (see
    // CODEX_PROTOCOL.md §4): turn overrides are sticky, so model/effort/serviceTier
    // are sent each turn to keep the thread matching the user's selection.
    // serviceTier is sent as explicit null when standard (clears any prior fast
    // override); summary/personality are fixed product defaults.
    let gen = inner.gen.lock().clone();
    let model = gen.model.unwrap_or_else(|| DEFAULT_GEN_MODEL.to_string());
    let effort = gen.effort.unwrap_or_else(|| DEFAULT_GEN_EFFORT.to_string());
    let service_tier = gen.service_tier; // None → JSON null → standard tier

    let res = match inner
        .call(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": input,
                "summary": "auto",
                "personality": "friendly",
                "model": model,
                "effort": effort,
                "serviceTier": service_tier,
            }),
            15_000,
        )
        .await
    {
        Ok(res) => res,
        Err(e) => {
            clear_active_turn(inner);
            clear_stream_state(inner);
            cleanup_dispatch_overlays(inner);
            return Err(e);
        }
    };
    if let Some(tid) = turn_id_of(&res) {
        *inner.current_turn_id.lock() = Some(tid);
    } else {
        tracing::warn!(module = "codex", "turn/start ok without turn id");
    }
    tracing::info!(module = "codex", refs = refs.len(), "turn/start ok");
    flush_pending_steers(inner).await;
    Ok(())
}

fn build_turn_input(prompt: &str, skills: &[SkillInputRef]) -> Value {
    let mut input = vec![json!({ "type": "text", "text": prompt, "text_elements": [] })];
    for skill in skills {
        input.push(json!({
            "type": "skill",
            "name": skill.name,
            "path": skill.path,
        }));
    }
    Value::Array(input)
}

fn build_turn_prompt(text: &str, refs: &[(String, Option<String>)]) -> String {
    if refs.is_empty() {
        return text.to_string();
    }
    let mut s = String::from(
        "Reference image(s) in the working directory — read these to see what I'm pointing at:\n",
    );
    for (clean, overlay) in refs {
        match overlay {
            Some(ov) => s.push_str(&format!(
                "- {clean}  (a marking overlay showing the region/subject to change is at: {ov})\n"
            )),
            None => s.push_str(&format!("- {clean}\n")),
        }
    }
    s.push('\n');
    s.push_str(text);
    s
}

/// Cancel the in-flight turn (keeps the session/process alive).
pub async fn interrupt_turn(codex_reg: Arc<CodexRegistry>, board_id: String) -> Result<(), String> {
    let session = codex_reg.get(&board_id).ok_or("no session")?;
    let inner = &session.inner;
    let thread_id = inner.thread_id.lock().clone();
    let turn = inner.current_turn_id.lock().clone();
    if let Some(turn_id) = turn {
        let _ = inner
            .call(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
                3_000,
            )
            .await;
    }
    Ok(())
}

pub async fn respond_permission(
    codex_reg: Arc<CodexRegistry>,
    board_id: String,
    request_id: u64,
    accept: bool,
) -> Result<(), String> {
    let session = codex_reg.get(&board_id).ok_or("no session")?;
    session
        .inner
        .respond(
            request_id,
            json!({ "decision": if accept { "accept" } else { "decline" } }),
        )
        .await;
    Ok(())
}

async fn get_auth_status(inner: &Arc<CodexSessionInner>) -> Result<CodexAuthStatus, String> {
    let res = inner
        .call(
            "getAuthStatus",
            json!({ "includeToken": false, "refreshToken": false }),
            10_000,
        )
        .await?;
    Ok(parse_auth_status(&res))
}

/// Probe ChatGPT-subscription auth. Returns (authMethod, requiresLogin).
pub async fn auth_status(codex_reg: Arc<CodexRegistry>, board_id: String) -> Result<Value, String> {
    let session = codex_reg.get(&board_id).ok_or("no session")?;
    let auth = get_auth_status(&session.inner).await?;
    Ok(json!({
        "authMethod": auth.auth_method,
        "requiresOpenaiAuth": auth.requires_openai_auth,
        "requiresLogin": auth.requires_login,
        "authSupported": auth.auth_supported,
    }))
}

/// Tear down a Board's session: interrupt, then tree-kill.
pub async fn stop_session(codex_reg: Arc<CodexRegistry>, board_id: &str) {
    let Some(session) = codex_reg.remove(board_id) else {
        return;
    };
    let inner = &session.inner;
    *inner.intentional_shutdown.lock() = true;
    let thread_id = inner.thread_id.lock().clone();
    let turn = inner.current_turn_id.lock().clone();
    if let Some(turn_id) = turn {
        let _ = inner
            .call(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
                2_000,
            )
            .await;
    }
    // unix: graceful SIGTERM over the group, wait, then SIGKILL.
    // windows: `taskkill /T /F` is forceful and immediate (`sig` is ignored).
    #[cfg(unix)]
    {
        kill_tree(session.pid, 15);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        kill_tree(session.pid, 9);
    }
    #[cfg(not(unix))]
    kill_tree(session.pid, 9);
    clear_active_turn(inner);
    clear_stream_state(inner);
    cleanup_dispatch_overlays(inner);
    let mut child = session.child.lock().await;
    let _ = child.start_kill();
}

/// Tree-kill the sidecar and everything it spawned. The Codex app-server forks
/// model/tool children, so killing only the direct PID would orphan them.
#[cfg(unix)]
fn kill_tree(pid: u32, sig: i32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    if pid == 0 {
        return;
    }
    if let Ok(signal) = Signal::try_from(sig) {
        // Negative pid = the whole process group (we spawn with process_group(0)).
        let _ = kill(Pid::from_raw(-(pid as i32)), signal);
    }
}

/// Windows has no POSIX signals/process groups; `taskkill /T` walks the child
/// tree by PID and `/F` forces termination. `sig` is ignored.
#[cfg(windows)]
fn kill_tree(pid: u32, _sig: i32) {
    if pid == 0 {
        return;
    }
    let mut cmd = std::process::Command::new("taskkill");
    crate::process::hide_console_window(&mut cmd);
    let _ = cmd
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

// ── Background loops ─────────────────────────────────────────────────────────

async fn reader_loop(inner: Arc<CodexSessionInner>, stdout: ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let has_id = msg.get("id").is_some();
                let has_method = msg.get("method").is_some();

                if has_id && !has_method {
                    // Response to our request.
                    if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
                        if let Some(tx) = inner.pending.lock().remove(&id) {
                            let result = if let Some(err) = msg.get("error") {
                                Err(format!("rpc error: {err}"))
                            } else {
                                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                            };
                            let _ = tx.send(result);
                        }
                    }
                } else if has_method && !has_id {
                    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    handle_notification(&inner, method, params).await;
                } else if has_method && has_id {
                    let id = msg.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let method = msg
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    handle_server_request(&inner, id, &method).await;
                }
            }
            _ => break, // EOF or error
        }
    }
    // Process exited: reject pending, signal completion.
    let drained: Vec<_> = inner.pending.lock().drain().collect();
    for (_, tx) in drained {
        let _ = tx.send(Err("app-server process exited".into()));
    }
    clear_active_turn(&inner);
    // Process died mid-turn (crash / wedge / kill): flush whatever the assistant
    // streamed so far so it isn't lost. No-op if turn/completed already flushed.
    flush_turn_timeline(&inner, "error", Some("Codex process exited"));
    clear_stream_state(&inner);
    cleanup_dispatch_overlays(&inner);
    if !*inner.intentional_shutdown.lock() {
        inner.emit(UnifiedEvent::SessionComplete {
            ok: false,
            message: "Codex process exited".into(),
        });
    }
}

/// Codex stderr lines whose lowercased text contains one of these substrings are
/// surfaced to the UI as a chat note so failures aren't silent (first match
/// wins). The full line is always written to the unified log regardless.
const STDERR_SURFACE: &[(&str, &str, &str)] = &[
    ("401", "warn", "认证失败 (401) — 确认已 codex login"),
    ("403", "warn", "无权限 (403)"),
    ("unauthorized", "warn", "认证失败 — 确认已 codex login"),
    (
        "error sending request",
        "error",
        "网络请求失败 — 检查网络/代理",
    ),
    ("connection refused", "error", "连接被拒绝 — 检查网络/代理"),
    ("connection reset", "error", "连接被重置 — 检查网络/代理"),
    ("timed out", "error", "请求超时 — 检查网络/代理"),
    ("dns error", "error", "DNS 解析失败 — 检查网络/代理"),
    ("rate limit", "warn", "触发限流 (rate limit)"),
    ("transport error", "error", "传输层错误 — 检查网络/代理"),
];

async fn stderr_drain(inner: Arc<CodexSessionInner>, stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let text = strip_ansi(&line);
        if text.trim().is_empty() {
            continue;
        }
        // Always captured in the unified log at info so it's visible by default.
        tracing::info!(module = "codex-stderr", "{text}");
        let low = text.to_lowercase();
        if let Some((_, level, prefix)) =
            STDERR_SURFACE.iter().find(|(sub, _, _)| low.contains(sub))
        {
            let detail: String = text.chars().take(200).collect();
            emit_runtime_log(&inner, level, format!("{prefix}: {detail}"));
        }
    }
}

fn strip_ansi(s: &str) -> String {
    // Minimal CSI stripper (avoids a regex per line).
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in s.chars().enumerate() {
        if idx >= max {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn compact_json(value: &Value, max_chars: usize) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".into());
    truncate_chars(&raw, max_chars)
}

fn string_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = value;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn codex_notice_message(params: &Value) -> Option<String> {
    for path in [
        &["message"][..],
        &["error", "message"][..],
        &["error"][..],
        &["detail"][..],
        &["details"][..],
        &["data", "message"][..],
        &["reason"][..],
        &["text"][..],
    ] {
        if let Some(msg) = string_at_path(params, path) {
            return Some(msg.to_string());
        }
    }

    let fallback = if let Some(error) = params.get("error") {
        error
    } else {
        params
    };
    match fallback {
        Value::Null => None,
        Value::Object(map) if map.is_empty() => None,
        Value::Array(arr) if arr.is_empty() => None,
        _ => Some(compact_json(fallback, 500)),
    }
}

fn parse_u64_prefix(s: &str) -> Option<u64> {
    let digits: String = s
        .trim()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn transport_status_from_log(message: &str) -> Option<(&'static str, Option<u64>, Option<u64>)> {
    let lower = message.to_ascii_lowercase();
    const RECONNECTING: &str = "reconnecting...";
    if lower.starts_with(RECONNECTING) {
        let tail = message.get(RECONNECTING.len()..).unwrap_or("").trim();
        let mut parts = tail.split('/');
        let attempt = parts.next().and_then(parse_u64_prefix);
        let max = parts.next().and_then(parse_u64_prefix);
        return Some(("reconnecting", attempt, max));
    }
    if lower.starts_with("falling back from websockets to https transport.") {
        return Some(("fallback", None, None));
    }
    None
}

fn emit_runtime_log(inner: &Arc<CodexSessionInner>, level: &str, message: String) {
    match level {
        "error" => tracing::error!(module = "codex", "{message}"),
        "warn" => tracing::warn!(module = "codex", "{message}"),
        _ => tracing::info!(module = "codex", "{message}"),
    }
    if let Some((phase, attempt, max)) = transport_status_from_log(&message) {
        inner.emit(UnifiedEvent::TransportStatus {
            phase: phase.to_string(),
            attempt,
            max,
            message: message.clone(),
        });
    }
    inner.emit(UnifiedEvent::Log {
        level: level.to_string(),
        message,
    });
}

// ── Notification + server-request handling ───────────────────────────────────

async fn handle_notification(inner: &Arc<CodexSessionInner>, method: &str, params: Value) {
    match method {
        "item/agentMessage/delta" => {
            let delta = params.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            let item_id = params
                .get("itemId")
                .and_then(|v| v.as_str())
                .unwrap_or("_")
                .to_string();
            if !delta.is_empty() {
                inner
                    .agent_accum
                    .lock()
                    .entry(item_id)
                    .or_default()
                    .push_str(delta);
                inner.emit(UnifiedEvent::TextDelta {
                    text: delta.to_string(),
                });
            }
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" | "item/plan/delta" => {
            let delta = params.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if !delta.is_empty() {
                inner.emit(UnifiedEvent::ThinkingDelta {
                    text: delta.to_string(),
                });
            }
        }
        "item/started" => {
            let item = params.get("item").cloned().unwrap_or(Value::Null);
            let typ = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match typ {
                "reasoning" | "plan" => inner.emit(UnifiedEvent::ThinkingStart),
                // Non-tool items that don't render as their own chat block.
                "agentMessage" | "userMessage" | "contextCompaction" | "hookPrompt"
                | "enteredReviewMode" | "exitedReviewMode" => {}
                "imageGeneration" => {
                    inner.emit(UnifiedEvent::ToolStart {
                        tool_use_id: id.clone(),
                        tool_name: "ImageGeneration".into(),
                        detail: None,
                    });
                    start_generation(inner, &id);
                }
                // Everything else is a tool call — incl. mcpToolCall / dynamicToolCall /
                // collabAgentToolCall and any future Codex tool type (forward-compat).
                _ => inner.emit(UnifiedEvent::ToolStart {
                    tool_use_id: id,
                    tool_name: tool_label(typ, &item),
                    detail: tool_detail(typ, &item),
                }),
            }
        }
        "item/completed" => {
            let item = params.get("item").cloned().unwrap_or(Value::Null);
            handle_item_completed(inner, &item).await;
        }
        "turn/started" => {
            *inner.turn_in_flight.lock() = true;
            if let Some(tid) = params
                .get("turn")
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str())
            {
                *inner.current_turn_id.lock() = Some(tid.to_string());
            }
            let flush_inner = inner.clone();
            tauri::async_runtime::spawn(async move {
                flush_pending_steers(&flush_inner).await;
            });
            inner.emit(UnifiedEvent::Status {
                state: "running".into(),
            });
        }
        "turn/completed" => {
            let turn = params.get("turn").cloned().unwrap_or(Value::Null);
            let status = turn
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("completed")
                .to_string();
            // Codex usually puts the reason in `turn.error.message`, but tolerate
            // a string-form `error` too. Never surface a blank message: on a
            // non-completed status with no reason, synthesize one from the status.
            let raw_error = turn.get("error").and_then(|e| {
                e.get("message")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| e.as_str().map(String::from))
            });
            let error = match (status.as_str(), raw_error) {
                ("completed", _) => None,
                (_, Some(m)) => Some(m),
                (s, None) => Some(format!("turn ended without success (status: {s})")),
            };
            if status == "completed" {
                tracing::info!(module = "codex", "turn completed");
            } else {
                tracing::warn!(module = "codex", status = %status, error = ?error, "turn did not complete");
            }
            clear_active_turn(inner);
            // Clear per-turn state so a stray late item can't leak / mis-place
            // against stale sources (review #3, #6).
            clear_stream_state(inner);
            cleanup_dispatch_overlays(inner);
            // Persist the assistant message BEFORE signalling completion, so a UI
            // that reacts to TurnComplete (e.g. switches session → reloads) always
            // reads a timeline that already includes this turn.
            flush_turn_timeline(inner, &status, error.as_deref());
            inner.emit(UnifiedEvent::TurnComplete { status, error });
        }
        "thread/tokenUsage/updated" => {
            let total = params.get("tokenUsage").and_then(|u| u.get("total"));
            if let Some(t) = total {
                inner.emit(UnifiedEvent::Usage {
                    input_tokens: t.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    output_tokens: t.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0),
                });
            }
        }
        "thread/status/changed" => {
            if let Some(s) = params
                .get("status")
                .and_then(|s| s.get("type"))
                .and_then(|v| v.as_str())
            {
                inner.emit(UnifiedEvent::Status {
                    state: s.to_string(),
                });
            }
        }
        "turn/plan/updated" => {
            let explanation = params
                .get("explanation")
                .and_then(|v| v.as_str())
                .map(String::from);
            let steps = params
                .get("plan")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| {
                            Some(PlanStep {
                                step: s.get("step")?.as_str()?.to_string(),
                                status: s
                                    .get("status")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("pending")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            inner.emit(UnifiedEvent::PlanUpdated { explanation, steps });
        }
        "account/rateLimits/updated" => {
            // Codex emits two windows: `primary` = 5-hour rolling, `secondary`
            // = weekly. Forward both — UI shows them as separate rows in the
            // Codex info popover so the user knows which one they're close to.
            let rl = params.get("rateLimits");
            let primary = rl.and_then(|r| r.get("primary"));
            let secondary = rl.and_then(|r| r.get("secondary"));
            // `usedPercent` on at least the primary is the gating field — if
            // it's missing we skip the whole event (Codex hasn't said anything
            // meaningful yet).
            if let Some(primary_used) = primary
                .and_then(|p| p.get("usedPercent"))
                .and_then(|v| v.as_f64())
            {
                inner.emit(UnifiedEvent::RateLimits {
                    used_percent: primary_used,
                    resets_at: primary
                        .and_then(|p| p.get("resetsAt"))
                        .and_then(|v| v.as_f64()),
                    secondary_used_percent: secondary
                        .and_then(|p| p.get("usedPercent"))
                        .and_then(|v| v.as_f64()),
                    secondary_resets_at: secondary
                        .and_then(|p| p.get("resetsAt"))
                        .and_then(|v| v.as_f64()),
                    reached: rl
                        .and_then(|r| r.get("rateLimitReachedType"))
                        .and_then(|v| v.as_str())
                        .map(String::from),
                });
            }
        }
        "error" => {
            // Codex app-server `error` notifications are not a terminal turn
            // contract: transport recovery can emit them while the same turn later
            // streams output and finishes with `turn/completed`. Keep the turn
            // alive; terminal state is driven by turn/completed/session EOF/RPC
            // failure only.
            let raw = compact_json(&params, 1200);
            let active_turn = *inner.turn_in_flight.lock();
            if let Some(msg) = codex_notice_message(&params) {
                tracing::warn!(
                    module = "codex",
                    active_turn,
                    params = %raw,
                    "codex error notification kept non-terminal: {msg}"
                );
                emit_runtime_log(inner, "warn", msg);
            } else {
                tracing::warn!(
                    module = "codex",
                    active_turn,
                    params = %raw,
                    "codex error notification without message kept non-terminal"
                );
            }
        }
        "warning" | "guardianWarning" | "configWarning" | "deprecationNotice" => {
            let msg = params
                .get("message")
                .or_else(|| params.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            tracing::warn!(module = "codex", "{method}: {msg}");
            if !msg.is_empty() {
                emit_runtime_log(inner, "warn", msg.to_string());
            }
        }
        _ => { /* forward-compat: ignore unknown notifications */ }
    }
}

async fn handle_item_completed(inner: &Arc<CodexSessionInner>, item: &Value) {
    let typ = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match typ {
        "imageGeneration" => {
            inner.emit(UnifiedEvent::ToolStop { tool_use_id: id });
            on_image_generation(inner, item).await;
        }
        "agentMessage" => {
            let final_text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let streamed = inner.agent_accum.lock().remove(&id).unwrap_or_default();
            if !final_text.is_empty() {
                if streamed.is_empty() {
                    inner.emit(UnifiedEvent::TextDelta {
                        text: final_text.to_string(),
                    });
                } else if final_text.starts_with(&streamed) && final_text.len() > streamed.len() {
                    inner.emit(UnifiedEvent::TextDelta {
                        text: final_text[streamed.len()..].to_string(),
                    });
                }
            }
            inner.emit(UnifiedEvent::TextStop);
            // Accumulate the completed text for authoritative timeline persistence
            // (the full message; prefer the item's final text, fall back to the
            // streamed deltas). Pushed in arrival order alongside image blocks.
            let full = if final_text.is_empty() { streamed } else { final_text.to_string() };
            if !full.is_empty() {
                inner
                    .turn_blocks
                    .lock()
                    .push(json!({ "type": "text", "text": full }));
            }
        }
        "reasoning" | "plan" => inner.emit(UnifiedEvent::ThinkingStop),
        // Non-tool items: no chat block (mirror item/started).
        "userMessage" | "contextCompaction" | "hookPrompt" | "enteredReviewMode"
        | "exitedReviewMode" => {}
        // Every other type is a tool — incl. mcp/dynamic/collab + future types.
        _ => {
            inner.emit(UnifiedEvent::ToolStop {
                tool_use_id: id.clone(),
            });
            let content = item
                .get("aggregatedOutput")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    item.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("")
                .to_string();
            inner.emit(UnifiedEvent::ToolResult {
                tool_use_id: id,
                content,
            });
        }
    }
}

/// Friendly chat label for a tool item. Unknown (future) types fall back to the
/// raw type name so they still show up.
fn tool_label(typ: &str, item: &Value) -> String {
    let s = |k: &str| item.get(k).and_then(|v| v.as_str());
    match typ {
        "commandExecution" => "Bash".into(),
        "fileChange" => "Edit".into(),
        "webSearch" => "WebSearch".into(),
        "imageView" => "Read".into(),
        "imageGeneration" => "ImageGeneration".into(),
        "mcpToolCall" => match (s("server"), s("tool")) {
            (Some(srv), Some(t)) => format!("{srv}·{t}"),
            (_, Some(t)) => t.into(),
            _ => "MCP tool".into(),
        },
        "dynamicToolCall" => s("tool").unwrap_or("Tool").into(),
        "collabAgentToolCall" => s("tool")
            .map(|t| format!("Agent·{t}"))
            .unwrap_or_else(|| "Agent".into()),
        other => other.into(),
    }
}

/// First-level gray subtitle for a tool row — the command / file / query, pulled
/// from the `item/started` payload (already present per the Codex schema).
/// Truncated; None when there's nothing useful to show.
fn tool_detail(typ: &str, item: &Value) -> Option<String> {
    let raw = match typ {
        "commandExecution" => item
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from),
        "fileChange" => item
            .get("changes")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("path"))
            .and_then(|v| v.as_str())
            .map(String::from),
        "webSearch" => item.get("query").and_then(|v| v.as_str()).map(String::from),
        "imageView" => item.get("path").and_then(|v| v.as_str()).map(String::from),
        _ => None,
    }?;
    let t = raw.trim().replace('\n', " ");
    if t.is_empty() {
        None
    } else {
        Some(t.chars().take(120).collect())
    }
}

/// The core Cameo handler: take a generated image (savedPath or base64),
/// import it into the Board folder, mint an Asset, place it right-of-source,
/// persist, and emit `ImageGenerated`.
/// Claim a layout slot + loading placeholder when a generation starts, so the
/// final image lands where the placeholder showed.
fn start_generation(inner: &Arc<CodexSessionInner>, item_id: &str) {
    let index = inner.output_index.fetch_add(1, Ordering::SeqCst) as i64;
    let placeholder_id = nanoid::nanoid!();
    let rect = {
        let Some(entry) = inner.registry.get(&inner.board_id) else {
            return;
        };
        let doc = entry.doc.lock();
        let sources = inner.current_sources.lock().clone();
        let source_pair = sources.first().and_then(|sid| {
            let p = doc.placements.iter().find(|p| &p.id == sid)?.clone();
            let a = doc.assets.iter().find(|a| a.id == p.asset_id)?.clone();
            Some((p, a))
        });
        board::placeholder_rect(source_pair.as_ref().map(|(p, a)| (p, a)), index, &doc)
    };
    inner
        .pending_gen
        .lock()
        .insert(item_id.to_string(), (placeholder_id.clone(), index));
    inner.emit(UnifiedEvent::GenerationStarted {
        placeholder_id,
        x: rect.0,
        y: rect.1,
        w: rect.2,
        h: rect.3,
    });
}

async fn on_image_generation(inner: &Arc<CodexSessionInner>, item: &Value) {
    let caption = item
        .get("revisedPrompt")
        .and_then(|v| v.as_str())
        .map(String::from);
    let saved_path = item.get("savedPath").and_then(|v| v.as_str());
    let result_b64 = item.get("result").and_then(|v| v.as_str());
    let item_id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Pair with the placeholder claimed at item/started (reuse its slot index).
    let (placeholder_id, out_index) = match inner.pending_gen.lock().remove(&item_id) {
        Some((pid, idx)) => (Some(pid), idx),
        None => (
            None,
            inner.output_index.fetch_add(1, Ordering::SeqCst) as i64,
        ),
    };

    // Snapshot the asset list for content-dedup without holding the lock during IO.
    let assets_snapshot = match inner.registry.get(&inner.board_id) {
        Some(entry) => entry.doc.lock().assets.clone(),
        None => return,
    };

    // Heavy file IO (decode / copy / write) happens OUTSIDE the doc lock so it
    // never freezes the canvas or blocks other commands (review #2).
    let asset = if let Some(sp) = saved_path.filter(|s| !s.is_empty()) {
        match assets::import_generated_file(&inner.folder, Path::new(sp), &assets_snapshot) {
            Ok(a) => a,
            Err(e) => {
                emit_runtime_log(inner, "error", format!("save generated image: {e}"));
                return;
            }
        }
    } else if let Some(b64) = result_b64.filter(|s| !s.is_empty()) {
        match base64::engine::general_purpose::STANDARD.decode(b64) {
            Ok(bytes) => match assets::import_bytes(
                &inner.folder,
                &bytes,
                "png",
                "gen",
                crate::model::Origin::Generated,
                &assets_snapshot,
            ) {
                Ok(a) => a,
                Err(e) => {
                    emit_runtime_log(inner, "error", format!("write generated image: {e}"));
                    return;
                }
            },
            Err(e) => {
                emit_runtime_log(inner, "error", format!("decode generated image: {e}"));
                return;
            }
        }
    } else {
        return; // nothing to place
    };

    // Serialize mutation snapshots with disk saves so board.json cannot be rolled
    // back by an older clone, while keeping the doc lock itself IO-free.
    let Some(entry) = inner.registry.get(&inner.board_id) else {
        return;
    };
    let save_guard = entry.save.lock();
    let (placement, doc_clone) = {
        let mut doc = entry.doc.lock();
        let sources = inner.current_sources.lock().clone();
        let source_pair = sources.first().and_then(|sid| {
            let p = doc.placements.iter().find(|p| &p.id == sid)?.clone();
            let a = doc.assets.iter().find(|a| a.id == p.asset_id)?.clone();
            Some((p, a))
        });
        let placement = board::make_derived_placement(
            &asset,
            source_pair.as_ref().map(|(p, a)| (p, a)),
            out_index,
            &doc,
        );
        if !doc.assets.iter().any(|a| a.id == asset.id) {
            doc.assets.push(asset.clone());
        }
        doc.placements.push(placement.clone());
        (placement, doc.clone())
    };

    if let Err(e) = storage::save_board_doc(&inner.folder, &doc_clone) {
        tracing::warn!(module = "codex", "save board after generation failed: {e}");
    }
    drop(save_guard);
    // Accumulate the generated image as an assistant image block for the turn's
    // authoritative timeline (matches the frontend's "done" image block shape).
    inner.turn_blocks.lock().push(json!({
        "type": "image",
        "placementId": placement.id,
        "caption": caption,
        "status": "done",
    }));
    inner.emit(UnifiedEvent::ImageGenerated {
        asset,
        placement,
        caption,
        placeholder_id,
    });
}

async fn handle_server_request(inner: &Arc<CodexSessionInner>, id: u64, method: &str) {
    // With approvalPolicy=never these are rare. Surface it, then auto-accept so
    // the turn never hangs (the workspace-write sandbox bounds the risk).
    inner.emit(UnifiedEvent::PermissionRequest {
        request_id: id,
        summary: method.to_string(),
    });
    inner.respond(id, json!({ "decision": "accept" })).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn push_unique_preserves_first_path() {
        let mut parts = Vec::new();
        push_unique(&mut parts, PathBuf::from("/a"));
        push_unique(&mut parts, PathBuf::from("/b"));
        push_unique(&mut parts, PathBuf::from("/a"));
        assert_eq!(parts, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn codex_error_notice_extracts_nested_messages() {
        let params = json!({
            "error": {
                "message": "stream disconnected before completion",
                "code": "transport"
            }
        });

        assert_eq!(
            codex_notice_message(&params).as_deref(),
            Some("stream disconnected before completion")
        );
    }

    #[test]
    fn codex_error_notice_does_not_fabricate_unknown_error() {
        assert_eq!(codex_notice_message(&json!({})), None);
        assert_eq!(codex_notice_message(&Value::Null), None);
    }

    #[test]
    fn auth_status_marks_missing_chatgpt_auth_as_login_required() {
        let auth = parse_auth_status(&json!({
            "authMethod": null,
            "authToken": null,
            "requiresOpenaiAuth": true,
        }));

        assert_eq!(auth.auth_method, None);
        assert!(auth.requires_openai_auth);
        assert!(auth.requires_login);
        assert!(!auth.auth_supported);
    }

    #[test]
    fn auth_status_accepts_chatgpt_login() {
        let auth = parse_auth_status(&json!({
            "authMethod": "chatgpt",
            "authToken": null,
            "requiresOpenaiAuth": true,
        }));

        assert_eq!(auth.auth_method.as_deref(), Some("chatgpt"));
        assert!(auth.requires_openai_auth);
        assert!(!auth.requires_login);
        assert!(auth.auth_supported);
    }

    #[test]
    fn auth_status_accepts_api_key_for_codex_runtime() {
        let auth = parse_auth_status(&json!({
            "authMethod": "apikey",
            "authToken": null,
            "requiresOpenaiAuth": true,
        }));

        assert_eq!(auth.auth_method.as_deref(), Some("apikey"));
        assert!(!auth.requires_login);
        assert!(auth.auth_supported);
    }

    #[test]
    fn rpc_response_filter_ignores_same_id_server_requests() {
        assert!(is_rpc_response_for(
            &json!({ "jsonrpc": "2.0", "id": 7, "result": {} }),
            7,
        ));
        assert!(!is_rpc_response_for(
            &json!({ "jsonrpc": "2.0", "id": 7, "method": "tool/request", "params": {} }),
            7,
        ));
        assert!(!is_rpc_response_for(
            &json!({ "jsonrpc": "2.0", "id": 8, "result": {} }),
            7,
        ));
    }

    #[test]
    fn turn_steer_params_include_expected_turn_precondition() {
        let params = turn_steer_params(
            "thread-1",
            "turn-1",
            json!([{ "type": "text", "text": "hi" }]),
        );

        assert_eq!(
            params.get("threadId").and_then(Value::as_str),
            Some("thread-1")
        );
        assert_eq!(
            params.get("expectedTurnId").and_then(Value::as_str),
            Some("turn-1")
        );
        assert_eq!(
            params
                .get("input")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str),
            Some("hi")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn extract_marked_path_ignores_noisy_shell_output() {
        let marker = "__CAMEO_PATH_TEST__";
        let stdout = format!("banner\n{marker}/nvm/bin:/usr/local/bin:/usr/bin{marker}\n");
        assert_eq!(
            extract_marked_path(&stdout, marker).as_deref(),
            Some("/nvm/bin:/usr/local/bin:/usr/bin")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn nvm_paths_are_added_newest_version_first() {
        let tmp = tempfile::tempdir().unwrap();
        for version in ["v9.0.0", "v20.10.0", "v18.19.1"] {
            std::fs::create_dir_all(
                tmp.path()
                    .join(".nvm")
                    .join("versions")
                    .join("node")
                    .join(version),
            )
            .unwrap();
        }

        let mut parts = Vec::new();
        push_nvm_paths(&mut parts, tmp.path());

        assert_eq!(
            parts,
            vec![
                tmp.path()
                    .join(".nvm")
                    .join("versions")
                    .join("node")
                    .join("v20.10.0")
                    .join("bin"),
                tmp.path()
                    .join(".nvm")
                    .join("versions")
                    .join("node")
                    .join("v18.19.1")
                    .join("bin"),
                tmp.path()
                    .join(".nvm")
                    .join("versions")
                    .join("node")
                    .join("v9.0.0")
                    .join("bin"),
            ]
        );
    }
}
