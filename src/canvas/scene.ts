import {
  Application,
  BlurFilter,
  Circle,
  Container,
  type FederatedPointerEvent,
  Graphics,
  Rectangle,
  Sprite,
  Text,
  Texture,
} from "pixi.js";
import type { Asset, Placement, PlacementUpdate, Shape, ShapeKind } from "../types";
import { cameoUrl, ipc } from "../lib/ipc";

export type Tool = "select" | "point" | "rect" | "ellipse" | "brush";

export interface SceneStats {
  fps: number;
  zoom: number;
  renderer: string;
}

/** Right-click target reported to React: an image (with viewport x/y for the
 *  menu) or empty canvas (with the world coords for pasting at the cursor). */
export type CanvasContextTarget =
  | { kind: "image"; placementId: string; x: number; y: number }
  | { kind: "canvas"; worldX: number; worldY: number; x: number; y: number };

export interface SceneCallbacks {
  onStats?: (s: SceneStats) => void;
  onSelectionChange?: (ids: string[]) => void;
  /** Native right-click on the canvas → show a custom context menu. */
  onContextMenu?: (t: CanvasContextTarget) => void;
  onCommitMoves?: (updates: PlacementUpdate[]) => void;
  onAnnotate?: (placementId: string, shapes: Shape[]) => void;
  onRename?: (placementId: string, newName: string) => void;
  /** Crop confirmed: rect is normalized [0,1] over the asset. */
  onCrop?: (placementId: string, rect: { x: number; y: number; w: number; h: number }) => void;
  /** Crop abandoned (clicked away) — clears ui.cropping. */
  onCropCancel?: () => void;
  /** Screen-space bbox of the single selected image (live transform); null when
   *  not exactly one is selected. Fires per frame so the floating action bar +
   *  in-place crop overlay follow drag/pan/zoom. */
  onSelRect?: (rect: { x: number; y: number; w: number; h: number } | null) => void;
}

interface Camera {
  x: number;
  y: number;
  scale: number;
}

interface PlaceholderNode {
  node: Container;
  blobs: Graphics[];
  w: number;
  h: number;
  seed: number;
}

interface Node {
  container: Container;
  placeholder: Graphics;
  outline: Graphics;
  /** Persisted annotation marks (drawn from the annotations map). */
  anno: Graphics;
  /** Number badges + note text for each mark. */
  noteLayer: Container;
  content: Sprite | null;
  assetId: string;
  w: number;
  h: number;
}

const MIN_SCALE = 0.02;
const MAX_SCALE = 12;
// Light + red design system (DESIGN.md). Accent = brand-500; HALO = white ring
// drawn under the accent so selection/marks read on ANY underlying image (§3.6).
const ACCENT = 0xe53935;
const HALO = 0xffffff;
const CANVAS_BG = "#F5F5F7";

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

let shapeSeq = 0;
const makeShapeId = (): string => `m${Date.now().toString(36)}${(shapeSeq++).toString(36)}`;

/** Decode off the main thread and wrap as a PixiJS texture. */
async function loadTexture(url: string): Promise<Texture> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`fetch ${url} → ${res.status}`);
  const blob = await res.blob();
  const bitmap = await createImageBitmap(blob);
  return Texture.from(bitmap);
}

/**
 * The PixiJS canvas controller — imperative GPU layer. React pushes data in via
 * `setData`/`setSelection` and receives gestures back through callbacks; it
 * never touches the scene graph directly (Figma's split).
 */
export class CanvasScene {
  readonly app: Application;
  private readonly world = new Container();
  private readonly gridLayer = new Graphics();
  private readonly placementLayer = new Container();
  private readonly placeholderLayer = new Container();
  private readonly marqueeGfx = new Graphics();
  private readonly minimapGfx = new Graphics();
  private placeholderNodes = new Map<string, PlaceholderNode>();
  private clock = 0;

  private cb: SceneCallbacks = {};
  private cam: Camera = { x: 0, y: 0, scale: 1 };

  private boardId: string | null = null;
  private placements = new Map<string, Placement>();
  private assets = new Map<string, Asset>();
  private annotations = new Map<string, Shape[]>();
  private nodes = new Map<string, Node>();
  private selected = new Set<string>();
  private tool: Tool = "select";

  // Interaction state.
  private mode:
    | "idle"
    | "pan"
    | "drag"
    | "marquee"
    | "annotate"
    | "markdrag"
    | "resize"
    | "rotate"
    | "crop" = "idle";
  private cropId: string | null = null;
  /** True while the in-place crop overlay is open — hides the transform chrome. */
  private cropActive = false;
  // Transform-handle layer (screen space) over the single selected node.
  private readonly handleLayer = new Container();
  private cornerHandles: Graphics[] = [];
  private rotateHandle: Graphics | null = null;
  private xform = { id: "", cx: 0, cy: 0, startScale: 1, startRot: 0, startDist: 1, startAngle: 0 };
  private lastPointer = { x: 0, y: 0 };
  private dragStart = { x: 0, y: 0 };
  private dragOrigin = new Map<string, { x: number; y: number }>();
  private moved = false;
  private marqueeStart = { x: 0, y: 0 };
  private marqueeAdditive = false;
  // Screen-space padding kept clear of the floating chrome (topbar / toolbar /
  // sidebar / AI panel) so fit + reveal land content in the VISIBLE area.
  private safeInset = { left: 0, right: 0, top: 0, bottom: 0 };
  // Annotation drawing state.
  private annoNodeId: string | null = null;
  private annoShape: ShapeKind = "rect";
  private annoStartLocal = { x: 0, y: 0 };
  private annoEndLocal = { x: 0, y: 0 };
  private annoPath: [number, number][] = []; // brush points (kind "path")
  private annoTemp: Graphics | null = null;
  // Dragging an existing mark (click its region → move it; pure click → edit).
  private markDrag: {
    pid: string;
    sid: string;
    startGlobal: { x: number; y: number };
    startLocal: { x: number; y: number };
    startPoints: [number, number][];
    currentPoints: [number, number][];
    moved: boolean;
  } | null = null;

  private statAccum = 0;
  private minimapAccum = 0;
  private minimapVisible = false;
  private destroyed = false;
  private inited = false;
  private canvasEl: HTMLCanvasElement | null = null;
  private hostEl: HTMLElement | null = null;
  private resizeObs: ResizeObserver | null = null;
  // Filename title over the single selected image (DOM overlay; editable).
  private titleEl: HTMLDivElement | null = null;
  private editingTitle = false;
  // Comment-pin input bound to a just-drawn mark (DOM overlay).
  private commentEl: HTMLTextAreaElement | null = null;
  private commentTarget: { placementId: string; shapeId: string } | null = null;
  private commentAnchor = { x: 0, y: 0 };
  // Localized strings for the imperative DOM overlays (set from React via i18n).
  private strings = { markComment: "What should change here…", renameHint: "Double-click to rename" };

  /** Update the canvas DOM-overlay strings when the UI language changes. */
  setStrings(s: { markComment: string; renameHint: string }): void {
    this.strings = s;
    if (this.commentEl) this.commentEl.placeholder = s.markComment;
    if (this.titleEl) this.titleEl.title = s.renameHint;
  }

  constructor() {
    this.app = new Application();
  }

  async init(host: HTMLElement, cb: SceneCallbacks): Promise<void> {
    this.cb = cb;
    await this.app.init({
      resizeTo: host,
      background: CANVAS_BG,
      antialias: true,
      autoDensity: true,
      resolution: window.devicePixelRatio || 1,
      // WebGL2 baseline (WKWebView has no WebGPU); validated by SP-3.
      preference: "webgl",
    });
    if (this.destroyed) {
      this.app.destroy(true, { children: true });
      return;
    }

    this.inited = true;
    this.canvasEl = this.app.canvas as HTMLCanvasElement;
    this.hostEl = host;
    host.appendChild(this.canvasEl);
    this.createTitleEl(host);
    this.createCommentEl(host);

    // Pixi's resizeTo only listens to window 'resize', so it misses element
    // size changes — initial flex layout settling, sidebar toggle, chat panel.
    // A ResizeObserver re-fits the renderer to the host whenever it changes,
    // so the canvas never overflows into the chat column.
    this.resizeObs = new ResizeObserver(() => {
      if (this.inited && !this.destroyed) this.app.resize();
    });
    this.resizeObs.observe(host);

    this.world.addChild(this.gridLayer);
    this.placementLayer.sortableChildren = true;
    this.world.addChild(this.placementLayer);
    this.world.addChild(this.placeholderLayer); // loading shimmers, above placements
    this.app.stage.addChild(this.world);
    this.app.stage.addChild(this.marqueeGfx); // screen-space overlay
    this.minimapGfx.eventMode = "none";
    this.app.stage.addChild(this.minimapGfx); // overview, bottom-right (screen space)
    this.app.stage.addChild(this.handleLayer); // transform handles — topmost
    this.createHandles();

    this.gridLayer.eventMode = "none";
    this.drawGrid();

    this.cam = { x: host.clientWidth / 2, y: host.clientHeight / 2, scale: 1 };
    this.applyCamera();
    this.bindInput();

    this.app.ticker.add((ticker) => {
      this.clock += ticker.deltaMS;
      if (this.placeholderNodes.size > 0) this.animatePlaceholders();
      this.updateTitle();
      this.updateComment();
      this.updateHandles();
      this.minimapAccum += ticker.deltaMS;
      if (this.minimapAccum >= 100) {
        this.minimapAccum = 0;
        this.drawMinimap();
      }
      this.statAccum += ticker.deltaMS;
      if (this.statAccum >= 400) {
        this.statAccum = 0;
        this.cb.onStats?.({
          fps: Math.round(this.app.ticker.FPS),
          zoom: this.cam.scale,
          renderer: this.rendererLabel(),
        });
      }
    });
  }

  // ── Data sync (called by React on store changes) ──────────────────────────

  setData(
    boardId: string | null,
    placements: Map<string, Placement>,
    assets: Map<string, Asset>,
    annotations: Map<string, Shape[]>,
    placeholders: Map<string, { x: number; y: number; w: number; h: number }>
  ): void {
    this.boardId = boardId;
    this.placements = placements;
    this.assets = assets;
    this.annotations = annotations;

    // Remove nodes whose placement is gone.
    for (const [id, node] of this.nodes) {
      if (!placements.has(id)) {
        this.destroyNode(node);
        this.nodes.delete(id);
        this.selected.delete(id);
      }
    }
    // Create / update.
    for (const p of placements.values()) {
      const existing = this.nodes.get(p.id);
      if (existing) {
        if (existing.assetId !== p.assetId) {
          // Asset swapped (e.g. crop bake / undo) — rebuild to load the new texture.
          this.destroyNode(existing);
          this.nodes.delete(p.id);
          this.createNode(p);
        } else {
          this.applyTransform(existing, p);
        }
      } else {
        this.createNode(p);
      }
    }
    // Render persisted annotation marks.
    for (const [id, node] of this.nodes) {
      this.drawAnnotation(id, node, annotations.get(id) ?? []);
    }

    // Sync loading placeholders.
    for (const [id, ph] of this.placeholderNodes) {
      if (!placeholders.has(id)) {
        ph.node.destroy({ children: true });
        this.placeholderNodes.delete(id);
      }
    }
    for (const [id, rect] of placeholders) {
      if (!this.placeholderNodes.has(id)) this.createPlaceholder(id, rect);
    }
  }

  setSelection(ids: string[]): void {
    this.selected = new Set(ids);
    this.refreshOutlines();
  }

  setTool(tool: Tool): void {
    this.tool = tool;
    if (this.canvasEl && !this.cropId) this.canvasEl.style.cursor = tool === "select" ? "default" : "crosshair";
    // Images show "move" only in select mode; a mark tool shows the draw cursor.
    const nodeCursor = tool === "select" ? "move" : "crosshair";
    for (const n of this.nodes.values()) n.container.cursor = nodeCursor;
  }

  /** Enter/leave crop mode for a placement (driven by ui.cropping). */
  enterCrop(id: string): void {
    this.cropId = id;
    if (this.canvasEl) this.canvasEl.style.cursor = "crosshair";
  }
  exitCrop(): void {
    this.cropId = null;
    if (this.annoTemp) {
      this.annoTemp.destroy();
      this.annoTemp = null;
    }
    if (this.mode === "crop") this.mode = "idle";
    if (this.canvasEl) this.canvasEl.style.cursor = this.tool === "select" ? "default" : "crosshair";
  }

  /** Padding (px) kept clear of the floating chrome; fit/reveal center content
   *  inside the remaining visible rect. */
  setSafeInsets(i: Partial<{ left: number; right: number; top: number; bottom: number }>): void {
    this.safeInset = { ...this.safeInset, ...i };
  }

  /** The visible viewport rect (full viewport minus the safe insets). */
  private safeRect(): { cx: number; cy: number; w: number; h: number } {
    const vw = this.hostEl?.clientWidth || this.app.screen.width;
    const vh = this.hostEl?.clientHeight || this.app.screen.height;
    const w = Math.max(1, vw - this.safeInset.left - this.safeInset.right);
    const h = Math.max(1, vh - this.safeInset.top - this.safeInset.bottom);
    return { cx: this.safeInset.left + w / 2, cy: this.safeInset.top + h / 2, w, h };
  }

  /** Center the viewport on a placement (fit to a comfortable size). */
  focusPlacement(id: string): void {
    const p = this.placements.get(id);
    const node = this.nodes.get(id);
    if (!p || !node) return;
    const s = this.safeRect();
    // Fit the image to ~55% of the smaller visible dimension.
    const imgW = node.w * p.scale;
    const imgH = node.h * p.scale;
    const fit = (Math.min(s.w, s.h) * 0.55) / Math.max(imgW, imgH, 1);
    this.cam.scale = clamp(fit, MIN_SCALE, MAX_SCALE);
    this.cam.x = s.cx - p.x * this.cam.scale;
    this.cam.y = s.cy - p.y * this.cam.scale;
    this.applyCamera();
  }

  /** Axis-aligned world bounds of the given placements (rotated corners). */
  private boundsOf(ids: string[]): { minX: number; minY: number; maxX: number; maxY: number } | null {
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    let any = false;
    for (const id of ids) {
      const p = this.placements.get(id);
      const node = this.nodes.get(id);
      if (!p || !node) continue;
      const hw = (node.w * p.scale) / 2;
      const hh = (node.h * p.scale) / 2;
      const cos = Math.cos(p.rotation);
      const sin = Math.sin(p.rotation);
      for (const [lx, ly] of [
        [-hw, -hh],
        [hw, -hh],
        [hw, hh],
        [-hw, hh],
      ]) {
        const wx = p.x + lx * cos - ly * sin;
        const wy = p.y + lx * sin + ly * cos;
        minX = Math.min(minX, wx);
        minY = Math.min(minY, wy);
        maxX = Math.max(maxX, wx);
        maxY = Math.max(maxY, wy);
        any = true;
      }
    }
    return any ? { minX, minY, maxX, maxY } : null;
  }

  private fitBounds(b: { minX: number; minY: number; maxX: number; maxY: number }): void {
    const s = this.safeRect();
    const bw = Math.max(1, b.maxX - b.minX);
    const bh = Math.max(1, b.maxY - b.minY);
    this.cam.scale = clamp(Math.min(s.w / bw, s.h / bh) * 0.88, MIN_SCALE, MAX_SCALE);
    const cx = (b.minX + b.maxX) / 2;
    const cy = (b.minY + b.maxY) / 2;
    this.cam.x = s.cx - cx * this.cam.scale;
    this.cam.y = s.cy - cy * this.cam.scale;
    this.applyCamera();
  }

  /** Frame all placements (⇧1). */
  fitAll(): void {
    const b = this.boundsOf([...this.nodes.keys()]);
    if (b) this.fitBounds(b);
  }

  /** Frame the current selection, or all if nothing selected (⇧2). */
  fitSelection(): void {
    const ids = this.selected.size ? [...this.selected] : [...this.nodes.keys()];
    const b = this.boundsOf(ids);
    if (b) this.fitBounds(b);
  }

  /** Reset to 100%, centered on the content (⌘0). */
  resetZoom(): void {
    const s = this.safeRect();
    const b = this.boundsOf([...this.nodes.keys()]);
    const cx = b ? (b.minX + b.maxX) / 2 : 0;
    const cy = b ? (b.minY + b.maxY) / 2 : 0;
    this.cam.scale = 1;
    this.cam.x = s.cx - cx;
    this.cam.y = s.cy - cy;
    this.applyCamera();
  }

  /** Bottom-right overview: all placements + the current viewport rect. */
  private drawMinimap(): void {
    const g = this.minimapGfx;
    g.clear();
    if (!this.minimapVisible || this.nodes.size === 0) return;
    const vw = this.hostEl?.clientWidth || this.app.screen.width;
    const vh = this.hostEl?.clientHeight || this.app.screen.height;
    const b = this.boundsOf([...this.nodes.keys()]);
    if (!b) return;
    // Always include the viewport so the box reflects where you are.
    const tl = this.screenToWorld(0, 0);
    const br = this.screenToWorld(vw, vh);
    const minX = Math.min(b.minX, tl.x);
    const minY = Math.min(b.minY, tl.y);
    const maxX = Math.max(b.maxX, br.x);
    const maxY = Math.max(b.maxY, br.y);
    const wW = Math.max(1, maxX - minX);
    const wH = Math.max(1, maxY - minY);
    const mW = 168;
    const mH = 120;
    const pad = 10;
    const mx = 14; // bottom-left, above the HUD bar
    const my = vh - mH - 52;
    const s = Math.min((mW - pad * 2) / wW, (mH - pad * 2) / wH);
    const ox = mx + (mW - wW * s) / 2;
    const oy = my + (mH - wH * s) / 2;
    const toMini = (wx: number, wy: number): [number, number] => [ox + (wx - minX) * s, oy + (wy - minY) * s];
    g.roundRect(mx, my, mW, mH, 8).fill({ color: 0xffffff, alpha: 0.92 }).stroke({ width: 1, color: 0xe5e7eb });
    for (const [id, node] of this.nodes) {
      const p = this.placements.get(id);
      if (!p) continue;
      const [rx, ry] = toMini(p.x - (node.w * p.scale) / 2, p.y - (node.h * p.scale) / 2);
      g.rect(rx, ry, Math.max(1, node.w * p.scale * s), Math.max(1, node.h * p.scale * s)).fill({
        color: this.selected.has(id) ? ACCENT : 0xc8c8ce,
        alpha: 0.95,
      });
    }
    const [vx, vy] = toMini(tl.x, tl.y);
    const [vx2, vy2] = toMini(br.x, br.y);
    g.rect(vx, vy, vx2 - vx, vy2 - vy).stroke({ width: 1.5, color: 0x1a1a1c, alpha: 0.5 });
  }

  // ── Node lifecycle ─────────────────────────────────────────────────────────

  private createNode(p: Placement): void {
    const asset = this.assets.get(p.assetId);
    const w = asset?.width || 480;
    const h = asset?.height || 480;

    const container = new Container();
    container.eventMode = "static";
    container.cursor = this.tool === "select" ? "move" : "crosshair";

    const placeholder = new Graphics();
    placeholder.rect(-w / 2, -h / 2, w, h).fill({ color: 0xe8e8ea });
    container.addChild(placeholder);

    const anno = new Graphics();
    container.addChild(anno);

    const noteLayer = new Container();
    container.addChild(noteLayer);

    const outline = new Graphics();
    container.addChild(outline);

    const node: Node = { container, placeholder, outline, anno, noteLayer, content: null, assetId: p.assetId, w, h };
    container.on("pointerdown", (e: FederatedPointerEvent) => this.onNodePointerDown(p.id, e));

    this.applyTransform(node, p);
    this.drawOutline(node, false);
    this.placementLayer.addChild(container);
    this.nodes.set(p.id, node);

    if (this.boardId && asset) {
      const url = cameoUrl(this.boardId, asset.path);
      loadTexture(url)
        .then((tex) => {
          const current = this.nodes.get(p.id);
          if (this.destroyed || current !== node || current.assetId !== p.assetId) {
            tex.destroy(true);
            return;
          }
          const sprite = new Sprite(tex);
          sprite.anchor.set(0.5);
          node.content = sprite;
          container.addChildAt(sprite, 1); // above placeholder, below outline
          node.placeholder.visible = false;
          // Correct dims to the true texture size.
          node.w = tex.width;
          node.h = tex.height;
          this.drawOutline(node, this.selected.has(p.id));
          this.drawAnnotation(p.id, node, this.annotations.get(p.id) ?? []);
        })
        .catch((err) => {
          console.error("texture load failed", url, err);
          void ipc.frontLog("error", `texture load failed ${asset.path}: ${err}`);
        });
    }
  }

  private destroyNode(node: Node): void {
    node.content?.destroy({ texture: true, textureSource: true });
    node.container.destroy({ children: true });
  }

  private applyTransform(node: Node, p: Placement): void {
    node.container.position.set(p.x, p.y);
    node.container.scale.set(p.scale);
    node.container.rotation = p.rotation;
    node.container.zIndex = p.z;
  }

  private drawOutline(node: Node, selected: boolean): void {
    const g = node.outline;
    g.clear();
    const x = -node.w / 2;
    const y = -node.h / 2;
    const w = node.w;
    const h = node.h;
    if (selected) {
      // A thin crisp brand-red edge tight on the image, with a small soft glow
      // bleeding OUTWARD onto the canvas. alignment:0 = stroke band sits outside
      // the rect (PixiJS v8); the wide glow is drawn first, the crisp line on top
      // so the line hugs the edge and the glow shows beyond it. Width is
      // proportional to the image but small and capped so big images stay thin.
      const b = Math.min(6, Math.max(1.5, Math.max(node.w, node.h) * 0.0022));
      g.rect(x, y, w, h).stroke({ width: b * 3.5, color: ACCENT, alpha: 0.15, alignment: 0 });
      g.rect(x, y, w, h).stroke({ width: b, color: ACCENT, alpha: 1, alignment: 0 });
    } else {
      // Subtle dark hairline so light/white images separate from the light canvas.
      g.rect(x, y, w, h).stroke({
        width: Math.max(1, Math.max(node.w, node.h) * 0.0016),
        color: 0x1a1a1c,
        alpha: 0.1,
        alignment: 1,
      });
    }
  }

  private refreshOutlines(): void {
    for (const [id, node] of this.nodes) {
      this.drawOutline(node, !this.cropActive && this.selected.has(id));
    }
  }

  /** Toggle the in-place crop overlay: hides the selection outline + transform
   *  handles on the cropped image so only the crop frame is adjustable. */
  setCropActive(active: boolean): void {
    this.cropActive = active;
    this.refreshOutlines();
  }

  setMinimapVisible(v: boolean): void {
    this.minimapVisible = v;
  }

  // ── Annotations ───────────────────────────────────────────────────────────

  private drawAnnotation(placementId: string, node: Node, shapes: Shape[]): void {
    const g = node.anno;
    g.clear();
    node.noteLayer.removeChildren().forEach((c) => c.destroy({ children: true }));
    const sw = Math.max(4, Math.max(node.w, node.h) * 0.006);
    const fs = Math.max(30, Math.max(node.w, node.h) * 0.04); // bigger badge/number
    shapes.forEach((s, i) => {
      this.drawShape(g, s, sw);
      const [x0, y0, x1, y1] = this.shapeBBox(s);
      const r = fs * 0.72;
      // Point pins center the badge on the click; region marks sit at the corner.
      const bx = s.kind === "point" ? x0 : x0 + r;
      const by = s.kind === "point" ? y0 : y0 + r;
      const badge = new Graphics();
      badge.circle(bx, by, r).fill({ color: ACCENT }).stroke({ width: Math.max(2, r * 0.12), color: HALO, alpha: 0.95 });
      const num = new Text({ text: String(i + 1), style: { fontSize: fs, fill: 0xffffff, fontWeight: "700" } });
      num.anchor.set(0.5);
      num.position.set(bx, by);
      const handle = new Container();
      handle.addChild(badge, num);
      if (s.id) {
        // Clicking anywhere in the mark region grabs it: drag to move, or a pure
        // click (no drag) opens its note for inline editing.
        handle.eventMode = "static";
        handle.cursor = "move";
        handle.hitArea =
          s.kind === "point"
            ? new Rectangle(bx - r, by - r, r * 2, r * 2)
            : new Rectangle(x0, y0, Math.max(1, x1 - x0), Math.max(1, y1 - y0));
        const sid = s.id;
        const startPoints = s.points.map(([px, py]) => [px, py] as [number, number]);
        handle.on("pointerdown", (e: FederatedPointerEvent) => {
          // Only interactive in select mode; in a mark tool, let the event fall
          // through so you can draw a new mark over an existing one.
          if (this.tool !== "select") return;
          if (e.button !== 0) return;
          e.stopPropagation();
          const local = node.container.toLocal(e.global);
          this.markDrag = {
            pid: placementId,
            sid,
            startGlobal: { x: e.global.x, y: e.global.y },
            startLocal: { x: local.x, y: local.y },
            startPoints,
            currentPoints: startPoints,
            moved: false,
          };
          this.mode = "markdrag";
        });
      }
      node.noteLayer.addChild(handle);
      // The note text is NOT drawn on the image — it lives in the "本轮标注"
      // panel and pops up (inline-editable) when you click the mark.
    });
  }

  /** Bounding box of a mark in local coords. */
  private shapeBBox(s: Shape): [number, number, number, number] {
    const pts = s.points;
    if (s.kind === "point" && pts[0]) {
      const [x, y] = pts[0];
      return [x, y, x, y];
    }
    if (s.kind === "path" && pts.length) {
      let mnx = Infinity;
      let mny = Infinity;
      let mxx = -Infinity;
      let mxy = -Infinity;
      for (const [x, y] of pts) {
        mnx = Math.min(mnx, x);
        mny = Math.min(mny, y);
        mxx = Math.max(mxx, x);
        mxy = Math.max(mxy, y);
      }
      return [mnx, mny, mxx, mxy];
    }
    const [a, b] = pts;
    if (a && b) return [Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.max(a[0], b[0]), Math.max(a[1], b[1])];
    return [0, 0, 0, 0];
  }

  private drawShape(g: Graphics, s: Shape, sw: number): void {
    const pts = s.points;
    // Each stroke is laid down twice: a wider white HALO first, then the brand-red
    // edge on top — so marks read on any underlying image (DESIGN.md §3.6). The
    // geometry is re-issued per pass because .stroke() consumes the current path.
    const halo = sw * 1.9;
    if ((s.kind === "rect" || s.kind === "ellipse") && pts.length >= 2) {
      const [a, b] = [pts[0], pts[1]];
      const x = Math.min(a[0], b[0]);
      const y = Math.min(a[1], b[1]);
      const w = Math.abs(b[0] - a[0]);
      const h = Math.abs(b[1] - a[1]);
      const shape = () =>
        s.kind === "rect" ? g.rect(x, y, w, h) : g.ellipse(x + w / 2, y + h / 2, w / 2, h / 2);
      shape().fill({ color: ACCENT, alpha: 0.12 });
      shape().stroke({ width: halo, color: HALO, alpha: 0.85 });
      shape().stroke({ width: sw, color: ACCENT, alpha: 0.98 });
    } else if (s.kind === "path" && pts.length >= 2) {
      const stroke = () => {
        g.moveTo(pts[0][0], pts[0][1]);
        for (let i = 1; i < pts.length; i++) g.lineTo(pts[i][0], pts[i][1]);
        return g;
      };
      stroke().stroke({ width: sw * 2 + halo, color: HALO, alpha: 0.8, cap: "round", join: "round" });
      stroke().stroke({ width: sw * 2, color: ACCENT, alpha: 0.92, cap: "round", join: "round" });
    }
  }

  /** The shape currently being drawn, from interaction state. */
  private currentAnnoShape(): Shape {
    if (this.annoShape === "path") return { kind: "path", points: this.annoPath.slice() };
    return {
      kind: this.annoShape,
      points: [
        [this.annoStartLocal.x, this.annoStartLocal.y],
        [this.annoEndLocal.x, this.annoEndLocal.y],
      ],
    };
  }

  private annoShapeValid(s: Shape): boolean {
    if (s.kind === "point") return s.points.length >= 1;
    if (s.kind === "path") return s.points.length > 2;
    const [a, b] = s.points;
    if (!a || !b) return false;
    return Math.abs(b[0] - a[0]) > 4 && Math.abs(b[1] - a[1]) > 4;
  }

  /** Kept-region bbox (clamped to the image) in local coords. */
  private cropBox(node: Node): [number, number, number, number] {
    const W = node.w;
    const H = node.h;
    const kx = clamp(Math.min(this.annoStartLocal.x, this.annoEndLocal.x), -W / 2, W / 2);
    const ky = clamp(Math.min(this.annoStartLocal.y, this.annoEndLocal.y), -H / 2, H / 2);
    const kx2 = clamp(Math.max(this.annoStartLocal.x, this.annoEndLocal.x), -W / 2, W / 2);
    const ky2 = clamp(Math.max(this.annoStartLocal.y, this.annoEndLocal.y), -H / 2, H / 2);
    return [kx, ky, kx2, ky2];
  }

  private drawCropRect(node: Node, g: Graphics): void {
    const W = node.w;
    const H = node.h;
    const [kx, ky, kx2, ky2] = this.cropBox(node);
    g.clear();
    const dim = { color: 0x000000, alpha: 0.5 };
    g.rect(-W / 2, -H / 2, W, ky + H / 2).fill(dim); // top
    g.rect(-W / 2, ky2, W, H / 2 - ky2).fill(dim); // bottom
    g.rect(-W / 2, ky, kx + W / 2, ky2 - ky).fill(dim); // left
    g.rect(kx2, ky, W / 2 - kx2, ky2 - ky).fill(dim); // right
    const sw = Math.max(2, Math.max(W, H) * 0.004);
    g.rect(kx, ky, kx2 - kx, ky2 - ky).stroke({ width: sw, color: 0xffffff, alpha: 0.95 });
  }

  private cropNormRect(node: Node): { x: number; y: number; w: number; h: number } {
    const W = node.w;
    const H = node.h;
    const [kx, ky, kx2, ky2] = this.cropBox(node);
    return { x: (kx + W / 2) / W, y: (ky + H / 2) / H, w: (kx2 - kx) / W, h: (ky2 - ky) / H };
  }

  // ── Generating placeholders (animated colorful blur) ──────────────────────

  private createPlaceholder(id: string, r: { x: number; y: number; w: number; h: number }): void {
    const node = new Container();
    node.position.set(r.x, r.y); // center
    const radius = 16;

    const bg = new Graphics();
    bg.roundRect(-r.w / 2, -r.h / 2, r.w, r.h, radius).fill({ color: 0xffffff });
    node.addChild(bg);

    // On-brand red-family blobs, masked to the card, heavily blurred → a soft
    // breathing wash on white that reads as "Codex is working" (light theme).
    const group = new Container();
    const colors = [0xe53935, 0xf87171, 0xfca5a5, 0xfee2e2, 0xffd9d6];
    const blobs: Graphics[] = [];
    const br = Math.max(r.w, r.h) * 0.42;
    for (let i = 0; i < colors.length; i++) {
      const g = new Graphics();
      g.circle(0, 0, br).fill({ color: colors[i], alpha: 0.6 });
      group.addChild(g);
      blobs.push(g);
    }
    group.filters = [new BlurFilter({ strength: Math.max(18, Math.min(r.w, r.h) * 0.14) })];

    const mask = new Graphics();
    mask.roundRect(-r.w / 2, -r.h / 2, r.w, r.h, radius).fill({ color: 0xffffff });
    node.addChild(mask);
    node.addChild(group);
    group.mask = mask;

    const border = new Graphics();
    border.roundRect(-r.w / 2, -r.h / 2, r.w, r.h, radius).stroke({ width: 2, color: ACCENT, alpha: 0.45 });
    node.addChild(border);

    this.placeholderLayer.addChild(node);
    this.placeholderNodes.set(id, { node, blobs, w: r.w, h: r.h, seed: Math.random() * 1000 });
  }

  private animatePlaceholders(): void {
    const t = this.clock / 1000;
    for (const ph of this.placeholderNodes.values()) {
      ph.blobs.forEach((b, i) => {
        b.position.set(
          Math.sin(t * 0.5 + ph.seed + i * 1.3) * ph.w * 0.22,
          Math.cos(t * 0.4 + ph.seed + i * 2.1) * ph.h * 0.22
        );
      });
      ph.node.alpha = 0.72 + Math.sin(t * 1.6 + ph.seed) * 0.16;
    }
  }

  // ── Filename title over the selected image (DOM overlay, editable) ────────

  private createTitleEl(host: HTMLElement): void {
    const el = document.createElement("div");
    el.className = "cm-img-title";
    el.style.display = "none";
    el.spellcheck = false;
    el.title = this.strings.renameHint;
    el.addEventListener("dblclick", () => {
      this.editingTitle = true;
      el.contentEditable = "true";
      el.classList.add("editing");
      el.focus();
      const r = document.createRange();
      r.selectNodeContents(el);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(r);
    });
    el.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        el.blur();
      } else if (e.key === "Escape") {
        e.preventDefault();
        this.editingTitle = false;
        el.contentEditable = "false";
        el.classList.remove("editing");
        el.blur();
      }
    });
    el.addEventListener("blur", () => this.commitTitleEdit());
    host.appendChild(el);
    this.titleEl = el;
  }

  private updateTitle(): void {
    const el = this.titleEl;
    if (!el) return;
    const id = this.selected.size === 1 ? [...this.selected][0] : null;
    const node = id ? this.nodes.get(id) : null;
    const p = id ? this.placements.get(id) : null;
    if (!id || !node || !p) {
      if (el.style.display !== "none") el.style.display = "none";
      this.cb.onSelRect?.(null);
      return;
    }
    // Live container transform → title + bar + crop overlay follow drag/resize.
    const cx = node.container.position.x;
    const cy = node.container.position.y;
    const sc = node.container.scale.x;
    const sw = node.w * sc * this.cam.scale;
    const sh = node.h * sc * this.cam.scale;
    const screenCx = this.cam.x + cx * this.cam.scale;
    const screenCy = this.cam.y + cy * this.cam.scale;
    // Filename title is LEFT-aligned to the image's top-left corner (the action
    // bar stays centered, positioned separately via onSelRect).
    const sx = screenCx - sw / 2;
    const sy = screenCy - sh / 2;
    this.cb.onSelRect?.({ x: screenCx - sw / 2, y: sy, w: sw, h: sh });
    if (this.editingTitle) return; // don't fight the inline editor
    const asset = this.assets.get(p.assetId);
    const name = asset ? asset.path.split(/[/\\]/).pop() ?? asset.path : "image";
    el.dataset.pid = id;
    if (el.textContent !== name) el.textContent = name;
    el.style.left = `${sx}px`;
    el.style.top = `${sy}px`;
    el.style.display = "block";
  }

  private commitTitleEdit(): void {
    const el = this.titleEl;
    if (!el || !this.editingTitle) return;
    this.editingTitle = false;
    el.contentEditable = "false";
    el.classList.remove("editing");
    const pid = el.dataset.pid;
    const name = (el.textContent || "").trim();
    if (pid && name) this.cb.onRename?.(pid, name);
  }

  // ── Comment pin: type the instruction for a just-drawn mark ────────────────

  private createCommentEl(host: HTMLElement): void {
    const el = document.createElement("textarea");
    el.className = "cm-mark-comment";
    el.rows = 2;
    el.placeholder = this.strings.markComment;
    el.spellcheck = false;
    el.style.display = "none";
    el.addEventListener("keydown", (e) => {
      e.stopPropagation(); // don't trigger canvas shortcuts while typing
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        this.commitComment();
      } else if (e.key === "Escape") {
        e.preventDefault();
        this.commentTarget = null;
        el.style.display = "none";
      }
    });
    el.addEventListener("blur", () => this.commitComment());
    host.appendChild(el);
    this.commentEl = el;
  }

  /** Open the comment box for a mark (by stable id), anchored at a local pt. */
  private openComment(placementId: string, shapeId: string, anchorLocal: { x: number; y: number }): void {
    const el = this.commentEl;
    if (!el) return;
    this.commentTarget = { placementId, shapeId };
    this.commentAnchor = anchorLocal;
    const existing = (this.annotations.get(placementId) ?? []).find((s) => s.id === shapeId);
    el.value = existing?.note ?? "";
    el.style.display = "block";
    this.updateComment();
    requestAnimationFrame(() => el.focus());
  }

  private updateComment(): void {
    const el = this.commentEl;
    const t = this.commentTarget;
    if (!el || !t) return;
    const node = this.nodes.get(t.placementId);
    if (!node) {
      el.style.display = "none";
      this.commentTarget = null;
      return;
    }
    const g = node.container.toGlobal({ x: this.commentAnchor.x, y: this.commentAnchor.y });
    el.style.left = `${g.x + 12}px`;
    el.style.top = `${g.y + 12}px`;
  }

  private commitComment(): void {
    const el = this.commentEl;
    const t = this.commentTarget;
    if (!el || !t) return;
    this.commentTarget = null;
    el.style.display = "none";
    const note = el.value.trim();
    if (!note) return;
    const current = this.annotations.get(t.placementId) ?? [];
    if (!current.some((s) => s.id === t.shapeId)) return; // mark gone (undo/delete)
    const shapes = current.map((s) => (s.id === t.shapeId ? { ...s, note } : s));
    this.cb.onAnnotate?.(t.placementId, shapes);
  }

  // ── Selection helpers ────────────────────────────────────────────────────

  private commitSelection(ids: Set<string>): void {
    this.selected = ids;
    this.refreshOutlines();
    this.cb.onSelectionChange?.([...ids]);
  }

  // ── Transform handles (resize / rotate the single selected node) ───────────

  private createHandles(): void {
    const cursors = ["nwse-resize", "nesw-resize", "nwse-resize", "nesw-resize"];
    for (let i = 0; i < 4; i++) {
      const g = new Graphics();
      g.rect(-4, -4, 8, 8).fill({ color: 0xffffff }).stroke({ width: 1.5, color: ACCENT });
      g.eventMode = "static";
      g.cursor = cursors[i];
      g.hitArea = new Rectangle(-9, -9, 18, 18);
      g.visible = false;
      g.on("pointerdown", (e: FederatedPointerEvent) => this.onHandleDown("resize", e));
      this.handleLayer.addChild(g);
      this.cornerHandles.push(g);
    }
    const r = new Graphics();
    r.circle(0, 0, 6).fill({ color: 0xffffff }).stroke({ width: 1.5, color: ACCENT });
    r.eventMode = "static";
    r.cursor = "grab";
    r.hitArea = new Circle(0, 0, 11);
    r.visible = false;
    r.on("pointerdown", (e: FederatedPointerEvent) => this.onHandleDown("rotate", e));
    this.handleLayer.addChild(r);
    this.rotateHandle = r;
  }

  /** Project the single selected node's live transform → screen-space handles. */
  private updateHandles(): void {
    const single = this.selected.size === 1 ? [...this.selected][0] : null;
    const node = single ? this.nodes.get(single) : null;
    const show = !this.cropActive && !!node && this.mode !== "marquee" && this.mode !== "annotate";
    this.handleLayer.visible = show;
    if (!show || !node) return;
    // Use the live container transform so handles track move/resize/rotate.
    const cx = node.container.position.x;
    const cy = node.container.position.y;
    const sc = node.container.scale.x;
    const rot = node.container.rotation;
    const hw = (node.w * sc) / 2;
    const hh = (node.h * sc) / 2;
    const cos = Math.cos(rot);
    const sin = Math.sin(rot);
    const toScreen = (lx: number, ly: number): [number, number] => {
      const wx = cx + lx * cos - ly * sin;
      const wy = cy + lx * sin + ly * cos;
      return [this.cam.x + wx * this.cam.scale, this.cam.y + wy * this.cam.scale];
    };
    const locals: [number, number][] = [
      [-hw, -hh],
      [hw, -hh],
      [hw, hh],
      [-hw, hh],
    ];
    for (let i = 0; i < 4; i++) {
      const [sx, sy] = toScreen(locals[i][0], locals[i][1]);
      this.cornerHandles[i].position.set(sx, sy);
      this.cornerHandles[i].visible = true;
    }
    if (this.rotateHandle) {
      const [sx, sy] = toScreen(0, -hh);
      // Offset 26px screen-space along the node's "up" (local (0,-1) rotated).
      this.rotateHandle.position.set(sx + sin * 26, sy - cos * 26);
      this.rotateHandle.visible = true;
    }
  }

  private onHandleDown(kind: "resize" | "rotate", e: FederatedPointerEvent): void {
    if (e.button !== 0) return;
    e.stopPropagation();
    const id = this.selected.size === 1 ? [...this.selected][0] : null;
    const p = id ? this.placements.get(id) : null;
    if (!id || !p) return;
    const pw = this.screenToWorld(e.global.x, e.global.y);
    this.xform = {
      id,
      cx: p.x,
      cy: p.y,
      startScale: p.scale,
      startRot: p.rotation,
      startDist: Math.max(1e-3, Math.hypot(pw.x - p.x, pw.y - p.y)),
      startAngle: Math.atan2(pw.y - p.y, pw.x - p.x),
    };
    this.mode = kind;
    this.moved = false;
  }

  // ── Pointer interaction (PixiJS federated events) ──────────────────────────

  private bindInput(): void {
    const stage = this.app.stage;
    stage.eventMode = "static";
    stage.hitArea = new Rectangle(-1e6, -1e6, 2e6, 2e6);
    stage.on("pointerdown", (e: FederatedPointerEvent) => this.onStagePointerDown(e));
    stage.on("globalpointermove", (e: FederatedPointerEvent) => this.onPointerMove(e));
    stage.on("pointerup", () => this.onPointerUp());
    stage.on("pointerupoutside", () => this.onPointerUp());

    const canvas = this.canvasEl;
    if (canvas) {
      canvas.addEventListener("wheel", this.onWheel, { passive: false });
      // Native right-click → custom context menu (suppress the browser menu).
      canvas.addEventListener("contextmenu", this.onContextMenuDom);
    }
  }

  /** Right-click: hit-test the topmost image under the cursor → image menu;
   *  empty space → canvas menu. Reported to React via onContextMenu. */
  private onContextMenuDom = (e: MouseEvent): void => {
    e.preventDefault();
    if (!this.canvasEl || this.cropActive) return;
    const rect = this.canvasEl.getBoundingClientRect();
    const worldX = (e.clientX - rect.left - this.cam.x) / this.cam.scale;
    const worldY = (e.clientY - rect.top - this.cam.y) / this.cam.scale;
    let hitId: string | null = null;
    let hitZ = -Infinity;
    for (const [id, node] of this.nodes) {
      const p = this.placements.get(id);
      if (!p) continue;
      const cx = node.container.position.x;
      const cy = node.container.position.y;
      const sc = node.container.scale.x;
      const halfW = (node.w * sc) / 2;
      const halfH = (node.h * sc) / 2;
      if (worldX >= cx - halfW && worldX <= cx + halfW && worldY >= cy - halfH && worldY <= cy + halfH) {
        if (p.z >= hitZ) {
          hitZ = p.z;
          hitId = id;
        }
      }
    }
    if (hitId) {
      if (!this.selected.has(hitId)) this.commitSelection(new Set([hitId]));
      this.cb.onContextMenu?.({ kind: "image", placementId: hitId, x: e.clientX, y: e.clientY });
    } else {
      this.cb.onContextMenu?.({ kind: "canvas", worldX, worldY, x: e.clientX, y: e.clientY });
    }
  };

  private onNodePointerDown(id: string, e: FederatedPointerEvent): void {
    if (e.button !== 0) return;
    if (this.cropActive) return; // only the crop frame is interactive while cropping
    e.stopPropagation();

    // Clicking a different image while cropping cancels crop.
    if (this.cropId && this.cropId !== id) {
      this.cb.onCropCancel?.();
      return;
    }
    // Crop mode: drag out the region to keep on this image.
    if (this.cropId === id) {
      const node = this.nodes.get(id);
      if (!node) return;
      this.mode = "crop";
      this.moved = false;
      const local = node.container.toLocal(e.global);
      this.annoStartLocal = { x: local.x, y: local.y };
      this.annoEndLocal = { x: local.x, y: local.y };
      this.annoTemp = new Graphics();
      node.container.addChild(this.annoTemp);
      return;
    }

    // Point mark (纯批注): a single click drops a numbered pin at the spot and
    // immediately opens its note box. No drag.
    if (this.tool === "point") {
      const node = this.nodes.get(id);
      if (!node) return;
      const local = node.container.toLocal(e.global);
      const shapeId = makeShapeId();
      const shape: Shape = { kind: "point", points: [[local.x, local.y]], id: shapeId };
      const existing = this.annotations.get(id) ?? [];
      this.cb.onAnnotate?.(id, [...existing, shape]);
      this.openComment(id, shapeId, { x: local.x, y: local.y });
      this.mode = "idle";
      return;
    }

    // Region mark tool: drag out an annotation (rect/ellipse/brush) on this node.
    if (this.tool !== "select") {
      const node = this.nodes.get(id);
      if (!node) return;
      this.mode = "annotate";
      this.annoNodeId = id;
      this.annoShape = this.tool === "brush" ? "path" : this.tool;
      const local = node.container.toLocal(e.global);
      this.annoStartLocal = { x: local.x, y: local.y };
      this.annoEndLocal = { x: local.x, y: local.y };
      this.annoPath = [[local.x, local.y]];
      this.annoTemp = new Graphics();
      node.container.addChild(this.annoTemp);
      return;
    }

    if (e.shiftKey) {
      const next = new Set(this.selected);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      this.commitSelection(next);
    } else if (!this.selected.has(id)) {
      this.commitSelection(new Set([id]));
    }

    // Begin drag of the whole current selection.
    this.mode = "drag";
    this.moved = false;
    this.dragStart = { x: e.global.x, y: e.global.y };
    this.dragOrigin.clear();
    for (const sid of this.selected) {
      const p = this.placements.get(sid);
      if (p) this.dragOrigin.set(sid, { x: p.x, y: p.y });
    }
  }

  private onStagePointerDown(e: FederatedPointerEvent): void {
    if (e.button !== 0) return;
    if (this.cropActive) return; // crop mode: ignore canvas marquee/deselect
    if (this.cropId) {
      this.cb.onCropCancel?.(); // click on empty space cancels crop
      return;
    }
    // Empty-space drag is a selection marquee; panning is two-finger scroll.
    this.moved = false;
    this.lastPointer = { x: e.global.x, y: e.global.y };
    this.mode = "marquee";
    this.marqueeAdditive = e.shiftKey;
    this.marqueeStart = { x: e.global.x, y: e.global.y };
  }

  private onPointerMove(e: FederatedPointerEvent): void {
    if (this.mode === "idle") return;
    const gx = e.global.x;
    const gy = e.global.y;

    if (this.mode === "drag") {
      const dx = (gx - this.dragStart.x) / this.cam.scale;
      const dy = (gy - this.dragStart.y) / this.cam.scale;
      if (Math.abs(gx - this.dragStart.x) + Math.abs(gy - this.dragStart.y) > 2) this.moved = true;
      for (const [sid, origin] of this.dragOrigin) {
        const node = this.nodes.get(sid);
        if (node) node.container.position.set(origin.x + dx, origin.y + dy);
      }
    } else if (this.mode === "resize") {
      const node = this.nodes.get(this.xform.id);
      if (node) {
        const pw = this.screenToWorld(gx, gy);
        const dist = Math.hypot(pw.x - this.xform.cx, pw.y - this.xform.cy);
        const ns = clamp(this.xform.startScale * (dist / this.xform.startDist), MIN_SCALE, MAX_SCALE);
        node.container.scale.set(ns);
        this.moved = true;
      }
    } else if (this.mode === "rotate") {
      const node = this.nodes.get(this.xform.id);
      if (node) {
        const pw = this.screenToWorld(gx, gy);
        const ang = Math.atan2(pw.y - this.xform.cy, pw.x - this.xform.cx);
        node.container.rotation = this.xform.startRot + (ang - this.xform.startAngle);
        this.moved = true;
      }
    } else if (this.mode === "marquee") {
      this.moved = true;
      this.lastPointer = { x: gx, y: gy };
      this.drawMarquee(this.marqueeStart.x, this.marqueeStart.y, gx, gy);
    } else if (this.mode === "annotate") {
      const node = this.annoNodeId ? this.nodes.get(this.annoNodeId) : null;
      if (node && this.annoTemp) {
        const local = node.container.toLocal(e.global);
        this.annoEndLocal = { x: local.x, y: local.y };
        if (this.annoShape === "path") this.annoPath.push([local.x, local.y]);
        const sw = Math.max(4, Math.max(node.w, node.h) * 0.006);
        this.annoTemp.clear();
        this.drawShape(this.annoTemp, this.currentAnnoShape(), sw);
      }
    } else if (this.mode === "markdrag" && this.markDrag) {
      const md = this.markDrag;
      const node = this.nodes.get(md.pid);
      if (node) {
        if (Math.abs(gx - md.startGlobal.x) + Math.abs(gy - md.startGlobal.y) > 3) md.moved = true;
        if (md.moved) {
          const local = node.container.toLocal(e.global);
          const dx = local.x - md.startLocal.x;
          const dy = local.y - md.startLocal.y;
          md.currentPoints = md.startPoints.map(([px, py]) => [px + dx, py + dy] as [number, number]);
          const shapes = this.annotations.get(md.pid) ?? [];
          const temp = shapes.map((sh) => (sh.id === md.sid ? { ...sh, points: md.currentPoints } : sh));
          this.drawAnnotation(md.pid, node, temp);
        }
      }
    } else if (this.mode === "crop") {
      const node = this.cropId ? this.nodes.get(this.cropId) : null;
      if (node && this.annoTemp) {
        const local = node.container.toLocal(e.global);
        this.annoEndLocal = { x: local.x, y: local.y };
        this.moved = true;
        this.drawCropRect(node, this.annoTemp);
      }
    }
  }

  private onPointerUp(): void {
    if (this.mode === "drag" && this.moved) {
      const updates: PlacementUpdate[] = [];
      for (const sid of this.dragOrigin.keys()) {
        const node = this.nodes.get(sid);
        const p = this.placements.get(sid);
        if (node && p) {
          updates.push({
            id: sid,
            x: node.container.position.x,
            y: node.container.position.y,
            scale: p.scale,
            rotation: p.rotation,
            z: p.z,
          });
        }
      }
      if (updates.length) this.cb.onCommitMoves?.(updates);
    } else if ((this.mode === "resize" || this.mode === "rotate") && this.moved) {
      const node = this.nodes.get(this.xform.id);
      const p = this.placements.get(this.xform.id);
      if (node && p) {
        this.cb.onCommitMoves?.([
          {
            id: this.xform.id,
            x: p.x,
            y: p.y,
            scale: node.container.scale.x,
            rotation: node.container.rotation,
            z: p.z,
          },
        ]);
      }
    } else if (this.mode === "marquee") {
      // A click (no drag) on empty space clears selection; a drag selects.
      if (!this.moved) this.commitSelection(new Set());
      else this.applyMarqueeSelection();
      this.marqueeGfx.clear();
    } else if (this.mode === "annotate") {
      const id = this.annoNodeId;
      if (this.annoTemp) {
        this.annoTemp.destroy();
        this.annoTemp = null;
      }
      if (id) {
        const base = this.currentAnnoShape();
        if (this.annoShapeValid(base)) {
          const shapeId = makeShapeId();
          const shape: Shape = { ...base, id: shapeId };
          const existing = this.annotations.get(id) ?? [];
          this.cb.onAnnotate?.(id, [...existing, shape]);
          // Immediately open the note box for the new mark (anchored at its
          // top-left), so you can type the instruction right away.
          const [x0, y0] = this.shapeBBox(shape);
          this.openComment(id, shapeId, { x: x0, y: y0 });
        }
      }
      this.annoNodeId = null;
    } else if (this.mode === "crop") {
      const id = this.cropId;
      if (this.annoTemp) {
        this.annoTemp.destroy();
        this.annoTemp = null;
      }
      const node = id ? this.nodes.get(id) : null;
      if (id && node && this.moved) {
        const rect = this.cropNormRect(node);
        if (rect && rect.w > 0.02 && rect.h > 0.02) this.cb.onCrop?.(id, rect);
      }
    } else if (this.mode === "markdrag" && this.markDrag) {
      const md = this.markDrag;
      const shapes = this.annotations.get(md.pid) ?? [];
      if (md.moved) {
        const next = shapes.map((sh) => (sh.id === md.sid ? { ...sh, points: md.currentPoints } : sh));
        this.cb.onAnnotate?.(md.pid, next);
      } else {
        // Pure click → open the note box for inline editing.
        const cur = shapes.find((x) => x.id === md.sid);
        const [x0, y0] = cur ? this.shapeBBox(cur) : [md.startLocal.x, md.startLocal.y];
        this.openComment(md.pid, md.sid, { x: x0, y: y0 });
      }
      this.markDrag = null;
    }
    this.mode = "idle";
  }

  private drawMarquee(x0: number, y0: number, x1: number, y1: number): void {
    const x = Math.min(x0, x1);
    const y = Math.min(y0, y1);
    const w = Math.abs(x1 - x0);
    const h = Math.abs(y1 - y0);
    this.marqueeGfx.clear();
    this.marqueeGfx
      .rect(x, y, w, h)
      .fill({ color: ACCENT, alpha: 0.1 })
      .stroke({ width: 1, color: ACCENT, alpha: 0.8 });
  }

  private applyMarqueeSelection(): void {
    // Marquee corners (screen) → world.
    const a = this.screenToWorld(this.marqueeStart.x, this.marqueeStart.y);
    const b = this.screenToWorld(this.lastPointer.x, this.lastPointer.y);
    const minX = Math.min(a.x, b.x);
    const maxX = Math.max(a.x, b.x);
    const minY = Math.min(a.y, b.y);
    const maxY = Math.max(a.y, b.y);
    const picked = new Set<string>();
    for (const [id, node] of this.nodes) {
      const p = this.placements.get(id);
      if (!p) continue;
      const hw = (node.w * p.scale) / 2;
      const hh = (node.h * p.scale) / 2;
      const intersects =
        p.x + hw >= minX && p.x - hw <= maxX && p.y + hh >= minY && p.y - hh <= maxY;
      if (intersects) picked.add(id);
    }
    if (this.marqueeAdditive) for (const id of this.selected) picked.add(id);
    this.commitSelection(picked);
  }

  // ── Camera ────────────────────────────────────────────────────────────────

  private onWheel = (e: WheelEvent): void => {
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      const factor = Math.exp(-e.deltaY * 0.01);
      this.zoomAt(e.offsetX, e.offsetY, factor);
    } else {
      this.cam.x -= e.deltaX;
      this.cam.y -= e.deltaY;
      this.applyCamera();
    }
  };

  private zoomAt(sx: number, sy: number, factor: number): void {
    const newScale = clamp(this.cam.scale * factor, MIN_SCALE, MAX_SCALE);
    const wx = (sx - this.cam.x) / this.cam.scale;
    const wy = (sy - this.cam.y) / this.cam.scale;
    this.cam.scale = newScale;
    this.cam.x = sx - wx * newScale;
    this.cam.y = sy - wy * newScale;
    this.applyCamera();
  }

  private screenToWorld(sx: number, sy: number): { x: number; y: number } {
    return { x: (sx - this.cam.x) / this.cam.scale, y: (sy - this.cam.y) / this.cam.scale };
  }

  private applyCamera(): void {
    this.world.position.set(this.cam.x, this.cam.y);
    this.world.scale.set(this.cam.scale);
  }

  private rendererLabel(): string {
    const t = (this.app.renderer as { type?: number } | undefined)?.type;
    if (t === 2) return "WebGPU";
    if (t === 1) return "WebGL2";
    return "?";
  }

  private drawGrid(): void {
    const g = this.gridLayer;
    g.clear();
    const span = 4000;
    const step = 56;
    for (let x = -span; x <= span; x += step) g.moveTo(x, -span).lineTo(x, span);
    for (let y = -span; y <= span; y += step) g.moveTo(-span, y).lineTo(span, y);
    g.stroke({ width: 1, color: 0x1a1a1c, alpha: 0.04 });
  }

  destroy(): void {
    this.destroyed = true;
    this.resizeObs?.disconnect();
    this.resizeObs = null;
    this.titleEl?.remove();
    this.titleEl = null;
    this.commentEl?.remove();
    this.commentEl = null;
    if (this.canvasEl) {
      this.canvasEl.removeEventListener("wheel", this.onWheel);
      this.canvasEl.removeEventListener("contextmenu", this.onContextMenuDom);
    }
    if (this.inited) {
      try {
        this.app.destroy(true, { children: true });
      } catch {
        /* already torn down */
      }
    }
  }
}
