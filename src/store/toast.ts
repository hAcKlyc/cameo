import { create } from "zustand";

/** Lightweight in-app toast. Distinct from lib/notify.ts (OS notifications that
 *  only fire when the window is unfocused) — these are visible feedback for
 *  actions the user just took inside the app (e.g. "already on the latest
 *  version"). Stack at the bottom-center, auto-dismiss, manually closable. */
export type ToastKind = "info" | "success" | "error";

export interface Toast {
  id: number;
  message: string;
  kind: ToastKind;
}

interface ToastState {
  toasts: Toast[];
  /** Push a toast; returns its id. durationMs<=0 keeps it until dismissed. */
  show: (message: string, kind?: ToastKind, durationMs?: number) => number;
  dismiss: (id: number) => void;
}

let seq = 0;
const timers = new Map<number, ReturnType<typeof setTimeout>>();

export const useToastStore = create<ToastState>((set, get) => ({
  toasts: [],
  show: (message, kind = "info", durationMs = 3000) => {
    const id = ++seq;
    set((s) => ({ toasts: [...s.toasts, { id, message, kind }] }));
    if (durationMs > 0) {
      timers.set(id, setTimeout(() => get().dismiss(id), durationMs));
    }
    return id;
  },
  dismiss: (id) => {
    const timer = timers.get(id);
    if (timer) {
      clearTimeout(timer);
      timers.delete(id);
    }
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
  },
}));

/** Imperative helper for non-component callers. */
export const showToast = (message: string, kind?: ToastKind, durationMs?: number) =>
  useToastStore.getState().show(message, kind, durationMs);
