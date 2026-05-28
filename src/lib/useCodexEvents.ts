import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { CodexEventEnvelope } from "../types";
import { useBoardStore } from "../store/board";
import { useChatStore } from "../store/chat";
import { ipc } from "./ipc";
import { notifyTurnDone } from "./notify";

/** Subscribes to the Rust `codex-event` channel and routes events:
 *  generated images → board store (canvas), everything → chat store. */
export function useCodexEvents() {
  useEffect(() => {
    // StrictMode dev double-mount: the cleanup can run before `listen` resolves,
    // so guard with `cancelled` and unlisten as soon as the handle arrives —
    // otherwise the first listener leaks and every event fires twice.
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<CodexEventEnvelope>("codex-event", (msg) => {
      const { boardId, event } = msg.payload;
      if (boardId !== useBoardStore.getState().boardId) return; // not the active board
      const board = useBoardStore.getState();
      if (event.kind === "generationStarted") {
        board.addPlaceholder(event.placeholderId, { x: event.x, y: event.y, w: event.w, h: event.h });
        void ipc.frontLog("info", `generating placeholder shown ${event.placeholderId}`);
      } else if (event.kind === "imageGenerated") {
        board.addGenerated(event.asset, event.placement, event.placeholderId);
      } else if (
        event.kind === "turnComplete" ||
        event.kind === "sessionComplete" ||
        event.kind === "error"
      ) {
        board.clearPlaceholders();
      }
      const wasRunning = useChatStore.getState().turnStatus === "running";
      useChatStore.getState().handleEvent(event);
      // After the turn settles, persist the assistant message + notify the OS
      // (only fires if the window is unfocused).
      if (
        wasRunning &&
        (event.kind === "turnComplete" || event.kind === "sessionComplete" || event.kind === "error")
      ) {
        useChatStore.getState().persistAssistant();
        if (event.kind === "turnComplete" && event.status === "completed") void notifyTurnDone("生成完成 ✓");
      }
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
