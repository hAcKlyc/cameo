import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { ipc } from "../lib/ipc";

/**
 * Single source of truth for the auto-updater UI, shared by the topbar
 * "重启更新" button (UpdateIndicator) and the Settings → Version section
 * (VersionSection).
 *
 * The Rust runtime owns the actual update flow (silent background download on
 * startup; on-demand install). This store mirrors its lifecycle events and
 * exposes two user-driven actions:
 *   - `check()`  — manual "检查更新": runs the same check+download as startup,
 *                  but immediately (no 60s delay). Resolves to a result code the
 *                  caller turns into a toast.
 *   - `install()`— "重启更新": hand the downloaded bytes to the installer.
 *
 * Design rule (learned from a stuck-state bug): the phase machine is driven by
 * the *authoritative* command return + explicit guards, NOT by assuming an event
 * will arrive. Events are an optimization that advance the UI sooner; every
 * action path also settles the phase deterministically so a missing event (e.g.
 * a background "up to date" check that emits nothing) can never strand the UI.
 *
 * `pendingVersion` means strictly "a downloaded build is on disk and
 * installable". It is set ONLY by `ready-to-restart`; an in-flight download does
 * not touch it, so a failed newer download never erases an older installable one.
 *
 * Phase machine:
 *   idle → checking → (idle | downloading | ready)
 *   downloading → (ready | back to ready/idle on failure)
 *   ready → installing → (process replaced | ready+error | idle if bytes gone)
 */
export type UpdaterPhase = "idle" | "checking" | "downloading" | "ready" | "installing";

export interface DownloadProgress {
  downloaded: number;
  total: number;
}

/** Outcome of a user-initiated `check()`, used to pick the right toast. */
export type CheckResult = "latest" | "found" | "error";

/** Outcome of a user-initiated `install()` failure (success never returns —
 *  the process is replaced). "cleared" = the pending bytes are gone, so the
 *  button dropped back to "检查更新"; "retry" = bytes remain, retry is viable. */
export type InstallResult = "cleared" | "retry";

interface UpdaterState {
  phase: UpdaterPhase;
  /** Version of the downloaded-and-installable update; null until one is ready. */
  pendingVersion: string | null;
  progress: DownloadProgress | null;
  /** Last check/install error (drives retry styling + tooltip). */
  error: string | null;
  /** Wire up the Rust event listeners. Idempotent — call once at app boot. */
  init: () => void;
  check: () => Promise<CheckResult>;
  install: () => Promise<InstallResult | undefined>;
}

let initialized = false;

/**
 * Install-failure payloads where the Rust side has already wiped the pending
 * bytes from disk. Keeping a "重启更新/重试" button after these would loop
 * forever — every retry hits "no pending update on disk". Drop back to idle so
 * the user can re-check instead. (See updater.rs: BAD_UPDATE_PAYLOAD /
 * VERSION_MISMATCH both clear_pending_update(); the "Compression method not
 * supported" path clears too but emits the raw error string.)
 */
function pendingClearedByFailure(payload: string | null | undefined): boolean {
  if (!payload) return false;
  return (
    payload === "BAD_UPDATE_PAYLOAD" ||
    payload === "VERSION_MISMATCH" ||
    payload.includes("no pending update") ||
    payload.includes("Compression method")
  );
}

export const useUpdaterStore = create<UpdaterState>((set, get) => ({
  phase: "idle",
  pendingVersion: null,
  progress: null,
  error: null,

  init: () => {
    if (initialized) return;
    initialized = true;

    // A download started (background or manual). Flip into the progress state.
    // Does NOT touch pendingVersion: an in-flight download isn't installable yet,
    // and overwriting it here would discard an already-pending older build if
    // this (newer) download later fails. Guarded so a stray event during an
    // install can't knock the UI out of "installing".
    void listen<string>("updater:download-started", () => {
      set((s) => (s.phase === "installing" ? {} : { phase: "downloading", progress: null, error: null }));
    });

    // Live byte progress (Rust throttles to ~2%; only emitted when the server
    // sends a content-length, hence progress can stay null on chunked responses
    // — VersionSection renders an indeterminate bar in that case).
    void listen<DownloadProgress>("updater:download-progress", (e) => {
      set((s) => (s.phase === "installing" ? {} : { phase: "downloading", progress: e.payload }));
    });

    // Bytes are down and a build is installable — the button becomes "重启更新".
    // This is the ONLY writer of pendingVersion. Guarded against clobbering an
    // in-progress install (e.g. a Windows cache-warmup re-emit).
    void listen<string>("updater:ready-to-restart", (e) => {
      set((s) =>
        s.phase === "installing"
          ? {}
          : { phase: "ready", pendingVersion: e.payload || s.pendingVersion, progress: null, error: null },
      );
    });

    // Download failed. Background failures are silent by product decision; we
    // just drop the transient progress UI. Crucially, fall back to "ready" (not
    // "idle") when an older installable build is still pending, so a failed newer
    // download doesn't hide the still-valid restart button.
    void listen<string>("updater:download-failed", () => {
      set((s) =>
        s.phase === "downloading" ? { phase: s.pendingVersion ? "ready" : "idle", progress: null } : {},
      );
    });

    // Install failed (incl. Rust-side payload checks at startup on Windows, which
    // fire with no install() in flight). If the bytes were cleared, return to
    // idle and forget the version; otherwise keep the retry affordance.
    void listen<string>("updater:install-failed", (e) => {
      const cleared = pendingClearedByFailure(e.payload);
      set((s) => ({
        phase: cleared ? "idle" : s.pendingVersion ? "ready" : "idle",
        pendingVersion: cleared ? null : s.pendingVersion,
        error: cleared ? null : e.payload || "install failed",
      }));
      void ipc.frontLog("warn", `update install failed: ${e.payload || "unknown"}`).catch(() => {});
    });

    // Windows: bytes left on disk by a previous session — reveal the button
    // without re-hitting the network.
    void ipc
      .checkPendingUpdate()
      .then((v) => {
        if (v) set((s) => (s.phase === "idle" ? { phase: "ready", pendingVersion: v } : {}));
      })
      .catch(() => {
        /* not on windows or no pending — silent */
      });
  },

  check: async () => {
    const cur = get();
    // Already busy on our side — don't start a second check. Report "found" so
    // the caller doesn't toast "up to date" for an in-flight/ready update.
    if (cur.phase === "downloading" || cur.phase === "checking" || cur.phase === "installing") {
      return "found";
    }

    set({ phase: "checking", error: null });
    try {
      // Resolves after the full check+download. On mac/win a real download emits
      // ready-to-restart *before* the command resolves, so the phase may already
      // be "ready"/"downloading" via events.
      const status = await ipc.checkAndDownloadUpdate();
      // Authoritative settle: if no event moved us off "checking" (genuine
      // up-to-date, OR "busy" where a background up-to-date check held the lock
      // and emitted nothing), we MUST settle here — otherwise the button spins
      // forever waiting on an event that will never come.
      set((s) => (s.phase === "checking" ? { phase: s.pendingVersion ? "ready" : "idle" } : {}));
      const after = get();
      if (after.phase === "ready" || after.phase === "downloading") return "found";
      // Settled with nothing in flight: only a real "uptodate" is "latest".
      // "busy"/"found" with no event is ambiguous → suppress the toast.
      return status === "uptodate" ? "latest" : "found";
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      set((s) => ({ phase: s.pendingVersion ? "ready" : "idle", error: msg }));
      void ipc.frontLog("warn", `check update failed: ${msg}`).catch(() => {});
      return "error";
    }
  },

  install: async () => {
    const { phase, pendingVersion } = get();
    if (phase === "installing" || !pendingVersion) return;
    set({ phase: "installing", error: null });
    try {
      await ipc.installPendingUpdate();
      // On success the process is replaced (mac relaunch / win NSIS exit); this
      // code does not run on the new process.
      return;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      const cleared = pendingClearedByFailure(msg);
      set((s) => ({
        phase: cleared ? "idle" : s.pendingVersion ? "ready" : "idle",
        pendingVersion: cleared ? null : s.pendingVersion,
        error: cleared ? null : msg,
      }));
      void ipc.frontLog("warn", `install_pending_update failed: ${msg}`).catch(() => {});
      return cleared ? "cleared" : "retry";
    }
  },
}));
