import { invoke } from "@tauri-apps/api/core";
import type {
  AppConfig,
  CodexInfo,
  Asset,
  BoardInfo,
  ImportResult,
  Placement,
  PlacementUpdate,
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

  renameAsset: (boardId: string, placementId: string, newName: string) =>
    invoke<string>("rename_asset", { boardId, placementId, newName }),

  // Annotations + overlay
  setAnnotation: (boardId: string, placementId: string, shapes: Shape[]) =>
    invoke<void>("set_annotation", { boardId, placementId, shapes }),
  writeOverlay: (boardId: string, bytes: number[]) =>
    invoke<string>("write_overlay", { boardId, bytes }),

  // Codex runtime
  startSession: (boardId: string) => invoke<string>("start_session", { boardId }),
  sendMessage: (boardId: string, text: string, sources: string[], overlays: OverlayRef[]) =>
    invoke<void>("send_message", { boardId, text, sources, overlays }),
  interruptTurn: (boardId: string) => invoke<void>("interrupt_turn", { boardId }),

  // Sessions
  listSessions: (boardId: string) => invoke<SessionsDoc>("list_sessions", { boardId }),
  newSession: (boardId: string) => invoke<string>("new_session", { boardId }),
  switchSession: (boardId: string, sessionId: string) =>
    invoke<void>("switch_session", { boardId, sessionId }),
  renameSession: (boardId: string, sessionId: string, title: string) =>
    invoke<void>("rename_session", { boardId, sessionId, title }),
  loadSession: (boardId: string, sessionId: string) =>
    invoke<unknown[]>("load_session", { boardId, sessionId }),
  appendMessage: (boardId: string, sessionId: string, message: unknown) =>
    invoke<void>("append_message", { boardId, sessionId, message }),
  respondPermission: (boardId: string, requestId: number, accept: boolean) =>
    invoke<void>("respond_permission", { boardId, requestId, accept }),
  stopSession: (boardId: string) => invoke<void>("stop_session", { boardId }),

  // App config (global ~/.cameo/config.json) + diagnostics
  cfgLoad: () => invoke<AppConfig>("cfg_load"),
  cfgSave: (config: AppConfig) => invoke<void>("cfg_save", { config }),
  /** Anonymous install identity (UUID v4). Generated on first call. */
  deviceIdGet: () => invoke<string>("device_id_get"),
  /** Wipe ~/.cameo/device_id; next launch mints a new one. */
  deviceIdReset: () => invoke<void>("device_id_reset"),
  openLogsDir: () => invoke<void>("open_logs_dir"),
  /** PNG bytes of the clipboard image, or null if none. */
  readClipboardImage: () => invoke<number[] | null>("read_clipboard_image"),
  /** Detect the local Codex CLI (path + version). */
  detectCodex: () => invoke<CodexInfo>("detect_codex"),
  /** Open a directory in the OS file manager. */
  openDir: (path: string) => invoke<void>("open_dir", { path }),

  // ── Chat-text-embedded image references ───────────────────────────────────
  /** Classify a path string the AI emitted in chat text. Returns whether
   *  it's a real image, whether it lives in the current workspace, the
   *  workspace-relative path (for in-workspace renders via cameo://) and a
   *  base64 thumb (for out-of-workspace renders). */
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
  error: string | null;
}

/** Build a cross-platform `cameo://` URL for a texture fetch.
 *
 * Board id lives in the path, not the host: WebView2 represents custom
 * protocols as `http://<scheme>.localhost/...`, so host-based routing breaks
 * on Windows. Relative paths are slash-normalized before per-segment encoding.
 */
export function cameoUrl(boardId: string, relPath: string): string {
  const encBoard = encodeURIComponent(boardId);
  const normalized = relPath.replace(/\\/g, "/").replace(/^\/+/, "");
  const enc = normalized
    .split("/")
    .map((seg) => encodeURIComponent(seg))
    .join("/");
  return `cameo://localhost/${encBoard}/${enc}`;
}
