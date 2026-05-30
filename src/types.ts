// Mirrors the Rust serde shapes in src-tauri/src/model.rs (camelCase wire form).

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** How an Asset entered the Board (mirrors Rust `Origin`). Drives the on-disk
 *  naming scheme; `imported` files keep their original filename. */
export type AssetOrigin = "imported" | "generated" | "crop" | "paste";

export interface Asset {
  id: string; // blake3 hex of contents
  path: string; // relative to the Board folder
  width: number;
  height: number;
  mime: string;
  createdAt: number;
  origin: AssetOrigin;
}

export interface Placement {
  id: string;
  assetId: string;
  x: number;
  y: number;
  scale: number;
  rotation: number;
  z: number;
  crop?: Rect;
  parentId?: string;
  fromOpId?: string;
}

export type ShapeKind = "point" | "rect" | "ellipse" | "path";

export interface Shape {
  kind: ShapeKind;
  /** Asset-pixel coords, origin at the image center. A "point" has one coord
   *  (the click); rect/ellipse have two (corners); path is a polyline. */
  points: [number, number][];
  /** Stable id so a note binds to the right mark even as the array changes. */
  id?: string;
  /** Per-mark instruction the user typed in the comment box ("把这里改成…"). */
  note?: string;
}

export interface Annotation {
  placementId: string;
  shapes: Shape[];
}

export interface BoardDoc {
  version: number;
  assets: Asset[];
  placements: Placement[];
  annotations?: Annotation[];
}

export interface BoardInfo {
  id: string;
  folder: string;
  name: string;
  doc: BoardDoc;
}

export interface WorkspaceEntry {
  id: string;
  path: string;
  name: string;
  kind: "app" | "external";
  lastOpened: number;
}

export interface SessionMeta {
  id: string;
  threadId: string | null;
  title: string;
  createdAt: number;
  updatedAt: number;
}

export interface SessionsDoc {
  activeSessionId: string | null;
  sessions: SessionMeta[];
}

export interface ImportResult {
  assets: Asset[];
  placements: Placement[];
}

export interface PlacementUpdate {
  id: string;
  x: number;
  y: number;
  scale: number;
  rotation: number;
  z: number;
}

// Mirrors src-tauri/src/proxy.rs ProxySettings + config.rs AppConfig.
export interface ProxySettings {
  enabled: boolean;
  protocol: "http" | "socks5";
  host: string;
  port: number;
}

export type RuntimeProvider = "codex" | "api";

export interface ApiImageSettings {
  base_url: string;
  api_key: string;
  model: string;
  size: string;
}

export interface AppConfig {
  provider: RuntimeProvider;
  api: ApiImageSettings;
  proxy: ProxySettings;
  /** Disable anonymous usage telemetry (default false = telemetry enabled). */
  telemetry_opt_out: boolean;
  /** ISO date "YYYY-MM-DD" of the last app_open event we sent. */
  last_telemetry_date: string | null;
  /** Close window → hide to tray instead of quitting (default true). */
  close_to_tray: boolean;
}

// Mirrors src-tauri/src/proxy.rs ProxyProbeResult.
export interface ProxyProbeResult {
  ok: boolean;
  stage: string;
  kind: string;
  message: string;
  detail: string | null;
  httpStatus: number | null;
  url: string;
}

// Mirrors src-tauri/src/codex.rs CodexInfo — local Codex CLI detection.
export interface CodexInfo {
  found: boolean;
  path?: string | null;
  version?: string | null;
}

export interface CodexAuthStatus {
  authMethod?: string | null;
  requiresOpenaiAuth: boolean;
  requiresLogin: boolean;
  authSupported: boolean;
}

// Mirrors src-tauri/src/runtime.rs UnifiedEvent (tag "kind", camelCase fields).
export type CodexEvent =
  | { kind: "sessionInit"; threadId: string; model: string }
  | { kind: "textDelta"; text: string }
  | { kind: "textStop" }
  | { kind: "thinkingStart" }
  | { kind: "thinkingDelta"; text: string }
  | { kind: "thinkingStop" }
  | { kind: "toolStart"; toolUseId: string; toolName: string; detail?: string | null }
  | { kind: "toolStop"; toolUseId: string }
  | { kind: "toolResult"; toolUseId: string; content: string }
  | { kind: "generationStarted"; placeholderId: string; x: number; y: number; w: number; h: number }
  | {
      kind: "imageGenerated";
      asset: Asset;
      placement: Placement;
      caption?: string | null;
      placeholderId?: string | null;
    }
  | { kind: "permissionRequest"; requestId: number; summary: string }
  | { kind: "turnComplete"; status: string; error?: string | null }
  | { kind: "usage"; inputTokens: number; outputTokens: number }
  | { kind: "planUpdated"; explanation?: string | null; steps: { step: string; status: string }[] }
  | {
      kind: "rateLimits";
      /** Primary window = 5-hour rolling. */
      usedPercent: number;
      resetsAt?: number | null;
      /** Secondary window = weekly. */
      secondaryUsedPercent?: number | null;
      secondaryResetsAt?: number | null;
      reached?: string | null;
    }
  | { kind: "status"; state: string }
  | {
      kind: "transportStatus";
      phase: "reconnecting" | "fallback";
      attempt?: number | null;
      max?: number | null;
      message: string;
    }
  /** Fatal runtime failure. Recoverable diagnostics arrive as `log`. */
  | { kind: "error"; message: string }
  | { kind: "sessionComplete"; ok: boolean; message: string }
  | { kind: "log"; level: string; message: string };

export interface CodexEventEnvelope {
  boardId: string;
  event: CodexEvent;
}

/** Per-Board generation knobs. `null` = product default (model/effort) or the
 *  standard tier (serviceTier). Mirrors Rust `GenSettings`. */
export interface GenSettings {
  model: string | null;
  effort: string | null;
  serviceTier: string | null;
}

/** A service (speed) tier offered by a model, e.g. `{ id: "priority", name:
 *  "Fast", description: "1.5x speed, increased usage" }`. */
export interface ServiceTierInfo {
  id: string;
  name: string;
  description: string;
}

/** A model from Codex `model/list`, projected for the composer menu. Field
 *  names mirror the wire `Model` (camelCase). */
export interface ModelInfo {
  id: string;
  displayName: string;
  defaultReasoningEffort: string | null;
  supportedEfforts: string[];
  serviceTiers: ServiceTierInfo[];
  defaultServiceTier: string | null;
}

export interface SkillInputRef {
  name: string;
  path: string;
}

export interface SkillInfo extends SkillInputRef {
  description: string;
  enabled: boolean;
  scope: "user" | "repo" | "system" | "admin" | string;
  displayName: string | null;
  shortDescription: string | null;
}
