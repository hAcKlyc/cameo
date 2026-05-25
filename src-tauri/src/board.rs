//! Board registry + folder↔doc reconciliation.
//!
//! The registry is the **single in-memory authority** for each open Board's
//! `BoardDoc`. All mutations funnel through commands that lock the entry's doc
//! and persist it; the frontend keeps a render mirror. Placement creation lives
//! here (one implementation, used by both reconcile and import) so layout
//! defaults never drift between code paths.

use crate::assets;
use crate::model::{Asset, BoardDoc, Origin, Placement};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct BoardEntry {
    pub folder: PathBuf,
    pub doc: Mutex<BoardDoc>,
    /// Serializes doc mutation snapshots with their disk saves. Commands keep
    /// expensive IO outside `doc`, but this prevents an older clone from saving
    /// after a newer mutation and rolling back `board.json`.
    pub save: Mutex<()>,
    /// Display name (workspace name); mutable via rename.
    pub name: Mutex<String>,
}

#[derive(Default)]
pub struct BoardRegistry {
    inner: Mutex<HashMap<String, Arc<BoardEntry>>>,
}

/// Stable id from the folder path → `cameo://` URLs + texture caches survive reopen.
pub fn board_id_for(folder: &Path) -> String {
    let s = folder.to_string_lossy();
    blake3::hash(s.as_bytes()).to_hex()[..12].to_string()
}

impl BoardRegistry {
    pub fn insert(&self, id: String, entry: BoardEntry) -> Arc<BoardEntry> {
        let arc = Arc::new(entry);
        self.inner.lock().insert(id, arc.clone());
        arc
    }

    pub fn get(&self, id: &str) -> Option<Arc<BoardEntry>> {
        self.inner.lock().get(id).cloned()
    }

    pub fn folder(&self, id: &str) -> Option<PathBuf> {
        self.inner.lock().get(id).map(|e| e.folder.clone())
    }
}

// ── Display tiers + flow layout (user rearranges afterwards) ─────────────────
//
// Display size is decoupled from intrinsic resolution: the on-canvas longest
// side snaps to the nearest power of two in `[TIER_MIN, TIER_MAX]` (world
// units). Powers of two are deliberate — a 1024² generation lands at scale 1.0
// (zero resampling, crispest) and the tier sizes line up with GPU mipmap
// levels. Rounding in log2 space == geometric (×√2) midpoints, so resample
// distortion stays ≤√2 either way. Asset bytes are never touched (non-destructive).

const TIER_MIN: f64 = 128.0;
const TIER_MAX: f64 = 2048.0;
/// Gap between tiles, both within an import flow and around derived outputs.
const GAP: f64 = 80.0;
/// Wrap a flow row once it would pass this width; bigger tiles ⇒ fewer per row.
const ROW_MAX_W: f64 = 2400.0;
/// Footprint estimate for source-less placeholders (no asset to measure yet).
const DEFAULT_TILE: f64 = 512.0;

/// Snap an intrinsic longest-side length (px) to its display tier (world units).
fn snap_tier(longest: f64) -> f64 {
    if longest <= 0.0 {
        return TIER_MIN;
    }
    2f64.powf(longest.log2().round()).clamp(TIER_MIN, TIER_MAX)
}

fn default_scale(a: &Asset) -> f64 {
    let m = a.width.max(a.height) as f64;
    if m <= 0.0 {
        1.0
    } else {
        snap_tier(m) / m
    }
}

/// On-canvas footprint (w, h in world units) at the asset's display tier.
pub fn footprint(a: &Asset) -> (f64, f64) {
    let s = default_scale(a);
    (a.width as f64 * s, a.height as f64 * s)
}

/// Bottom edge of current content (`max(y + h/2)` over placements), or `None`
/// for an empty board. Fresh batches flow below this; existing layout never moves.
fn content_bottom(doc: &BoardDoc) -> Option<f64> {
    let mut bottom: Option<f64> = None;
    for p in &doc.placements {
        if let Some(a) = doc.assets.iter().find(|a| a.id == p.asset_id) {
            let edge = p.y + (a.height as f64 * p.scale) / 2.0;
            bottom = Some(bottom.map_or(edge, |cur: f64| cur.max(edge)));
        }
    }
    bottom
}

/// Top edge where the next imported/scanned batch should start flowing.
pub fn next_batch_top(doc: &BoardDoc) -> f64 {
    content_bottom(doc).map_or(0.0, |b| b + GAP)
}

/// Flow `footprints` (w,h, in order) left→right into rows starting at `top`,
/// wrapping once a row would pass `ROW_MAX_W`. Returns a center (x,y) per item.
pub fn flow_layout(top: f64, footprints: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(footprints.len());
    let mut x = 0.0; // left-edge cursor of the current row
    let mut row_top = top;
    let mut row_h = 0.0f64;
    for &(w, h) in footprints {
        if x > 0.0 && x + w > ROW_MAX_W {
            row_top += row_h + GAP;
            x = 0.0;
            row_h = 0.0;
        }
        out.push((x + w / 2.0, row_top + h / 2.0));
        x += w + GAP;
        row_h = row_h.max(h);
    }
    out
}

/// Build a Placement for an Asset at an explicit center. The single source of
/// placement defaults (display-tier scale, no crop); position comes from the
/// layout helpers above so defaults never drift between code paths.
pub fn make_placement(
    asset: &Asset,
    x: f64,
    y: f64,
    z: i64,
    parent_id: Option<String>,
) -> Placement {
    Placement {
        id: nanoid::nanoid!(),
        asset_id: asset.id.clone(),
        x,
        y,
        scale: default_scale(asset),
        rotation: 0.0,
        z,
        crop: None,
        parent_id,
        from_op_id: None,
    }
}

/// Top-left world anchor for the output at slot `index` (right of source,
/// top-aligned with the source; stacks downward for multiple outputs). Both the
/// generating placeholder and the final image anchor here, so swapping the
/// placeholder for the real image keeps the top-left fixed (no jump).
pub fn derived_anchor(
    source: Option<(&Placement, &Asset)>,
    index: i64,
    doc: &BoardDoc,
) -> (f64, f64) {
    match source {
        Some((s, sa)) => {
            let src_w = sa.width as f64 * s.scale;
            let src_h = sa.height as f64 * s.scale;
            let left = s.x + src_w / 2.0 + GAP;
            let top = (s.y - src_h / 2.0) + index as f64 * (src_h + GAP);
            (left, top)
        }
        None => {
            // No source selected: flow the output below existing content.
            let top = next_batch_top(doc) + index as f64 * (DEFAULT_TILE + GAP);
            (0.0, top)
        }
    }
}

/// World-space rect (center x,y + size w,h) for a generating placeholder — uses
/// the source's on-screen size as an estimate (real output may differ).
pub fn placeholder_rect(
    source: Option<(&Placement, &Asset)>,
    index: i64,
    doc: &BoardDoc,
) -> (f64, f64, f64, f64) {
    let (left, top) = derived_anchor(source, index, doc);
    let (w, h) = match source {
        Some((s, sa)) => (sa.width as f64 * s.scale, sa.height as f64 * s.scale),
        None => (DEFAULT_TILE, DEFAULT_TILE),
    };
    (left + w / 2.0, top + h / 2.0, w, h)
}

/// Place a generated Asset to the right of its source Placement (decision D6:
/// position computed at placement time, then owned by the user; lineage lives
/// in `parent_id`). Multiple outputs in one turn stack vertically.
pub fn make_derived_placement(
    asset: &Asset,
    source: Option<(&Placement, &Asset)>,
    output_index: i64,
    doc: &BoardDoc,
) -> Placement {
    let scale = default_scale(asset);
    let (left, top) = derived_anchor(source, output_index, doc);
    let x = left + asset.width as f64 * scale / 2.0;
    let y = top + asset.height as f64 * scale / 2.0;
    let z = doc.placements.iter().map(|p| p.z).max().unwrap_or(-1) + 1 + output_index;
    Placement {
        id: nanoid::nanoid!(),
        asset_id: asset.id.clone(),
        x,
        y,
        scale,
        rotation: 0.0,
        z,
        crop: None,
        parent_id: source.map(|(s, _)| s.id.clone()),
        from_op_id: None,
    }
}

/// Make the folder the source of truth (PRD §4):
/// - drop Assets/Placements whose file vanished,
/// - mint an Asset + grid Placement for each new image file,
/// - preserve existing Placements (user layout sticks).
///
/// (Content-duplicate files keyed by path may yield Assets sharing a blake3 id;
/// harmless — identical bytes render identically.)
pub fn reconcile(folder: &Path, doc: &mut BoardDoc) {
    let files = assets::scan_images(folder);
    let on_disk: HashSet<&str> = files.iter().map(|s| s.as_str()).collect();

    let removed: Vec<String> = doc
        .assets
        .iter()
        .filter(|a| !on_disk.contains(a.path.as_str()))
        .map(|a| a.id.clone())
        .collect();
    if !removed.is_empty() {
        doc.assets.retain(|a| on_disk.contains(a.path.as_str()));
        doc.placements.retain(|p| !removed.contains(&p.asset_id));
    }

    let known: HashSet<String> = doc.assets.iter().map(|a| a.path.clone()).collect();
    let mut fresh: Vec<Asset> = Vec::new();
    for name in &files {
        if known.contains(name) {
            continue;
        }
        match assets::mint_asset(folder, name, Origin::Imported) {
            Ok(asset) => fresh.push(asset),
            Err(e) => tracing::warn!(module = "board", "mint {name} failed: {e}"),
        }
    }
    if fresh.is_empty() {
        return;
    }
    let foots: Vec<(f64, f64)> = fresh.iter().map(footprint).collect();
    let centers = flow_layout(next_batch_top(doc), &foots);
    let base_z = doc.placements.len() as i64;
    for (i, asset) in fresh.into_iter().enumerate() {
        let (x, y) = centers[i];
        let placement = make_placement(&asset, x, y, base_z + i as i64, None);
        doc.assets.push(asset);
        doc.placements.push(placement);
    }
}

/// Recover `Asset.origin` for docs written before v2 (the field defaulted to
/// `imported` on load). Heuristic: an Asset whose Placement carries a `parent_id`
/// was an agent output → mark it `generated`. Imports/crops/pastes never get a
/// parent, so they correctly stay `imported`. Idempotent; cheap; forward-safe
/// (new assets already carry the right origin from mint time).
pub fn backfill_origins(doc: &mut BoardDoc) {
    let derived: HashSet<String> = doc
        .placements
        .iter()
        .filter(|p| p.parent_id.is_some())
        .map(|p| p.asset_id.clone())
        .collect();
    for a in doc.assets.iter_mut() {
        if a.origin == Origin::Imported && derived.contains(&a.id) {
            a.origin = Origin::Generated;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(w: u32, h: u32) -> Asset {
        Asset {
            id: "x".into(),
            path: "x.png".into(),
            width: w,
            height: h,
            mime: "image/png".into(),
            created_at: 0,
            origin: Origin::Imported,
        }
    }

    #[test]
    fn tier_snaps_to_nearest_power_of_two() {
        // A 1024² generation lands exactly on a tier → 1:1, no resampling.
        assert_eq!(default_scale(&asset(1024, 1024)), 1.0);
        assert_eq!(snap_tier(1024.0), 1024.0);
        // Geometric (×√2) midpoints: 512·√2 ≈ 724.1, 1024·√2 ≈ 1448.2.
        assert_eq!(snap_tier(724.0), 512.0);
        assert_eq!(snap_tier(725.0), 1024.0);
        assert_eq!(snap_tier(1447.0), 1024.0);
        assert_eq!(snap_tier(1449.0), 2048.0);
    }

    #[test]
    fn tier_clamps_both_ends() {
        assert_eq!(snap_tier(40.0), TIER_MIN); // tiny snaps up
        assert_eq!(snap_tier(4096.0), TIER_MAX); // huge caps at 2048
        assert_eq!(snap_tier(0.0), TIER_MIN); // degenerate
    }

    #[test]
    fn footprint_preserves_aspect() {
        // 4096×2048 → longest side 4096 → tier 2048 → scale 0.5.
        let (w, h) = footprint(&asset(4096, 2048));
        assert_eq!((w, h), (2048.0, 1024.0));
    }

    #[test]
    fn flow_packs_a_row_then_wraps_without_overlap() {
        // Two 480-tiles fit one row; a third 2048-wide tile forces a wrap.
        let centers = flow_layout(0.0, &[(480.0, 480.0), (480.0, 480.0), (2048.0, 2048.0)]);
        assert_eq!(centers[0].0, 240.0);
        assert_eq!(centers[1].0, 800.0); // 480 + GAP + 480/2
        assert_eq!(centers[0].1, centers[1].1); // same row
        assert!(centers[2].1 > centers[1].1); // wrapped to a new row
        assert_eq!(centers[2].0, 1024.0); // new row starts at x=0
    }
}
