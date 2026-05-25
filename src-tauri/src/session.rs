//! Per-Board session index + message timelines (v0.0.2 multi-session).
//!
//! A Board has N sessions (conversations); the canvas (board.json) is shared.
//! This module is pure file I/O — it owns `.cameo/sessions.json` (the index,
//! incl. each session's Codex threadId) and `.cameo/sessions/<id>.jsonl`
//! (opaque message timelines; the frontend owns the message/block shape and
//! just hands JSON in/out).

use crate::paths::{board_session_timeline, board_sessions_doc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub id: String,
    pub thread_id: Option<String>,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsDoc {
    pub active_session_id: Option<String>,
    pub sessions: Vec<SessionMeta>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn load(folder: &Path) -> SessionsDoc {
    match std::fs::read(board_sessions_doc(folder)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => SessionsDoc::default(),
    }
}

pub fn save(folder: &Path, doc: &SessionsDoc) {
    if let Err(e) = write_json_atomic(board_sessions_doc(folder), doc) {
        tracing::warn!(module = "session", "save sessions.json failed: {e}");
    }
}

fn write_json_atomic(path: PathBuf, doc: &SessionsDoc) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sessions.json");
    let tmp = path.with_file_name(format!("{base}.{}.tmp", nanoid::nanoid!(8)));
    let json = serde_json::to_vec_pretty(doc)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn make_session(thread_id: Option<String>, title: &str) -> SessionMeta {
    let t = now_ms();
    SessionMeta {
        id: nanoid::nanoid!(12),
        thread_id,
        title: title.to_string(),
        created_at: t,
        updated_at: t,
    }
}

/// Ensure at least one session exists. Migrates a legacy single-session
/// `meta.threadId` into session #1. Returns the (possibly created) doc.
pub fn ensure_initial(folder: &Path, legacy_thread: Option<String>) -> SessionsDoc {
    let mut doc = load(folder);
    if doc.sessions.is_empty() {
        let s = make_session(legacy_thread, "New session");
        doc.active_session_id = Some(s.id.clone());
        doc.sessions.push(s);
        save(folder, &doc);
    } else if doc.active_session_id.is_none() {
        doc.active_session_id = doc.sessions.first().map(|s| s.id.clone());
        save(folder, &doc);
    }
    doc
}

/// Create a new session and make it active. Returns it.
pub fn new_session(folder: &Path) -> SessionMeta {
    let mut doc = load(folder);
    let s = make_session(None, "New session");
    doc.active_session_id = Some(s.id.clone());
    doc.sessions.push(s.clone());
    save(folder, &doc);
    s
}

pub fn set_active(folder: &Path, id: &str) {
    let mut doc = load(folder);
    if doc.sessions.iter().any(|s| s.id == id) {
        doc.active_session_id = Some(id.to_string());
        save(folder, &doc);
    }
}

pub fn set_thread(folder: &Path, id: &str, thread_id: &str) {
    let mut doc = load(folder);
    if let Some(s) = doc.sessions.iter_mut().find(|s| s.id == id) {
        s.thread_id = Some(thread_id.to_string());
        s.updated_at = now_ms();
        save(folder, &doc);
    }
}

pub fn rename(folder: &Path, id: &str, title: &str) {
    let mut doc = load(folder);
    if let Some(s) = doc.sessions.iter_mut().find(|s| s.id == id) {
        s.title = title.to_string();
        s.updated_at = now_ms();
        save(folder, &doc);
    }
}

pub fn thread_of(folder: &Path, id: &str) -> Option<String> {
    load(folder)
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .and_then(|s| s.thread_id)
}

/// Append one message (opaque JSON) to a session's timeline + bump updatedAt.
pub fn append_message(folder: &Path, id: &str, msg: &Value) {
    let line = match serde_json::to_string(msg) {
        Ok(l) => l,
        Err(_) => return,
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(board_session_timeline(folder, id))
    {
        let _ = writeln!(f, "{line}");
    }
    let mut doc = load(folder);
    if let Some(s) = doc.sessions.iter_mut().find(|s| s.id == id) {
        s.updated_at = now_ms();
        save(folder, &doc);
    }
}

/// Read a session's timeline as opaque JSON messages.
pub fn load_timeline(folder: &Path, id: &str) -> Vec<Value> {
    match std::fs::read_to_string(board_session_timeline(folder, id)) {
        Ok(text) => text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}
