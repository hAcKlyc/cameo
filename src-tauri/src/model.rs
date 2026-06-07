//! Board data model — the serde shapes that persist in `<folder>/.cameo/board.json`
//! and cross the IPC boundary to the frontend.
//!
//! Invariants (PRD §4, decisions D6):
//! - **Asset** is immutable + content-addressed (blake3). Originals are never
//!   rewritten; every op mints a new Asset.
//! - **Placement** is the mutable, non-destructive presentation of an Asset on
//!   the canvas. Lineage lives in `parent_id`; `x/y` is computed at placement
//!   time, then owned by the user.

use serde::{Deserialize, Serialize};

/// Current `board.json` schema version. Bump + migrate on shape changes.
/// v2: added `Asset.origin` (additive — old docs default to `imported`, then
/// generated assets are recovered on load via lineage backfill, see board.rs).
/// v3: added time-based media fields on `Asset` (`duration_ms`/`fps`/`has_audio`/
/// `poster_path`) + the `Frame` origin. All additive `Option` (old docs load
/// with `None`); the migration is therefore a no-op beyond re-stamping the
/// version — `media_kind` is NOT stored, it's derived from `mime` on the
/// frontend (src/lib/media.ts) so it can never go stale.
pub const BOARD_DOC_VERSION: u32 = 3;

/// How an Asset entered the Board. Drives the on-disk naming scheme
/// (`<origin>-<timestamp>` for minted files; imports keep their original name)
/// and future "originals vs outputs" filtering. Classification lives in the
/// state JSON so we never need role subfolders (the folder is the agent's cwd).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Dropped/picked by the user (original source material). Keeps its filename.
    Imported,
    /// Produced by the agent (Codex imageGeneration).
    Generated,
    /// Local non-destructive crop bake.
    Crop,
    /// Pasted from the clipboard.
    Paste,
    /// A still extracted from a video Asset (lineage in the Placement's
    /// `parent_id`, like any derived output). Drives `frame-<timestamp>` naming.
    Frame,
}

impl Default for Origin {
    fn default() -> Self {
        Origin::Imported
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Immutable, content-addressed image. The `path` is relative to the Board
/// folder (= the agent's cwd).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    /// blake3 hex of file contents.
    pub id: String,
    /// Path relative to the Board folder.
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub mime: String,
    /// Unix ms.
    pub created_at: i64,
    /// How this Asset entered the Board (additive — old docs default to `imported`).
    #[serde(default)]
    pub origin: Origin,
    // ── Time-based media (v3, video). All additive `Option`: `None` on images
    // and on any video minted while ffmpeg was unavailable. `media_kind` is NOT
    // stored — derive it from `mime` (image/* vs video/*) so it can't go stale.
    /// Video duration in milliseconds (from ffprobe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    /// Video frame rate (from ffprobe `avg_frame_rate`), for frame stepping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// Whether the video carries an audio stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_audio: Option<bool>,
    /// Board-relative path to the extracted poster frame (the canvas renders this
    /// as the still texture; `<video>` overlays it only when focused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster_path: Option<String>,
}

/// A placed instance of an Asset on the canvas. Mutating a Placement never
/// touches the Asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub id: String,
    pub asset_id: String,
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub rotation: f64,
    pub z: i64,
    /// Non-destructive crop frame in Asset pixel space.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub crop: Option<Rect>,
    /// Lineage: which Placement this was derived from (drives right-of-source layout).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_id: Option<String>,
    /// Which Op produced this Placement (Phase 3+).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub from_op_id: Option<String>,
}

/// A vector mark on a Placement. Points are in Asset-pixel coords with origin at
/// the image center (so an overlay renders 1:1 at native resolution).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shape {
    /// "rect" | "ellipse" | "arrow" | "path"
    pub kind: String,
    pub points: Vec<[f64; 2]>,
    /// Stable mark id (so a note binds to the right mark).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Per-mark instruction typed in the comment box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Vector annotation layer attached to one Placement. Rendered to an overlay
/// image at dispatch time (decision D2: overlay-as-image).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub placement_id: String,
    pub shapes: Vec<Shape>,
}

/// The persisted Board document: the spatial projection of the folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardDoc {
    pub version: u32,
    pub assets: Vec<Asset>,
    pub placements: Vec<Placement>,
    /// Additive (serde default) — old docs without it load as empty.
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

impl Default for BoardDoc {
    fn default() -> Self {
        BoardDoc {
            version: BOARD_DOC_VERSION,
            assets: Vec::new(),
            placements: Vec::new(),
            annotations: Vec::new(),
        }
    }
}

/// `<folder>/.cameo/meta.json` — per-Board identity + runtime + active session.
/// `board_id` is a STABLE persisted id (generated once): renaming the folder or
/// the workspace name never changes it, so image URLs / board.json / sessions
/// stay valid. `thread_id` is the legacy single-session token (migrated into
/// sessions.json in v0.0.2; kept for back-compat reads).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardMeta {
    pub board_id: Option<String>,
    pub name: Option<String>,
    pub runtime: Option<String>,
    /// Active session (v0.0.2 multi-session).
    pub active_session_id: Option<String>,
    /// Legacy v0.0.1 single-session thread; migrated into sessions on open.
    pub thread_id: Option<String>,
    /// Per-Board generation knobs (v0.1.6). All additive `Option` → old metas
    /// load as `None` and fall back to product defaults at dispatch time.
    /// `None` model/effort → defaults (gpt-5.5 / medium); `None` service tier →
    /// standard (sent as explicit JSON null, which Codex treats as "clear").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gen_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gen_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gen_service_tier: Option<String>,
    /// Per-Board Codex permission mode (v0.1.9). `None` loads as the product
    /// default ("workspace"). This is a UI-level mode; the runtime maps it to
    /// Codex approval/sandbox fields on thread/start and turn/start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

/// The user's generation knobs for a Board, crossing the IPC boundary and held
/// on the live Codex session. `service_tier == None` means the standard tier
/// (dispatched as explicit JSON `null`, see CODEX_PROTOCOL.md §4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenSettings {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}

/// The Board's Codex permission preset. `mode == None` means the product
/// default ("workspace"); the only persisted concrete values are "workspace"
/// and "fullAccess".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSettings {
    pub mode: Option<String>,
}

/// Returned to the frontend when a Board is opened.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardInfo {
    /// Stable persisted board id (survives folder/name changes).
    pub id: String,
    /// Absolute folder path.
    pub folder: String,
    /// Display name (workspace name).
    pub name: String,
    pub doc: BoardDoc,
}
