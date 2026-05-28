import type { Asset, Shape } from "../types";
import { ipc, type OverlayRef } from "./ipc";
import { useBoardStore } from "../store/board";
import { loadAssetImage } from "./asset-url";

/** Where a mark's number badge sits (the point / bbox center / path end). */
function shapeAnchor(s: Shape): [number, number] {
  const pts = s.points;
  if (s.kind === "point") return pts[0] ?? [0, 0];
  if (s.kind === "path") return pts.length ? pts[pts.length - 1] : [0, 0];
  const [a, b] = pts;
  if (a && b) return [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
  return [0, 0];
}

/**
 * Render the clean image + annotation marks to a PNG at the Asset's native
 * resolution (decision D2). Shape points are in centered Asset-pixel coords.
 * Returns PNG bytes for `write_overlay`.
 */
async function renderOverlayBytes(boardId: string, asset: Asset, shapes: Shape[]): Promise<number[]> {
  const w = asset.width || 1024;
  const h = asset.height || 1024;
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no 2d context");

  const img = await loadAssetImage(boardId, asset.path, asset.mime);
  ctx.drawImage(img, 0, 0, w, h);

  ctx.translate(w / 2, h / 2); // shapes use a center origin
  const sw = Math.max(4, Math.max(w, h) * 0.006);
  ctx.lineWidth = sw;
  ctx.strokeStyle = "rgba(229,57,53,0.95)"; // brand red (DESIGN.md §3.2)
  ctx.fillStyle = "rgba(229,57,53,0.12)";

  shapes.forEach((s, idx) => {
    const pts = s.points;
    if ((s.kind === "rect" || s.kind === "ellipse") && pts.length >= 2) {
      const [a, b] = pts;
      const x = Math.min(a[0], b[0]);
      const y = Math.min(a[1], b[1]);
      const rw = Math.abs(b[0] - a[0]);
      const rh = Math.abs(b[1] - a[1]);
      if (s.kind === "rect") {
        ctx.fillRect(x, y, rw, rh);
        ctx.strokeRect(x, y, rw, rh);
      } else {
        ctx.beginPath();
        ctx.ellipse(x + rw / 2, y + rh / 2, rw / 2, rh / 2, 0, 0, Math.PI * 2);
        ctx.fill();
        ctx.stroke();
      }
    } else if (s.kind === "path" && pts.length >= 2) {
      ctx.beginPath();
      ctx.moveTo(pts[0][0], pts[0][1]);
      for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i][0], pts[i][1]);
      ctx.save();
      ctx.lineWidth = sw * 2;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.stroke();
      ctx.restore();
    }
    // Numbered badge so the prompt's "① …" lines up with the marks.
    const [ax, ay] = shapeAnchor(s);
    const r = Math.max(14, Math.max(w, h) * 0.018);
    ctx.save();
    ctx.beginPath();
    ctx.arc(ax, ay, r, 0, Math.PI * 2);
    ctx.fillStyle = "rgba(229,57,53,1)";
    ctx.fill();
    ctx.fillStyle = "#fff";
    ctx.font = `700 ${r * 1.2}px sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(String(idx + 1), ax, ay);
    ctx.restore();
  });

  const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, "image/png"));
  if (!blob) throw new Error("toBlob failed");
  return Array.from(new Uint8Array(await blob.arrayBuffer()));
}

/** For each referenced placement that has annotations, render + write an
 *  overlay file and return the (placementId, path) refs to send. */
export async function buildOverlays(boardId: string, refs: string[]): Promise<OverlayRef[]> {
  const { annotations, placements, assets } = useBoardStore.getState();
  const out: OverlayRef[] = [];
  for (const id of refs) {
    const shapes = annotations.get(id);
    if (!shapes || shapes.length === 0) continue;
    const p = placements.get(id);
    const a = p && assets.get(p.assetId);
    if (!a) continue;
    try {
      const bytes = await renderOverlayBytes(boardId, a, shapes);
      const path = await ipc.writeOverlay(boardId, bytes);
      out.push({ placementId: id, path });
    } catch (e) {
      // Clean reference still sent; surface that the mark didn't make it.
      void ipc.frontLog("warn", `overlay render failed for ${a.path}: ${e}`);
    }
  }
  return out;
}

/** Placement ids that currently carry marks (auto-referenced on send). */
export function annotatedImages(): string[] {
  const { annotations } = useBoardStore.getState();
  return [...annotations.entries()].filter(([, s]) => s.length > 0).map(([id]) => id);
}

/** Compose per-mark notes into a formatted prompt block, one line per mark and
 *  numbered to match the overlay badges. ALWAYS English labels (agent-facing);
 *  the note text itself is whatever the user typed. Goes at the START of the
 *  message:
 *    Mark (1): make this red
 *    Mark (2): remove the cup
 *  With multiple referenced images each line is prefixed "Image N ". */
export function buildMarkNotes(refs: string[]): string {
  const { annotations } = useBoardStore.getState();
  const lines: string[] = [];
  refs.forEach((id, ri) => {
    const shapes = annotations.get(id);
    if (!shapes?.length) return;
    const label = refs.length > 1 ? `Image ${ri + 1} ` : "";
    shapes.forEach((s, i) => {
      const note = s.note?.trim();
      if (note) lines.push(`${label}Mark (${i + 1}): ${note}`);
    });
  });
  return lines.join("\n");
}
