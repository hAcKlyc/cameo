import { create } from "zustand";
import type { SceneStats } from "../canvas/scene";

/** Canvas interaction modes. The mark tools draw an annotation that points the
 *  agent at a spot/region. "point" is the default: click to drop a numbered
 *  note pin; rect/ellipse/brush drag out a region. */
export type Tool = "select" | "point" | "rect" | "ellipse" | "brush";
export type MarkShape = "point" | "rect" | "ellipse" | "brush";
export type CanvasZoomDirection = "in" | "out" | "reset";
export const MARK_SHAPES: MarkShape[] = ["point", "rect", "ellipse", "brush"];
export const isMarkTool = (t: Tool): t is MarkShape => t !== "select";
export const CHAT_PANEL_MIN_WIDTH = 380;
export const CHAT_PANEL_DEFAULT_WIDTH = 460;
export const CHAT_PANEL_MAX_WIDTH = 640;

/** Two-image comparison overlay. "slider" overlays before/after of the same
 *  scene (lineage); "side" shows two images side by side. */
export interface CompareState {
  mode: "slider" | "side";
  beforeUrl: string;
  afterUrl: string;
  beforeLabel: string;
  afterLabel: string;
}

interface UiState {
  stats: SceneStats;
  setStats: (s: SceneStats) => void;
  tool: Tool;
  setTool: (t: Tool) => void;
  compare: CompareState | null;
  setCompare: (c: CompareState | null) => void;
  /** Placement id currently in crop mode (drives the crop modal), or null. */
  cropping: string | null;
  setCropping: (id: string | null) => void;
  /** Minimap visibility (default hidden, toggled from the HUD). */
  minimapVisible: boolean;
  toggleMinimap: () => void;
  /** Imperative canvas zoom hook registered by CameoCanvas while the scene exists. */
  canvasZoom: ((direction: CanvasZoomDirection) => void) | null;
  setCanvasZoom: (handler: ((direction: CanvasZoomDirection) => void) | null) => void;
  /** Floating AI panel visibility (toggled from the topbar, like the sidebar). */
  chatOpen: boolean;
  toggleChat: () => void;
  /** Runtime-only chat width; deliberately not persisted. */
  chatWidth: number;
  setChatWidth: (width: number) => void;
}

/** Chrome-side UI state (HUD readouts, active tool). Kept separate from the
 *  Pixi scene graph so React re-renders never touch the canvas. */
export const useUiStore = create<UiState>((set) => ({
  stats: { fps: 0, zoom: 1, renderer: "?" },
  setStats: (stats) => set({ stats }),
  tool: "select",
  setTool: (tool) => set({ tool }),
  compare: null,
  setCompare: (compare) => set({ compare }),
  cropping: null,
  setCropping: (cropping) => set({ cropping }),
  minimapVisible: false,
  toggleMinimap: () => set((s) => ({ minimapVisible: !s.minimapVisible })),
  canvasZoom: null,
  setCanvasZoom: (canvasZoom) => set({ canvasZoom }),
  chatOpen: true,
  toggleChat: () => set((s) => ({ chatOpen: !s.chatOpen })),
  chatWidth: CHAT_PANEL_DEFAULT_WIDTH,
  setChatWidth: (width) =>
    set({
      chatWidth: Math.min(CHAT_PANEL_MAX_WIDTH, Math.max(CHAT_PANEL_MIN_WIDTH, width)),
    }),
}));
