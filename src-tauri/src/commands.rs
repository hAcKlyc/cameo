//! Tauri commands for the spatial core. The Board's `BoardDoc` lives in the
//! registry (single authority); these mutate it under lock and persist.

use crate::board::{self, BoardEntry, BoardRegistry};
use crate::codex::{self, CodexRegistry};
use crate::model::{
    Annotation, Asset, BoardDoc, BoardInfo, Origin, Placement, Shape, BOARD_DOC_VERSION,
};
use crate::paths::ensure_board_sidecar;
use crate::session::{self, SessionsDoc};
use crate::workspace::{self, WorkspaceEntry};
use crate::{assets, storage};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, State};

fn e2s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn with_doc_save<T>(
    entry: &BoardEntry,
    f: impl FnOnce(&mut BoardDoc) -> Result<T, String>,
) -> Result<T, String> {
    let _save = entry.save.lock();
    let (result, doc_clone) = {
        let mut doc = entry.doc.lock();
        let result = f(&mut doc)?;
        (result, doc.clone())
    };
    storage::save_board_doc(&entry.folder, &doc_clone).map_err(e2s)?;
    Ok(result)
}

/// Folder to auto-open on launch: CAMEO_OPEN_BOARD (testing) → else the most
/// recent workspace → else create a fresh default workspace.
#[tauri::command]
pub fn initial_board() -> Option<String> {
    if let Ok(p) = std::env::var("CAMEO_OPEN_BOARD") {
        if std::path::Path::new(&p).is_dir() {
            return Some(p);
        }
    }
    if let Some(first) = workspace::list().into_iter().next() {
        return Some(first.path);
    }
    workspace::create()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

// ── Workspace commands (v0.0.2) ──────────────────────────────────────────────

#[tauri::command]
pub fn list_workspaces() -> Vec<WorkspaceEntry> {
    workspace::list()
}

/// Create a fresh board folder in the app area; returns its path (frontend opens it).
#[tauri::command]
pub fn create_workspace() -> Result<String, String> {
    workspace::create()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(e2s)
}

/// Rename a workspace (display name only — folder + boardId stay). Updates the
/// index, meta.json, and the live registry entry so all references stay in sync.
#[tauri::command]
pub fn rename_workspace(
    id: String,
    name: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("empty name".into());
    }
    if let Some(entry) = workspace::list().into_iter().find(|e| e.id == id) {
        let folder = PathBuf::from(&entry.path);
        let mut meta = storage::load_meta(&folder);
        meta.name = Some(name.clone());
        storage::save_meta(&folder, &meta);
    }
    workspace::rename(&id, &name);
    if let Some(entry) = registry.get(&id) {
        *entry.name.lock() = name;
    }
    Ok(())
}

#[tauri::command]
pub fn remove_workspace(id: String) -> Result<(), String> {
    workspace::remove(&id);
    Ok(())
}

/// Optional auto-send prompt (`CAMEO_TEST_PROMPT`) for headless end-to-end
/// testing of the dispatch → generate → place loop. Dev/testing only.
#[tauri::command]
pub fn initial_test_prompt() -> Option<String> {
    std::env::var("CAMEO_TEST_PROMPT")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Open (or create) a Board from a local folder. Reconciles folder→doc, then
/// registers the in-memory authority and returns the full doc.
#[tauri::command]
pub fn open_board(path: String, registry: State<Arc<BoardRegistry>>) -> Result<BoardInfo, String> {
    let folder = PathBuf::from(&path);
    if !folder.is_dir() {
        return Err(format!("not a folder: {path}"));
    }
    ensure_board_sidecar(&folder).map_err(e2s)?;
    assets::sweep_overlays(&folder); // clear stale dispatch temps from prior runs

    let mut doc = storage::load_board_doc(&folder);
    board::reconcile(&folder, &mut doc);
    board::backfill_origins(&mut doc); // recover Origin for pre-v2 docs
    doc.version = BOARD_DOC_VERSION; // stamp current schema after migration
    storage::save_board_doc(&folder, &doc).map_err(e2s)?;

    // Stable id + display name, persisted in meta.json (generated once). Renames
    // and folder moves never change the id (cameo:// / board.json stay valid).
    let mut meta = storage::load_meta(&folder);
    let id = meta
        .board_id
        .clone()
        .unwrap_or_else(|| board::board_id_for(&folder));
    let name = meta
        .name
        .clone()
        .unwrap_or_else(|| storage::folder_name(&folder));
    if meta.board_id.is_none() || meta.name.is_none() {
        meta.board_id = Some(id.clone());
        meta.name = Some(name.clone());
        storage::save_meta(&folder, &meta);
    }

    registry.insert(
        id.clone(),
        BoardEntry {
            folder: folder.clone(),
            doc: Mutex::new(doc.clone()),
            save: Mutex::new(()),
            name: Mutex::new(name.clone()),
        },
    );
    workspace::touch(&id, &folder, &name); // record in the recent-workspaces index

    tracing::info!(module = "commands", board = %id, placements = doc.placements.len(), "board opened");
    Ok(BoardInfo {
        id,
        folder: path,
        name,
        doc,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    /// Newly tracked Assets (content-new). Existing-content imports omit theirs
    /// (the frontend already has them).
    pub assets: Vec<Asset>,
    /// One Placement per imported path (placed even on content-dedup).
    pub placements: Vec<Placement>,
}

/// Import external files (drag/drop, file picker): copy into the folder, mint
/// (dedup by content), create a grid Placement for each, persist.
#[tauri::command]
pub fn import_paths(
    board_id: String,
    paths: Vec<String>,
    registry: State<Arc<BoardRegistry>>,
) -> Result<ImportResult, String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let mut known_assets = entry.doc.lock().assets.clone();
    let mut staged: Vec<Asset> = Vec::new();
    for p in &paths {
        let src = PathBuf::from(p);
        match assets::import_external(&entry.folder, &src, &known_assets) {
            Ok(asset) => {
                if !known_assets.iter().any(|a| a.id == asset.id) {
                    known_assets.push(asset.clone());
                }
                staged.push(asset);
            }
            Err(e) => tracing::warn!(module = "commands", "import {p} failed: {e}"),
        }
    }

    if staged.is_empty() {
        return Ok(ImportResult {
            assets: Vec::new(),
            placements: Vec::new(),
        });
    }

    with_doc_save(&entry, |doc| {
        let mut out = ImportResult {
            assets: Vec::new(),
            placements: Vec::new(),
        };
        for asset in &staged {
            if !doc.assets.iter().any(|a| a.id == asset.id) {
                doc.assets.push(asset.clone());
                out.assets.push(asset.clone());
            }
        }
        let foots: Vec<(f64, f64)> = staged.iter().map(board::footprint).collect();
        let centers = board::flow_layout(board::next_batch_top(&doc), &foots);
        let base_z = doc.placements.len() as i64;
        for (i, asset) in staged.iter().enumerate() {
            let (x, y) = centers[i];
            let placement = board::make_placement(asset, x, y, base_z + i as i64, None);
            doc.placements.push(placement.clone());
            out.placements.push(placement);
        }
        Ok(out)
    })
}

/// Import raw image bytes (clipboard paste).
#[tauri::command]
pub fn import_image_bytes(
    board_id: String,
    bytes: Vec<u8>,
    ext: String,
    stem: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<ImportResult, String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let stem = if stem.is_empty() { "paste" } else { &stem };
    let assets_snapshot = entry.doc.lock().assets.clone();

    let asset = assets::import_bytes(
        &entry.folder,
        &bytes,
        &ext,
        stem,
        Origin::Paste,
        &assets_snapshot,
    )
    .map_err(e2s)?;

    with_doc_save(&entry, |doc| {
        let mut out = ImportResult {
            assets: Vec::new(),
            placements: Vec::new(),
        };
        let is_new = !doc.assets.iter().any(|a| a.id == asset.id);
        if is_new {
            doc.assets.push(asset.clone());
            out.assets.push(asset.clone());
        }
        let centers = board::flow_layout(board::next_batch_top(&doc), &[board::footprint(&asset)]);
        let (x, y) = centers[0];
        let placement = board::make_placement(&asset, x, y, doc.placements.len() as i64, None);
        doc.placements.push(placement.clone());
        out.placements.push(placement);
        Ok(out)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacementUpdate {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub rotation: f64,
    pub z: i64,
}

/// Apply transform changes (move/scale/rotate/restack) from the canvas and persist.
#[tauri::command]
pub fn update_placements(
    board_id: String,
    updates: Vec<PlacementUpdate>,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    with_doc_save(&entry, |doc| {
        for u in &updates {
            if let Some(p) = doc.placements.iter_mut().find(|p| p.id == u.id) {
                p.x = u.x;
                p.y = u.y;
                p.scale = u.scale;
                p.rotation = u.rotation;
                p.z = u.z;
            }
        }
        Ok(())
    })
}

/// Remove Placements from the canvas. Non-destructive: the Asset + file stay on
/// disk (and won't be re-placed on reopen, since the Asset is still tracked).
#[tauri::command]
pub fn delete_placements(
    board_id: String,
    ids: Vec<String>,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    with_doc_save(&entry, |doc| {
        doc.placements.retain(|p| !ids.contains(&p.id));
        Ok(())
    })
}

/// Re-insert placements (undo of delete / redo of add). Idempotent by id; the
/// backing Assets are untouched by delete, so they're still present on disk.
#[tauri::command]
pub fn restore_placements(
    board_id: String,
    placements: Vec<Placement>,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    with_doc_save(&entry, |doc| {
        for p in placements {
            // Upsert by id: restores a deleted placement OR reverts a changed one
            // (e.g. undo of crop, which swapped the assetId).
            if let Some(existing) = doc.placements.iter_mut().find(|e| e.id == p.id) {
                *existing = p;
            } else {
                doc.placements.push(p);
            }
        }
        Ok(())
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceResult {
    /// The minted asset, if content-new (else the frontend already has it).
    pub asset: Option<Asset>,
    pub placement: Placement,
}

/// Replace a placement's backing image with new bytes (e.g. a crop bake). Mints
/// a new Asset (dedup by content), repoints the placement, persists. The old
/// Asset file is untouched (non-destructive).
#[tauri::command]
pub fn replace_placement_image(
    board_id: String,
    placement_id: String,
    bytes: Vec<u8>,
    ext: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<ReplaceResult, String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let assets_snapshot = {
        let doc = entry.doc.lock();
        if !doc.placements.iter().any(|p| p.id == placement_id) {
            return Err("placement not found".into());
        }
        doc.assets.clone()
    };
    let asset = assets::import_bytes(
        &entry.folder,
        &bytes,
        &ext,
        "crop",
        Origin::Crop,
        &assets_snapshot,
    )
    .map_err(e2s)?;
    let (is_new, updated) = with_doc_save(&entry, |doc| {
        let is_new = !doc.assets.iter().any(|a| a.id == asset.id);
        if is_new {
            doc.assets.push(asset.clone());
        }
        let placement = doc
            .placements
            .iter_mut()
            .find(|p| p.id == placement_id)
            .ok_or("placement not found")?;
        placement.asset_id = asset.id.clone();
        placement.crop = None;
        let updated = placement.clone();
        Ok((is_new, updated))
    })?;
    Ok(ReplaceResult {
        asset: if is_new { Some(asset) } else { None },
        placement: updated,
    })
}

// ── Export (v0.0.2) ──────────────────────────────────────────────────────────

/// Resolve a placement → its backing asset's absolute file path, guarding
/// against board.json paths that try to escape the folder (`../…`).
fn asset_abs_path(entry: &BoardEntry, placement_id: &str) -> Result<PathBuf, String> {
    let rel = {
        let doc = entry.doc.lock();
        let asset_id = doc
            .placements
            .iter()
            .find(|p| p.id == placement_id)
            .map(|p| p.asset_id.clone())
            .ok_or("placement not found")?;
        doc.assets
            .iter()
            .find(|a| a.id == asset_id)
            .map(|a| a.path.clone())
            .ok_or("asset not found")?
    };
    let abs = entry.folder.join(&rel).canonicalize().map_err(e2s)?;
    let root = entry.folder.canonicalize().map_err(e2s)?;
    if !abs.starts_with(&root) {
        return Err("asset path escapes the board folder".into());
    }
    Ok(abs)
}

/// Reveal the backing file in the OS file manager (Finder / Explorer / Linux).
#[tauri::command]
pub fn reveal_in_finder(
    board_id: String,
    placement_id: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let path = asset_abs_path(&entry, &placement_id)?;
    tauri_plugin_opener::reveal_item_in_dir(&path).map_err(e2s)
}

/// Copy the backing image to the system clipboard.
#[tauri::command]
pub fn copy_image(
    board_id: String,
    placement_id: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let path = asset_abs_path(&entry, &placement_id)?;
    let img = image::open(&path).map_err(e2s)?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut cb = arboard::Clipboard::new().map_err(e2s)?;
    cb.set_image(arboard::ImageData {
        width: w,
        height: h,
        bytes: std::borrow::Cow::Owned(img.into_raw()),
    })
    .map_err(e2s)?;
    Ok(())
}

// ── Chat-text-embedded image references ─────────────────────────────────────
//
// When the AI's reply mentions an image path (markdown, Windows absolute path,
// slash/backslash relative path, or a bare workspace filename), these commands
// cooperate to render + canvas-import + clipboard / reveal it. See
// src/lib/chatImageDetect.ts for the path-extraction regex and
// src/components/ChatInlineImage.tsx for the UI side.

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatImageResolution {
    /// Canonical absolute path on disk (or best-effort joined path if the
    /// file doesn't exist — useful for the UI to display the attempted name).
    abs_path: String,
    /// Whether `abs_path` resolves to a real file.
    exists: bool,
    /// Whether the path has an image extension. (Independent of `exists` —
    /// we may want to surface "this looks like an image path but the file
    /// is missing" diagnostics.)
    is_image: bool,
    /// True when `abs_path` is inside the board's workspace folder.
    in_workspace: bool,
    /// Relative to the workspace folder, when `in_workspace`. The UI uses
    /// this with `cameoUrl(boardId, relPath)` to load the full image.
    workspace_rel_path: Option<String>,
    /// Base64-encoded JPEG thumbnail (max side 240 px) for OUT-OF-workspace
    /// images, where `cameo://` won't reach. Generated once per resolve and
    /// cached in the JS chat store.
    thumb_data_url: Option<String>,
    /// Human-readable reason when the resolution doesn't yield a usable image.
    error: Option<String>,
}

/// Classify a path string from chat text: expand tilde, join against the
/// workspace folder if relative, canonicalize, check it's an image. Returns
/// enough info for the renderer to display the right card variant.
#[tauri::command]
pub fn resolve_chat_image(
    board_id: String,
    raw_path: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<ChatImageResolution, String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let raw = normalize_chat_path_raw(&raw_path);

    // Expand a leading `~/` or `~\` against the user's home dir. We DON'T do
    // `$HOME` expansion or anything more elaborate — the AI typically emits
    // clean paths or markdown.
    let expanded: PathBuf = if let Some(file_url) = file_url_to_path(&raw) {
        file_url
    } else if let Some(stripped) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        dirs::home_dir()
            .map(|h| h.join(stripped))
            .unwrap_or_else(|| PathBuf::from(&raw))
    } else if raw == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))
    } else {
        PathBuf::from(&raw)
    };

    // Relative paths resolve against the workspace folder.
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        entry.folder.join(expanded)
    };
    let canonical = std::fs::canonicalize(&abs).unwrap_or(abs);

    let exists = canonical.is_file();
    let is_image = is_image_extension(&canonical);

    if !exists || !is_image {
        return Ok(ChatImageResolution {
            abs_path: canonical.to_string_lossy().to_string(),
            exists,
            is_image,
            in_workspace: false,
            workspace_rel_path: None,
            thumb_data_url: None,
            error: Some(
                if !exists {
                    "file not found"
                } else {
                    "not an image"
                }
                .to_string(),
            ),
        });
    }

    let canonical_workspace =
        std::fs::canonicalize(&entry.folder).unwrap_or_else(|_| entry.folder.clone());
    let in_workspace = canonical.starts_with(&canonical_workspace);
    let workspace_rel_path = if in_workspace {
        canonical
            .strip_prefix(&canonical_workspace)
            .ok()
            .map(path_to_url_rel)
    } else {
        None
    };

    // Out-of-workspace images can't be loaded via `cameo://` (which is scoped
    // to one board folder); inline them as a base64 thumbnail instead. JS
    // caches the result so the cost is paid once per unique path per session.
    let thumb_data_url = if !in_workspace {
        match build_thumb_data_url(&canonical) {
            Ok(url) => Some(url),
            Err(e) => {
                tracing::warn!(
                    module = "commands",
                    "chat-image thumb gen failed for {}: {e}",
                    canonical.display()
                );
                None
            }
        }
    } else {
        None
    };

    Ok(ChatImageResolution {
        abs_path: canonical.to_string_lossy().to_string(),
        exists: true,
        is_image: true,
        in_workspace,
        workspace_rel_path,
        thumb_data_url,
        error: None,
    })
}

fn normalize_chat_path_raw(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| matches!(c, '`' | '"' | '\'' | '<' | '>'))
        .to_string()
}

fn file_url_to_path(raw: &str) -> Option<PathBuf> {
    if !raw
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("file://"))
    {
        return None;
    }
    let mut rest = raw[7..].to_string();
    if let Some(local) = rest
        .strip_prefix("localhost/")
        .or_else(|| rest.strip_prefix("localhost\\"))
    {
        rest = format!("/{local}");
    }
    let decoded = urlencoding::decode(&rest)
        .map(|c| c.into_owned())
        .unwrap_or(rest);

    #[cfg(windows)]
    {
        let path = if decoded.len() >= 3 {
            let bytes = decoded.as_bytes();
            if bytes[0] == b'/' && bytes[2] == b':' && bytes[1].is_ascii_alphabetic() {
                &decoded[1..]
            } else {
                decoded.as_str()
            }
        } else {
            decoded.as_str()
        };
        return Some(PathBuf::from(path));
    }

    #[cfg(not(windows))]
    Some(PathBuf::from(decoded))
}

fn path_to_url_rel(path: &Path) -> String {
    let mut parts = Vec::new();
    for comp in path.components() {
        if let Component::Normal(part) = comp {
            parts.push(part.to_string_lossy());
        }
    }
    parts.join("/")
}

fn is_image_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "avif")
    )
}

fn build_thumb_data_url(path: &Path) -> anyhow::Result<String> {
    use base64::Engine;
    use std::io::Cursor;
    let img = image::open(path)?;
    let thumb = img.thumbnail(240, 240);
    let mut buf = Cursor::new(Vec::new());
    thumb.write_to(&mut buf, image::ImageFormat::Jpeg)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
    Ok(format!("data:image/jpeg;base64,{}", encoded))
}

#[cfg(test)]
mod tests {
    use super::{file_url_to_path, normalize_chat_path_raw, path_to_url_rel};
    use std::path::Path;

    #[test]
    fn chat_path_normalization_trims_common_wrappers() {
        assert_eq!(
            normalize_chat_path_raw("  `cute-tiger.png`  "),
            "cute-tiger.png"
        );
    }

    #[test]
    fn file_url_decodes_percent_escapes() {
        let path = file_url_to_path("file:///tmp/cute%20tiger.png").unwrap();
        assert!(path.to_string_lossy().contains("cute tiger.png"));
    }

    #[test]
    fn workspace_relative_paths_are_url_slash_separated() {
        let rel = Path::new("imports").join("cute-tiger.png");
        assert_eq!(path_to_url_rel(&rel), "imports/cute-tiger.png");
    }
}

/// User-initiated "add to canvas" from the chat-image right-click menu.
/// If `abs_path` is outside the workspace folder we first copy the file in to
/// `<workspace>/imports/`, then run the normal import (which content-addresses
/// the bytes and dedupes against existing assets). Returns the new placement(s)
/// in the same shape as `import_paths` so the JS side can update its caches.
#[tauri::command]
pub fn import_chat_image_to_canvas(
    board_id: String,
    abs_path: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<ImportResult, String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let src = PathBuf::from(&abs_path);
    if !src.is_file() {
        return Err(format!("not a file: {abs_path}"));
    }

    let canonical_workspace =
        std::fs::canonicalize(&entry.folder).unwrap_or_else(|_| entry.folder.clone());
    let canonical_src = std::fs::canonicalize(&src).unwrap_or(src.clone());
    let in_workspace = canonical_src.starts_with(&canonical_workspace);

    // If outside the workspace, stage into <workspace>/imports/ first so the
    // ensuing import_external sees a workspace-relative source and the
    // Placement winds up referencing in-workspace bytes (Cameo's "all canvas
    // resources live in the workspace folder" invariant).
    let staged_src = if in_workspace {
        canonical_src
    } else {
        let imports_dir = entry.folder.join("imports");
        std::fs::create_dir_all(&imports_dir).map_err(e2s)?;
        let original_name = canonical_src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("imported.png");
        let target_name = uniqueify_import_name(&imports_dir, original_name, &canonical_src)?;
        let target = imports_dir.join(&target_name);
        if !target.exists() {
            std::fs::copy(&canonical_src, &target).map_err(e2s)?;
        }
        target
    };

    let assets_snapshot = entry.doc.lock().assets.clone();
    let asset = assets::import_external(&entry.folder, &staged_src, &assets_snapshot)
        .map_err(|e| format!("import failed: {e}"))?;

    with_doc_save(&entry, |doc| {
        let mut out = ImportResult {
            assets: Vec::new(),
            placements: Vec::new(),
        };
        if !doc.assets.iter().any(|a| a.id == asset.id) {
            doc.assets.push(asset.clone());
            out.assets.push(asset.clone());
        }
        let centers = board::flow_layout(board::next_batch_top(&doc), &[board::footprint(&asset)]);
        let (x, y) = centers[0];
        let placement = board::make_placement(&asset, x, y, doc.placements.len() as i64, None);
        doc.placements.push(placement.clone());
        out.placements.push(placement);
        Ok(out)
    })
}

/// Resolve a target filename inside `imports_dir` that:
///   • keeps the original filename when free OR when the existing file is the
///     same bytes as `src` (idempotent re-import of the same chat reference),
///   • otherwise appends a 6-char content-hash suffix so two DIFFERENT files
///     with the same basename don't clobber each other.
fn uniqueify_import_name(
    imports_dir: &Path,
    original_name: &str,
    src: &Path,
) -> Result<String, String> {
    let target = imports_dir.join(original_name);
    if !target.exists() {
        return Ok(original_name.to_string());
    }
    if let (Ok(a), Ok(b)) = (std::fs::read(src), std::fs::read(&target)) {
        if a == b {
            return Ok(original_name.to_string()); // same content — reuse
        }
    }
    let stem = Path::new(original_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported");
    let ext = Path::new(original_name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("png");
    let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
    let hash = blake3::hash(&bytes);
    let short: String = hash.to_hex().chars().take(6).collect();
    Ok(format!("{stem}-{short}.{ext}"))
}

/// Copy an arbitrary file (PNG bytes) to the system clipboard. Sibling of the
/// existing `copy_image` (which takes a placement id) — this one accepts any
/// path so the chat-image right-click menu works for files that aren't yet
/// canvas placements.
#[tauri::command]
pub fn copy_image_from_path(abs_path: String) -> Result<(), String> {
    let path = PathBuf::from(&abs_path);
    if !path.is_file() {
        return Err(format!("not a file: {abs_path}"));
    }
    let img = image::open(&path).map_err(e2s)?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut cb = arboard::Clipboard::new().map_err(e2s)?;
    cb.set_image(arboard::ImageData {
        width: w,
        height: h,
        bytes: std::borrow::Cow::Owned(img.into_raw()),
    })
    .map_err(e2s)?;
    Ok(())
}

/// Reveal an arbitrary file in the OS file manager. Sibling of the existing
/// `reveal_in_finder` (placement-id based) — used by the chat right-click
/// menu for paths that may not yet be canvas placements.
#[tauri::command]
pub fn reveal_path_in_finder(abs_path: String) -> Result<(), String> {
    tauri_plugin_opener::reveal_item_in_dir(&abs_path).map_err(e2s)
}

/// Export (copy) the backing file to a chosen destination path.
#[tauri::command]
pub fn export_asset(
    board_id: String,
    placement_id: String,
    dest: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let src = asset_abs_path(&entry, &placement_id)?;
    std::fs::copy(&src, &dest).map_err(e2s)?;
    Ok(())
}

// ── Codex runtime commands (Phase 3) ─────────────────────────────────────────

/// Spawn (or reuse) the Codex app-server session for a Board. Returns threadId.
#[tauri::command]
pub async fn start_session(
    app: AppHandle,
    board_id: String,
    boards: State<'_, Arc<BoardRegistry>>,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<String, String> {
    codex::start_session(app, boards.inner().clone(), codex.inner().clone(), board_id).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayRef {
    pub placement_id: String,
    pub path: String,
}

/// Send a turn. `sources` = referenced Placement ids (their files are named in
/// the prompt for the agent to read — decision D4). `overlays` pairs a source
/// with its rendered marking-overlay file path (decision D2).
#[tauri::command]
pub async fn send_message(
    board_id: String,
    text: String,
    sources: Vec<String>,
    overlays: Vec<OverlayRef>,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<(), String> {
    let ov = overlays
        .into_iter()
        .map(|o| (o.placement_id, o.path))
        .collect();
    codex::send_message(codex.inner().clone(), board_id, text, sources, ov).await
}

/// Replace the annotation shapes for a Placement (empty clears it). Persisted.
#[tauri::command]
pub fn set_annotation(
    board_id: String,
    placement_id: String,
    shapes: Vec<Shape>,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    with_doc_save(&entry, |doc| {
        doc.annotations.retain(|a| a.placement_id != placement_id);
        if !shapes.is_empty() {
            doc.annotations.push(Annotation {
                placement_id,
                shapes,
            });
        }
        Ok(())
    })
}

/// Rename the file backing a Placement's Asset on disk, keeping the extension
/// if the new name omits one, and auto-suffixing on collision. Updates the
/// Asset path (content id is unchanged — same bytes). Returns the new rel path.
#[tauri::command]
pub fn rename_asset(
    board_id: String,
    placement_id: String,
    new_name: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<String, String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let _save = entry.save.lock();
    let old_path = {
        let doc = entry.doc.lock();
        let asset_id = doc
            .placements
            .iter()
            .find(|p| p.id == placement_id)
            .map(|p| p.asset_id.clone())
            .ok_or("placement not found")?;
        doc.assets
            .iter()
            .find(|a| a.id == asset_id)
            .map(|a| a.path.clone())
            .ok_or("asset not found")?
    };

    // Sanitize: basename only, no path separators or leading dots.
    let raw = new_name
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim();
    let raw = raw.trim_start_matches('.').trim();
    if raw.is_empty() {
        return Err("empty name".into());
    }
    let old_ext = std::path::Path::new(&old_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let has_ext = std::path::Path::new(raw).extension().is_some();
    let mut desired = if has_ext || old_ext.is_empty() {
        raw.to_string()
    } else {
        format!("{raw}.{old_ext}")
    };
    if desired == old_path {
        return Ok(old_path);
    }

    let folder = &entry.folder;
    if folder.join(&desired).exists() {
        let path = std::path::Path::new(&desired);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("image")
            .to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let mut i = 1;
        loop {
            let cand = if ext.is_empty() {
                format!("{stem}-{i}")
            } else {
                format!("{stem}-{i}.{ext}")
            };
            if !folder.join(&cand).exists() {
                desired = cand;
                break;
            }
            i += 1;
        }
    }

    std::fs::rename(folder.join(&old_path), folder.join(&desired)).map_err(e2s)?;
    let doc_clone = {
        let mut doc = entry.doc.lock();
        for a in doc.assets.iter_mut() {
            if a.path == old_path {
                a.path = desired.clone();
            }
        }
        doc.clone()
    };
    storage::save_board_doc(folder, &doc_clone).map_err(e2s)?;
    Ok(desired)
}

/// Write a rendered overlay PNG into the Board root as a dotfile
/// (`.overlay-<id>.png`): in the workspace so Codex's sandbox can read it (D5),
/// hidden + skipped by our image scan, and not under `.cameo/` (D7). Returns the
/// relative path to name in the prompt.
#[tauri::command]
pub fn write_overlay(
    board_id: String,
    bytes: Vec<u8>,
    registry: State<Arc<BoardRegistry>>,
) -> Result<String, String> {
    let entry = registry.get(&board_id).ok_or("unknown board")?;
    let name = format!(".overlay-{}.png", nanoid::nanoid!(8));
    std::fs::write(entry.folder.join(&name), &bytes).map_err(e2s)?;
    Ok(name)
}

#[tauri::command]
pub async fn interrupt_turn(
    board_id: String,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<(), String> {
    codex::interrupt_turn(codex.inner().clone(), board_id).await
}

#[tauri::command]
pub async fn respond_permission(
    board_id: String,
    request_id: u64,
    accept: bool,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<(), String> {
    codex::respond_permission(codex.inner().clone(), board_id, request_id, accept).await
}

#[tauri::command]
pub async fn codex_auth_status(
    board_id: String,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<serde_json::Value, String> {
    codex::auth_status(codex.inner().clone(), board_id).await
}

#[tauri::command]
pub async fn stop_session(
    board_id: String,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<(), String> {
    codex::stop_session(codex.inner().clone(), &board_id).await;
    Ok(())
}

// ── Multi-session (v0.0.2) ───────────────────────────────────────────────────

/// All sessions for a Board + the active one.
#[tauri::command]
pub fn list_sessions(
    board_id: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<SessionsDoc, String> {
    let folder = registry.folder(&board_id).ok_or("unknown board")?;
    Ok(session::load(&folder))
}

/// Start a fresh conversation (new thread); returns its session id.
#[tauri::command]
pub async fn new_session(
    board_id: String,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<String, String> {
    codex::new_session(codex.inner().clone(), board_id).await
}

#[tauri::command]
pub async fn switch_session(
    board_id: String,
    session_id: String,
    codex: State<'_, Arc<CodexRegistry>>,
) -> Result<(), String> {
    codex::switch_session(codex.inner().clone(), board_id, session_id).await
}

#[tauri::command]
pub fn rename_session(
    board_id: String,
    session_id: String,
    title: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let folder = registry.folder(&board_id).ok_or("unknown board")?;
    session::rename(&folder, &session_id, title.trim());
    Ok(())
}

/// Load a session's persisted message timeline (opaque JSON the frontend renders).
#[tauri::command]
pub fn load_session(
    board_id: String,
    session_id: String,
    registry: State<Arc<BoardRegistry>>,
) -> Result<Vec<serde_json::Value>, String> {
    let folder = registry.folder(&board_id).ok_or("unknown board")?;
    Ok(session::load_timeline(&folder, &session_id))
}

/// Append one message to a session's timeline (frontend calls on send + turn end).
#[tauri::command]
pub fn append_message(
    board_id: String,
    session_id: String,
    message: serde_json::Value,
    registry: State<Arc<BoardRegistry>>,
) -> Result<(), String> {
    let folder = registry.folder(&board_id).ok_or("unknown board")?;
    session::append_message(&folder, &session_id, &message);
    Ok(())
}

// ── App config (global ~/.cameo/config.json) + diagnostics ───────────────────

/// Load the global app config (network proxy etc.). Missing/corrupt → defaults.
#[tauri::command]
pub fn cfg_load() -> crate::config::AppConfig {
    crate::config::load()
}

/// Persist the global app config.
#[tauri::command]
pub fn cfg_save(config: crate::config::AppConfig) -> Result<(), String> {
    crate::config::save(&config).map_err(e2s)
}

/// Open the unified log folder (`~/.cameo/logs`) in the OS file manager.
#[tauri::command]
pub fn open_logs_dir() -> Result<(), String> {
    let dir = crate::paths::cameo_logs_dir();
    tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(e2s)
}

/// Detect the local Codex CLI (path + version) for the agent-status panel.
#[tauri::command]
pub fn detect_codex() -> codex::CodexInfo {
    codex::detect()
}

/// Open a directory in the OS file manager (workspace "open folder" action).
#[tauri::command]
pub fn open_dir(path: String) -> Result<(), String> {
    tauri_plugin_opener::open_path(&path, None::<&str>).map_err(e2s)
}

/// Read an image from the system clipboard as PNG bytes (None if there's no
/// image). Backs the canvas "粘贴" context-menu action.
#[tauri::command]
pub fn read_clipboard_image() -> Result<Option<Vec<u8>>, String> {
    let mut cb = arboard::Clipboard::new().map_err(e2s)?;
    match cb.get_image() {
        Ok(img) => {
            let w = img.width as u32;
            let h = img.height as u32;
            let rgba = image::RgbaImage::from_raw(w, h, img.bytes.into_owned())
                .ok_or("clipboard image had unexpected dimensions")?;
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(rgba)
                .write_to(&mut png, image::ImageFormat::Png)
                .map_err(e2s)?;
            Ok(Some(png.into_inner()))
        }
        Err(_) => Ok(None),
    }
}
