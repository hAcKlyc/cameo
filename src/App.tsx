import { useEffect, useRef, useState } from "react";
import {
  PanelLeft,
  PanelRight,
  MousePointer2,
  ImagePlus,
  Map as MapIcon,
  Undo2,
  Redo2,
  ChevronDown,
  Square,
  Circle,
  MapPin,
  Brush,
  Settings as SettingsIcon,
  type LucideIcon,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { CameoCanvas } from "./canvas/CameoCanvas";
import { ChatPanel } from "./components/ChatPanel";
import { Sidebar } from "./components/Sidebar";
import { SettingsModal } from "./components/SettingsModal";
import { UpdateIndicator } from "./components/UpdateIndicator";
import { useUiStore, MARK_SHAPES, isMarkTool, type MarkShape } from "./store/ui";
import { useBoardStore } from "./store/board";
import { useChatStore } from "./store/chat";
import { useWorkspaceStore } from "./store/workspace";
import { useHistoryStore } from "./store/history";
import { useSettingsStore } from "./store/settings";
import { useT } from "./i18n/locale";
import type { MsgKey } from "./i18n/messages";
import { useFileImport } from "./lib/useFileImport";
import { useCodexEvents } from "./lib/useCodexEvents";
import { ipc } from "./lib/ipc";
import { buildOverlays } from "./lib/overlay";

const IMAGE_EXTS = ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "avif"];

async function pickImages() {
  const sel = await open({
    multiple: true,
    title: "Add images",
    filters: [{ name: "Images", extensions: IMAGE_EXTS }],
  });
  const paths = Array.isArray(sel) ? sel : sel ? [sel] : [];
  if (paths.length) void useBoardStore.getState().importFiles(paths as string[]);
}

function TopBar({ onOpenSettings }: { onOpenSettings: () => void }) {
  const folder = useBoardStore((s) => s.folder);
  const name = useBoardStore((s) => s.name);
  const count = useBoardStore((s) => s.placements.size);
  const sidebarOpen = useWorkspaceStore((s) => s.sidebarOpen);
  const chatOpen = useUiStore((s) => s.chatOpen);
  const t = useT();
  return (
    <div className="cm-topbar">
      <button
        className={`cm-topbar__toggle${sidebarOpen ? " is-active" : ""}`}
        title={t("topbar.workspaces")}
        onClick={() => useWorkspaceStore.getState().toggleSidebar()}
      >
        <PanelLeft size={16} />
      </button>
      <span className="cm-topbar__title" data-tauri-drag-region>
        Cameo
      </span>
      {name ? (
        <span className="cm-topbar__folder" title={folder ?? undefined} data-tauri-drag-region>
          {name} · {t(count === 1 ? "topbar.images_one" : "topbar.images", { count })}
        </span>
      ) : (
        <span className="cm-topbar__hint" data-tauri-drag-region>
          {t("topbar.hint")}
        </span>
      )}
      <div className="cm-topbar__spacer" data-tauri-drag-region />
      <button
        className={`cm-topbar__toggle${chatOpen ? " is-active" : ""}`}
        title={t("topbar.aiPanel")}
        onClick={() => useUiStore.getState().toggleChat()}
      >
        <PanelRight size={16} />
      </button>
      <UpdateIndicator />
      <button className="cm-topbar__toggle" title={t("topbar.settings")} onClick={onOpenSettings}>
        <SettingsIcon size={16} />
      </button>
    </div>
  );
}

const MARK_ICON: Record<MarkShape, LucideIcon> = { point: MapPin, rect: Square, ellipse: Circle, brush: Brush };
const MARK_LABEL: Record<MarkShape, MsgKey> = {
  point: "mark.point",
  rect: "mark.rect",
  ellipse: "mark.ellipse",
  brush: "mark.brush",
};

function MarkTool() {
  const tool = useUiStore((s) => s.tool);
  const setTool = useUiStore((s) => s.setTool);
  const [last, setLast] = useState<MarkShape>("point");
  const [open, setOpen] = useState(false);
  const active = isMarkTool(tool);
  const shown = active ? (tool as MarkShape) : last;
  const ShownIcon = MARK_ICON[shown];
  const t = useT();
  return (
    <div className={`cm-marktool${active ? " is-active" : ""}`} role="group">
      <span
        className="cm-marktool__icon"
        data-tip={t("tool.mark")}
        onClick={() => setTool(shown)}
      >
        <ShownIcon size={16} />
      </span>
      <span className="cm-marktool__caret" data-tip={t("tool.markType")} onClick={() => setOpen((o) => !o)}>
        <ChevronDown size={11} />
      </span>
      {open && (
        <div className="cm-marktool__menu" onMouseLeave={() => setOpen(false)}>
          {MARK_SHAPES.map((s) => {
            const Ico = MARK_ICON[s];
            return (
              <button
                key={s}
                className={`cm-marktool__opt${tool === s ? " is-active" : ""}`}
                onClick={() => {
                  setLast(s);
                  setTool(s);
                  setOpen(false);
                }}
              >
                <span className="cm-marktool__ico">
                  <Ico size={15} />
                </span>
                {t(MARK_LABEL[s])}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

function Toolbar() {
  const tool = useUiStore((s) => s.tool);
  const setTool = useUiStore((s) => s.setTool);
  const t = useT();
  return (
    <div className="cm-toolbar">
      <button
        className={`cm-toolbtn${tool === "select" ? " is-active" : ""}`}
        onClick={() => setTool("select")}
        data-tip={t("tool.select")}
        aria-label={t("tool.select")}
      >
        <MousePointer2 size={17} />
      </button>
      <MarkTool />
      <span className="cm-toolbar__sep" />
      <button className="cm-toolbtn" onClick={pickImages} data-tip={t("tool.addImage")} aria-label={t("tool.addImage")}>
        <ImagePlus size={17} />
      </button>
    </div>
  );
}

function Hud() {
  const stats = useUiStore((s) => s.stats);
  const minimapVisible = useUiStore((s) => s.minimapVisible);
  const sel = useBoardStore((s) => s.selection.size);
  const canUndo = useHistoryStore((s) => s.undoStack.length > 0);
  const canRedo = useHistoryStore((s) => s.redoStack.length > 0);
  const t = useT();
  return (
    <div className="cm-hud">
      <button
        className={`cm-hud__btn${minimapVisible ? " is-active" : ""}`}
        title={t("hud.minimap")}
        onClick={() => useUiStore.getState().toggleMinimap()}
      >
        <MapIcon size={15} />
      </button>
      <button
        className="cm-hud__btn"
        disabled={!canUndo}
        title={t("hud.undo")}
        onClick={() => useHistoryStore.getState().undo()}
      >
        <Undo2 size={15} />
      </button>
      <button
        className="cm-hud__btn"
        disabled={!canRedo}
        title={t("hud.redo")}
        onClick={() => useHistoryStore.getState().redo()}
      >
        <Redo2 size={15} />
      </button>
      {sel > 0 && <span className="cm-hud__sel">{t("hud.selected", { count: sel })}</span>}
      <span>{stats.fps} fps</span>
      <span>{Math.round(stats.zoom * 100)}%</span>
    </div>
  );
}

function EmptyState() {
  const boardId = useBoardStore((s) => s.boardId);
  const opening = useBoardStore((s) => s.opening);
  const error = useBoardStore((s) => s.error);
  const t = useT();
  if (boardId) return null;
  return (
    <div className="cm-empty">
      <div className="cm-empty__card">
        <div className="cm-empty__mark">Cameo</div>
        <p className="cm-empty__lead">{t("empty.lead")}</p>
        <button
          className="cm-btn cm-btn--primary"
          onClick={() => void useWorkspaceStore.getState().newWorkspace()}
          disabled={opening}
        >
          {opening ? t("empty.opening") : t("empty.new")}
        </button>
        <p className="cm-empty__sub">{t("empty.sub")}</p>
        {error && <p className="cm-empty__err">{error}</p>}
      </div>
    </div>
  );
}

export default function App() {
  const boardId = useBoardStore((s) => s.boardId);
  const didInit = useRef(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const chatOpen = useUiStore((s) => s.chatOpen);
  // Bumped when settings are saved → restart the active session so a new proxy
  // (injected at sidecar spawn) takes effect.
  const restartNonce = useSettingsStore((s) => s.restartNonce);
  useFileImport();
  useCodexEvents();

  // Restore the last workspace (or create a default) on launch. Guarded so
  // StrictMode's double-invoke doesn't create two default workspaces.
  useEffect(() => {
    if (didInit.current) return;
    didInit.current = true;
    void ipc.initialBoard().then((path) => {
      if (path) void useWorkspaceStore.getState().openWorkspace(path);
    });
    void useWorkspaceStore.getState().refresh();
    // Daily cloud ping (registers device on first launch, app_open after).
    // Fully gated: cloud module short-circuits when build env is unset.
    void (async () => {
      const settings = useSettingsStore.getState();
      if (!settings.loaded) await settings.load();
      const { bootDailyPing } = await import("./services/cloud/telemetry");
      await bootDailyPing(useSettingsStore.getState().config);
    })();
  }, []);

  // Start the Codex session when a Board opens; tear it down on change/exit.
  useEffect(() => {
    if (!boardId) return;
    let cancelled = false;
    const startedBoardId = boardId;
    const startedRestartNonce = restartNonce;
    const isCurrentStart = () =>
      !cancelled &&
      useBoardStore.getState().boardId === startedBoardId &&
      useSettingsStore.getState().restartNonce === startedRestartNonce;

    const chat = useChatStore.getState();
    chat.reset();
    chat.setSessionStatus("starting");
    ipc
      .startSession(startedBoardId)
      .then(async () => {
        if (!isCurrentStart()) return;
        useChatStore.getState().setSessionStatus("ready");
        // Load session list + the active session's persisted timeline.
        await useChatStore.getState().initSessions(startedBoardId, startedRestartNonce);
        if (!isCurrentStart()) return;
        // Headless e2e hook: auto-send a prompt if CAMEO_TEST_PROMPT is set.
        const testPrompt = await ipc.initialTestPrompt();
        if (!isCurrentStart()) return;
        if (testPrompt) {
          const st = useBoardStore.getState();
          const first = [...st.placements.keys()][0];
          const refs = first ? [first] : [];
          const p = first ? st.placements.get(first) : undefined;
          const a = p ? st.assets.get(p.assetId) : undefined;
          if (first && a) {
            const hw = a.width / 2;
            const hh = a.height / 2;
            st.setAnnotation(first, [
              { kind: "rect", points: [[-hw * 0.4, -hh * 0.4], [hw * 0.4, hh * 0.4]] },
            ]);
          }
          const turn = useChatStore.getState().startTurn(testPrompt, refs);
          try {
            const overlays = await buildOverlays(startedBoardId, refs);
            if (!isCurrentStart()) return;
            await ipc.sendMessage(startedBoardId, testPrompt, refs, overlays);
          } catch (e) {
            if (isCurrentStart()) {
              useChatStore.getState().failTurn(
                `Could not send test prompt: ${e instanceof Error ? e.message : String(e)}`,
                turn,
              );
            }
          }
        }
      })
      .catch((e) => {
        if (isCurrentStart()) useChatStore.getState().setSessionStatus("error", String(e));
      });
    return () => {
      cancelled = true;
      void ipc.stopSession(startedBoardId);
    };
  }, [boardId, restartNonce]);

  return (
    <div className="cm-app">
      <TopBar onOpenSettings={() => setSettingsOpen(true)} />
      <div className="cm-main">
        <Sidebar />
        <div className="cm-canvasarea">
          <CameoCanvas />
          {boardId && <Toolbar />}
          <Hud />
        </div>
        {boardId && chatOpen && <ChatPanel />}
      </div>
      <EmptyState />
      {settingsOpen && <SettingsModal onClose={() => setSettingsOpen(false)} />}
    </div>
  );
}
