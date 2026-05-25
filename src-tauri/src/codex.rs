//! Codex runtime — drives `codex app-server` as a persistent JSON-RPC 2.0 /
//! stdio sidecar (one process per Board). Ported from a sibling TS
//! implementation (`codex.ts`, TS→Rust). The adapter translates Codex
//! notifications into `UnifiedEvent`s.
//!
//! Flow: spawn → initialize (+initialized) → thread/start | thread/resume →
//! turn/start (per message) / turn/interrupt (cancel) → stream items.
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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::oneshot;
use tokio::sync::Mutex as TokioMutex;

// ── PATH resolution (GUI apps get a minimal PATH; augment + `which`) ─────────

fn augmented_path() -> String {
    // GUI-launched apps inherit a stripped PATH (notably macOS .app bundles), so
    // prepend the well-known toolchain bin dirs where `codex` tends to live, then
    // fall back to the inherited PATH. Joined with the platform separator
    // (`;` on Windows, `:` elsewhere) via `std::env::join_paths` — never hardcode.
    let mut parts: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for d in [".local/bin", ".cargo/bin", ".bun/bin", ".deno/bin"] {
            parts.push(home.join(d));
        }
        // Windows: npm global (`npm i -g @openai/codex`) and scoop shims sit
        // under the user profile. `which` resolves the `.cmd`/`.exe` via PATHEXT.
        #[cfg(windows)]
        for d in ["AppData/Roaming/npm", "scoop/shims"] {
            parts.push(home.join(d));
        }
    }
    #[cfg(not(windows))]
    for d in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        parts.push(PathBuf::from(d));
    }
    if let Some(existing) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(&parts)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| std::env::var("PATH").unwrap_or_default())
}

fn resolve_codex() -> Result<PathBuf, String> {
    let path = augmented_path();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    which::which_in("codex", Some(&path), cwd).map_err(|e| format!("codex not found in PATH: {e}"))
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

pub fn detect() -> CodexInfo {
    match resolve_codex() {
        Ok(p) => {
            let version = std::process::Command::new(&p)
                .arg("--version")
                .env("PATH", augmented_path())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty());
            CodexInfo {
                found: true,
                path: Some(p.to_string_lossy().to_string()),
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

// ── Per-Board session state ──────────────────────────────────────────────────

type Pending = PlMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

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
    current_turn_id: PlMutex<Option<String>>,
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

fn cleanup_dispatch_overlays(inner: &Arc<CodexSessionInner>) {
    let overlays = {
        let mut current = inner.current_overlays.lock();
        std::mem::take(&mut *current)
    };
    for rel in overlays {
        if !is_overlay_temp_path(&rel) {
            tracing::warn!(module = "codex", overlay = %rel, "skip unsafe overlay cleanup path");
            continue;
        }
        if let Err(e) = std::fs::remove_file(inner.folder.join(&rel)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(module = "codex", overlay = %rel, "overlay cleanup failed: {e}");
            }
        }
    }
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

    let mut cmd = tokio::process::Command::new(&codex);
    cmd.arg("app-server")
        .current_dir(&folder)
        .env("PATH", augmented_path())
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
        current_turn_id: PlMutex::new(None),
        current_sources: PlMutex::new(Vec::new()),
        current_overlays: PlMutex::new(Vec::new()),
        output_index: AtomicU64::new(0),
        agent_accum: PlMutex::new(HashMap::new()),
        pending_gen: PlMutex::new(HashMap::new()),
        intentional_shutdown: PlMutex::new(false),
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

fn thread_id_of(res: &Value) -> Option<String> {
    res.get("thread")?.get("id")?.as_str().map(String::from)
}

/// Send a user message: resolve referenced placements → file paths in the
/// prompt (D4), record sources for output placement, fire `turn/start`.
pub async fn send_message(
    codex_reg: Arc<CodexRegistry>,
    board_id: String,
    text: String,
    source_placement_ids: Vec<String>,
    overlays: Vec<(String, String)>,
) -> Result<(), String> {
    let session = codex_reg.get(&board_id).ok_or("session not started")?;
    let inner = &session.inner;
    let overlay_map: HashMap<String, String> = overlays.into_iter().collect();
    *inner.current_overlays.lock() = overlay_map.values().cloned().collect();

    let thread_id = inner.thread_id.lock().clone();
    if thread_id.is_empty() {
        cleanup_dispatch_overlays(inner);
        return Err("session has no thread yet".into());
    }

    // Resolve placement ids → (clean asset path, optional marking-overlay path).
    let refs: Vec<(String, Option<String>)> = {
        let Some(entry) = inner.registry.get(&board_id) else {
            cleanup_dispatch_overlays(inner);
            return Err("unknown board".into());
        };
        let doc = entry.doc.lock();
        source_placement_ids
            .iter()
            .filter_map(|pid| {
                let p = doc.placements.iter().find(|p| &p.id == pid)?;
                let a = doc.assets.iter().find(|a| a.id == p.asset_id)?;
                Some((a.path.clone(), overlay_map.get(pid).cloned()))
            })
            .collect()
    };

    *inner.current_sources.lock() = source_placement_ids;
    inner.output_index.store(0, Ordering::SeqCst);

    let prompt = build_turn_prompt(&text, &refs);
    let input = json!([{ "type": "text", "text": prompt, "text_elements": [] }]);
    let res = match inner
        .call(
            "turn/start",
            json!({ "threadId": thread_id, "input": input, "summary": "concise" }),
            15_000,
        )
        .await
    {
        Ok(res) => res,
        Err(e) => {
            cleanup_dispatch_overlays(inner);
            return Err(e);
        }
    };
    if let Some(tid) = res
        .get("turn")
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
    {
        *inner.current_turn_id.lock() = Some(tid.to_string());
    }
    tracing::info!(module = "codex", refs = refs.len(), "turn/start ok");
    Ok(())
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

/// Probe ChatGPT-subscription auth. Returns (authMethod, requiresLogin).
pub async fn auth_status(codex_reg: Arc<CodexRegistry>, board_id: String) -> Result<Value, String> {
    let session = codex_reg.get(&board_id).ok_or("no session")?;
    let res = session
        .inner
        .call("getAuthStatus", json!({}), 10_000)
        .await?;
    let auth_method = res.get("authMethod").and_then(|v| v.as_str());
    let requires_openai = res
        .get("requiresOpenaiAuth")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(json!({
        "authMethod": auth_method,
        "requiresLogin": auth_method.is_none() && requires_openai,
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
    let _ = std::process::Command::new("taskkill")
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
            inner.emit(UnifiedEvent::Log {
                level: (*level).into(),
                message: format!("{prefix}: {detail}"),
            });
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
            if let Some(tid) = params
                .get("turn")
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str())
            {
                *inner.current_turn_id.lock() = Some(tid.to_string());
            }
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
            *inner.current_turn_id.lock() = None;
            // Clear per-turn state so a stray late item can't leak / mis-place
            // against stale sources (review #3, #6).
            inner.agent_accum.lock().clear();
            inner.current_sources.lock().clear();
            inner.pending_gen.lock().clear();
            cleanup_dispatch_overlays(inner);
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
            let msg = params
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            tracing::error!(module = "codex", "error notification: {msg}");
            cleanup_dispatch_overlays(inner);
            inner.emit(UnifiedEvent::Error {
                message: msg.to_string(),
            });
        }
        "warning" | "guardianWarning" | "configWarning" | "deprecationNotice" => {
            let msg = params
                .get("message")
                .or_else(|| params.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            tracing::warn!(module = "codex", "{method}: {msg}");
            if !msg.is_empty() {
                inner.emit(UnifiedEvent::Log {
                    level: "warn".into(),
                    message: msg.to_string(),
                });
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
                inner.emit(UnifiedEvent::Error {
                    message: format!("save generated image: {e}"),
                });
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
                    inner.emit(UnifiedEvent::Error {
                        message: format!("write generated image: {e}"),
                    });
                    return;
                }
            },
            Err(e) => {
                inner.emit(UnifiedEvent::Error {
                    message: format!("decode generated image: {e}"),
                });
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
