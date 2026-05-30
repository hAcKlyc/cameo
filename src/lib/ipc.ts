import { invoke } from "@tauri-apps/api/core";
import type {
  AppConfig,
  CodexAuthStatus,
  CodexInfo,
  CodexSkillInfo,
  CodexSkillRef,
  Asset,
  BoardInfo,
  GenSettings,
  ImportResult,
  ModelInfo,
  Placement,
  PlacementUpdate,
  ProxyProbeResult,
  ProxySettings,
  SessionsDoc,
  Shape,
  WorkspaceEntry,
} from "../types";

export interface OverlayRef {
  placementId: string;
  path: string;
}

// Tauri v2 auto-converts camelCase JS keys → snake_case Rust params.
export const ipc = {
  frontLog: (level: "info" | "warn" | "error", msg: string) =>
    invoke<void>("front_log", { level, msg }),

  /** Proxied transport for the cloud API (gallery + telemetry). Routes through
   *  Rust's single proxied client so cloud traffic honors Settings → Proxy —
   *  the WebView's own fetch cannot. See services/cloud/index.ts. */
  cloudRequest: (req: {
    url: string;
    method: string;
    headers: Record<string, string>;
    body?: string;
  }) => invoke<{ status: number; body: string }>("cloud_request", { req }),

  initialBoard: () => invoke<string | null>("initial_board"),

  initialTestPrompt: () => invoke<string | null>("initial_test_prompt"),

  // Workspaces
  listWorkspaces: () => invoke<WorkspaceEntry[]>("list_workspaces"),
  createWorkspace: () => invoke<string>("create_workspace"),
  renameWorkspace: (id: string, name: string) => invoke<void>("rename_workspace", { id, name }),
  removeWorkspace: (id: string) => invoke<void>("remove_workspace", { id }),

  openBoard: (path: string) => invoke<BoardInfo>("open_board", { path }),

  importPaths: (boardId: string, paths: string[]) =>
    invoke<ImportResult>("import_paths", { boardId, paths }),

  importImageBytes: (boardId: string, bytes: number[], ext: string, stem: string) =>
    invoke<ImportResult>("import_image_bytes", { boardId, bytes, ext, stem }),

  readAssetBytes: (boardId: string, relPath: string) =>
    invoke<number[]>("read_asset_bytes", { boardId, relPath }),

  updatePlacements: (boardId: string, updates: PlacementUpdate[]) =>
    invoke<void>("update_placements", { boardId, updates }),

  deletePlacements: (boardId: string, ids: string[]) =>
    invoke<void>("delete_placements", { boardId, ids }),

  restorePlacements: (boardId: string, placements: Placement[]) =>
    invoke<void>("restore_placements", { boardId, placements }),

  replacePlacementImage: (boardId: string, placementId: string, bytes: number[], ext: string) =>
    invoke<{ asset: Asset | null; placement: Placement }>("replace_placement_image", {
      boardId,
      placementId,
      bytes,
      ext,
    }),

  // Export
  revealInFinder: (boardId: string, placementId: string) =>
    invoke<void>("reveal_in_finder", { boardId, placementId }),
  copyImage: (boardId: string, placementId: string) =>
    invoke<void>("copy_image", { boardId, placementId }),
  exportAsset: (boardId: string, placementId: string, dest: string) =>
    invoke<void>("export_asset", { boardId, placementId, dest }),
  exportAssets: (boardId: string, placementIds: string[], destDir: string) =>
    invoke<string[]>("export_assets", { boardId, placementIds, destDir }),

  renameAsset: (boardId: string, placementId: string, newName: string) =>
    invoke<string>("rename_asset", { boardId, placementId, newName }),

  // Annotations + overlay
  setAnnotation: (boardId: string, placementId: string, shapes: Shape[]) =>
    invoke<void>("set_annotation", { boardId, placementId, shapes }),
  writeOverlay: (boardId: string, bytes: number[]) =>
    invoke<string>("write_overlay", { boardId, bytes }),

  // Codex runtime
  startSession: (boardId: string) => invoke<string>("start_session", { boardId }),
  sendMessage: (
    boardId: string,
    text: string,
    sources: string[],
    overlays: OverlayRef[],
    skills: CodexSkillRef[] = [],
  ) =>
    invoke<void>("send_message", { boardId, text, sources, overlays, skills }),
  interruptTurn: (boardId: string) => invoke<void>("interrupt_turn", { boardId }),

  // Generation knobs (model / effort / service tier) — per-Board, sticky.
  getGenSettings: (boardId: string) => invoke<GenSettings>("get_gen_settings", { boardId }),
  setGenSettings: (boardId: string, settings: GenSettings) =>
    invoke<void>("set_gen_settings", { boardId, settings }),
  listModels: (boardId: string) => invoke<ModelInfo[]>("list_models", { boardId }),
  listSkills: (boardId: string, forceReload = false) =>
    invoke<CodexSkillInfo[]>("list_skills", { boardId, forceReload }),

  // Sessions
  listSessions: (boardId: string) => invoke<SessionsDoc>("list_sessions", { boardId }),
  newSession: (boardId: string) => invoke<string>("new_session", { boardId }),
  switchSession: (boardId: string, sessionId: string) =>
    invoke<void>("switch_session", { boardId, sessionId }),
  renameSession: (boardId: string, sessionId: string, title: string) =>
    invoke<void>("rename_session", { boardId, sessionId, title }),
  loadSession: (boardId: string, sessionId: string) =>
    invoke<unknown[]>("load_session", { boardId, sessionId }),
  respondPermission: (boardId: string, requestId: number, accept: boolean) =>
    invoke<void>("respond_permission", { boardId, requestId, accept }),
  stopSession: (boardId: string) => invoke<void>("stop_session", { boardId }),

  // App config (global ~/.cameo/config.json) + diagnostics
  cfgLoad: () => invoke<AppConfig>("cfg_load"),
  cfgSave: (config: AppConfig) => invoke<void>("cfg_save", { config }),
  probeProxy: (protocol: ProxySettings["protocol"], host: string, port: number) =>
    invoke<ProxyProbeResult>("probe_proxy", { protocol, host, port }),
  probeCodexNetwork: () => invoke<ProxyProbeResult>("probe_codex_network"),
  /** Anonymous install identity (UUID v4). Generated on first call. */
  deviceIdGet: () => invoke<string>("device_id_get"),
  /** Wipe ~/.cameo/device_id; next launch mints a new one. */
  deviceIdReset: () => invoke<void>("device_id_reset"),
  openLogsDir: () => invoke<void>("open_logs_dir"),
  /** PNG bytes of the clipboard image, or null if none. */
  readClipboardImage: () => invoke<number[] | null>("read_clipboard_image"),
  /** Detect the local Codex CLI (path + version). */
  detectCodex: () => invoke<CodexInfo>("detect_codex"),
  /** Probe local Codex CLI auth without requiring an open Board session. */
  probeCodexAuth: () => invoke<CodexAuthStatus>("probe_codex_auth"),
  /** Open a visible terminal that installs the Codex CLI. */
  openCodexInstallTerminal: () => invoke<void>("open_codex_install_terminal"),
  /** Open a visible terminal that runs `codex login`. */
  openCodexLoginTerminal: () => invoke<void>("open_codex_login_terminal"),
  /** Open a directory in the OS file manager. */
  openDir: (path: string) => invoke<void>("open_dir", { path }),

  // ── Auto-updater (manual trigger of the otherwise-silent pipeline) ─────────
  /** Run the same check+download as the startup path, immediately. Resolves a
   *  status: "found" (newer version downloading/downloaded), "busy" (a check is
   *  already running — treat like found), or "uptodate" (nothing newer). Awaits
   *  the full download; live progress arrives via `updater:*` events. */
  checkAndDownloadUpdate: () =>
    invoke<"found" | "busy" | "uptodate">("check_and_download_update"),
  /** Apply the downloaded update (mac relaunch / win NSIS). Replaces the
   *  process on success — the returned promise resolves only on failure. */
  installPendingUpdate: () => invoke<void>("install_pending_update"),
  /** Windows: a previously-downloaded update on disk, or null. */
  checkPendingUpdate: () => invoke<string | null>("check_pending_update"),

  // ── Chat-text-embedded image references ───────────────────────────────────
  /** Classify a path string the AI emitted in chat text. Returns whether
   *  it's a real image, whether it lives in the current workspace, the
   *  workspace-relative path (for in-workspace renders via the Cameo image
   *  protocol) and a base64 thumb (for out-of-workspace renders). */
  resolveChatImage: (boardId: string, rawPath: string) =>
    invoke<ChatImageResolution>("resolve_chat_image", { boardId, rawPath }),
  /** Right-click "添加到画布" — copy outside-workspace bytes into
   *  <workspace>/imports/ then run the normal import. */
  importChatImageToCanvas: (boardId: string, absPath: string) =>
    invoke<ImportResult>("import_chat_image_to_canvas", { boardId, absPath }),
  /** Right-click "复制" — clipboard the PNG bytes from an arbitrary path. */
  copyImageFromPath: (absPath: string) =>
    invoke<void>("copy_image_from_path", { absPath }),
  /** Right-click "打开所在文件夹" — reveal an arbitrary file. */
  revealPathInFinder: (absPath: string) =>
    invoke<void>("reveal_path_in_finder", { absPath }),
};

export interface ChatImageResolution {
  absPath: string;
  exists: boolean;
  isImage: boolean;
  inWorkspace: boolean;
  workspaceRelPath: string | null;
  thumbDataUrl: string | null;
  existingPlacementId: string | null;
  error: string | null;
}

export { cameoUrl } from "./cameo-url";
