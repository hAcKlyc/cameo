/**
 * Per-Board Codex permission preset for the composer menu. This intentionally
 * stays separate from generation knobs: it maps to sandbox/approval behavior,
 * not model behavior.
 */

import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type { PermissionMode } from "../types";

export const DEFAULT_PERMISSION_MODE: PermissionMode = "workspace";

interface PermissionState {
  boardId: string | null;
  mode: PermissionMode;

  load: (boardId: string) => Promise<void>;
  setMode: (mode: PermissionMode) => void;
}

function cleanMode(mode: string | null | undefined): PermissionMode {
  return mode === "fullAccess" ? "fullAccess" : DEFAULT_PERMISSION_MODE;
}

let loadSeq = 0;
let mutationSeq = 0;

function persist(s: Pick<PermissionState, "boardId" | "mode">) {
  if (!s.boardId) return;
  void ipc
    .setPermissionSettings(s.boardId, { mode: s.mode })
    .catch((e) => {
      void ipc.frontLog("warn", `persist permission settings failed: ${e}`).catch(() => {});
    });
}

export const usePermissionStore = create<PermissionState>((set, get) => ({
  boardId: null,
  mode: DEFAULT_PERMISSION_MODE,

  load: async (boardId) => {
    const requestSeq = ++loadSeq;
    const requestMutationSeq = mutationSeq;
    set({ boardId, mode: DEFAULT_PERMISSION_MODE });
    try {
      const settings = await ipc.getPermissionSettings(boardId);
      if (
        get().boardId !== boardId ||
        requestSeq !== loadSeq ||
        requestMutationSeq !== mutationSeq
      ) {
        return;
      }
      set({ mode: cleanMode(settings.mode) });
    } catch {
      if (
        get().boardId !== boardId ||
        requestSeq !== loadSeq ||
        requestMutationSeq !== mutationSeq
      ) {
        return;
      }
      set({ mode: DEFAULT_PERMISSION_MODE });
    }
  },

  setMode: (mode) => {
    mutationSeq += 1;
    set({ mode: cleanMode(mode) });
    persist(get());
  },
}));
