import { useCallback, useEffect, useRef, useState } from "react";
import { CanvasScene, type CanvasContextTarget } from "./scene";
import { useUiStore } from "../store/ui";
import { useBoardStore } from "../store/board";
import { useHistoryStore } from "../store/history";
import { useWorkspaceStore } from "../store/workspace";
import { ipc } from "../lib/ipc";
import { SelectionBar } from "../components/SelectionBar";
import { CropOverlay } from "../components/CropOverlay";
import { CanvasContextMenu } from "../components/CanvasContextMenu";
import { useT } from "../i18n/locale";
import { useFileImport } from "../lib/useFileImport";

/**
 * Mounts the PixiJS scene and bridges it to the stores:
 * - pushes board data (placements/assets) into the scene on change,
 * - relays scene gestures (selection, moves) back into the store,
 * - handles the Delete key for the current selection.
 *
 * React renders this once; the canvas is driven imperatively.
 */
export function CameoCanvas() {
  const hostRef = useRef<HTMLDivElement>(null);
  const sceneRef = useRef<CanvasScene | null>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const cropRef = useRef<HTMLDivElement>(null);
  const [ctxMenu, setCtxMenu] = useState<CanvasContextTarget | null>(null);
  const chatOpen = useUiStore((s) => s.chatOpen);
  const chatWidth = useUiStore((s) => s.chatWidth);
  const sidebarOpen = useWorkspaceStore((s) => s.sidebarOpen);
  const t = useT();
  const markComment = t("canvas.markComment");
  const renameHint = t("canvas.renameHint");

  const importAtDropPoint = useCallback(async (paths: string[], position?: { x: number; y: number }) => {
    if (!position || !sceneRef.current) {
      await useBoardStore.getState().importFiles(paths);
      return;
    }
    await useBoardStore.getState().importFilesAt(paths, sceneRef.current.screenToWorldPoint(position.x, position.y));
  }, []);

  useFileImport({ onDrop: importAtDropPoint });

  // Scene lifecycle.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const scene = new CanvasScene();
    sceneRef.current = scene;
    let alive = true;

    void scene.init(host, {
      onStats: (s) => alive && useUiStore.getState().setStats(s),
      onSelectionChange: (ids) => alive && useBoardStore.getState().setSelection(ids),
      onCommitMoves: (u) => alive && void useBoardStore.getState().commitMoves(u),
      onAnnotate: (id, shapes) => alive && useBoardStore.getState().setAnnotation(id, shapes),
      onRename: (id, name) => alive && void useBoardStore.getState().renameAsset(id, name),
      onContextMenu: (t) => alive && setCtxMenu(t),
      onSelRect: (rect) => {
        const bar = barRef.current;
        const crop = cropRef.current;
        const cropping = useUiStore.getState().cropping;
        if (bar) {
          if (rect && !cropping) {
            bar.style.display = "flex";
            bar.style.left = `${rect.x + rect.w / 2}px`;
            bar.style.top = `${rect.y}px`;
          } else {
            bar.style.display = "none";
          }
        }
        if (crop) {
          if (rect && cropping) {
            crop.style.display = "block";
            crop.style.left = `${rect.x}px`;
            crop.style.top = `${rect.y}px`;
            crop.style.width = `${rect.w}px`;
            crop.style.height = `${rect.h}px`;
            crop.style.setProperty("--cm-crop-area-left", `${rect.imageX - rect.x}px`);
            crop.style.setProperty("--cm-crop-area-top", `${rect.imageY - rect.y}px`);
            crop.style.setProperty("--cm-crop-area-width", `${rect.imageW}px`);
            crop.style.setProperty("--cm-crop-area-height", `${rect.imageH}px`);
            crop.style.setProperty("--cm-crop-area-rotation", `${rect.rotation}rad`);
          } else {
            crop.style.display = "none";
          }
        }
      },
    });
    scene.setTool(useUiStore.getState().tool);
    useUiStore.getState().setCanvasZoom((direction) => {
      if (direction === "reset") scene.resetZoom();
      else scene.zoomStep(direction);
    });

    return () => {
      alive = false;
      useUiStore.getState().setCanvasZoom(null);
      scene.destroy();
      sceneRef.current = null;
    };
  }, []);

  // Push board data + selection into the scene on change (reference compare —
  // the store hands out fresh Maps/Sets on mutation). Pushing selection is safe:
  // scene.setSelection only redraws, it never re-fires onSelectionChange.
  useEffect(() => {
    const pushData = (s = useBoardStore.getState()) =>
      sceneRef.current?.setData(s.boardId, s.placements, s.assets, s.annotations, s.placeholders);
    const pushSel = (s = useBoardStore.getState()) =>
      sceneRef.current?.setSelection([...s.selection]);
    pushData();
    pushSel();
    const unsubBoard = useBoardStore.subscribe((state, prev) => {
      if (
        state.boardId !== prev.boardId ||
        state.placements !== prev.placements ||
        state.assets !== prev.assets ||
        state.annotations !== prev.annotations ||
        state.placeholders !== prev.placeholders
      ) {
        pushData(state);
      }
      if (state.selection !== prev.selection) pushSel(state);
      if (state.revealRequest !== prev.revealRequest && state.revealRequest) {
        sceneRef.current?.focusPlacement(state.revealRequest.id);
      }
    });
    const unsubTool = useUiStore.subscribe((state, prev) => {
      if (state.tool !== prev.tool) sceneRef.current?.setTool(state.tool);
      if (state.cropping !== prev.cropping) sceneRef.current?.setCropActive(!!state.cropping);
      if (state.minimapVisible !== prev.minimapVisible) sceneRef.current?.setMinimapVisible(state.minimapVisible);
    });
    return () => {
      unsubBoard();
      unsubTool();
    };
  }, []);

  // Delete key removes the current selection.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) {
        return;
      }
      if (e.key === "Escape" && useUiStore.getState().cropping) {
        e.preventDefault();
        useUiStore.getState().setCropping(null);
        return;
      }
      if (
        useUiStore.getState().cropping ||
        document.querySelector(".cm-ctx, .cm-modal-backdrop, .cm-gallery-backdrop, .cm-gdetail-backdrop, .cm-compare")
      ) {
        return;
      }
      if ((e.metaKey || e.ctrlKey) && (e.key === "z" || e.key === "Z")) {
        e.preventDefault();
        if (e.shiftKey) useHistoryStore.getState().redo();
        else useHistoryStore.getState().undo();
        return;
      }
      // Navigation: ⇧1 fit all, ⇧2 zoom to selection, ⌘0 reset 100%.
      if (e.shiftKey && e.code === "Digit1") {
        e.preventDefault();
        sceneRef.current?.fitAll();
        return;
      }
      if (e.shiftKey && e.code === "Digit2") {
        e.preventDefault();
        sceneRef.current?.fitSelection();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && (e.code === "Equal" || e.code === "NumpadAdd" || e.key === "+" || e.key === "=")) {
        e.preventDefault();
        sceneRef.current?.zoomStep("in");
        return;
      }
      if ((e.metaKey || e.ctrlKey) && (e.code === "Minus" || e.code === "NumpadSubtract" || e.key === "-")) {
        e.preventDefault();
        sceneRef.current?.zoomStep("out");
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.code === "Digit0") {
        e.preventDefault();
        sceneRef.current?.resetZoom();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && (e.key === "c" || e.key === "C")) {
        const b = useBoardStore.getState();
        const first = [...b.selection][0];
        if (b.boardId && first) {
          e.preventDefault();
          void ipc.copyImage(b.boardId, first);
        }
        return;
      }
      if (e.key === "Delete" || e.key === "Backspace") {
        if (useBoardStore.getState().selection.size > 0) {
          e.preventDefault();
          void useBoardStore.getState().deleteSelected();
        }
      } else if (e.key === "v" || e.key === "V") {
        useUiStore.getState().setTool("select");
      } else if (e.key === "r" || e.key === "R") {
        useUiStore.getState().setTool("rect");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Keep fit / reveal clear of the floating chrome so outputs land in view.
  useEffect(() => {
    sceneRef.current?.setSafeInsets({
      left: sidebarOpen ? 252 : 0,
      right: chatOpen ? chatWidth + 20 : 0,
      top: 56,
      bottom: 72,
    });
  }, [chatOpen, chatWidth, sidebarOpen]);

  // Push localized canvas-overlay strings to the scene (re-applies on lang change).
  useEffect(() => {
    sceneRef.current?.setStrings({ markComment, renameHint });
  }, [markComment, renameHint]);

  return (
    <>
      <div ref={hostRef} className="cm-canvas-host" />
      <SelectionBar rootRef={barRef} />
      <CropOverlay rootRef={cropRef} />
      <CanvasContextMenu
        menu={ctxMenu}
        onClose={() => setCtxMenu(null)}
        onFitAll={() => sceneRef.current?.fitAll()}
      />
    </>
  );
}
