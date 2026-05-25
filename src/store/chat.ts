import { create } from "zustand";
import type { CodexEvent, SessionMeta } from "../types";
import { ipc, type ChatImageResolution } from "../lib/ipc";
import { useBoardStore } from "./board";
import { useSettingsStore } from "./settings";
import { InactivityWatchdog } from "../lib/watchdog";

// ── runtime stability ───────────────────────────────────────────────────────
//
// Three layered guarantees keep the UI's `turnStatus` from getting wedged in
// "running" when the underlying codex sidecar misbehaves (network drop, OS
// memory pressure killing the subprocess between turns, the sidecar receiving
// a turn/start but never emitting turn/completed, etc.):
//
//   1. **Per-event watchdog touch** — every notification from the runtime
//      resets the inactivity clock; a healthy turn never trips it.
//   2. **Inactivity timeout** — 10 minutes without ANY event → force-restart
//      the session. Models the wedged-sidecar case we observed in production.
//   3. **Stop button escalation** — `stopTurn()` issues a best-effort
//      `turn/interrupt` RPC, waits 3 s, then on no progress falls through to
//      `stop_session` (Rust tree-kills the subprocess) + bumps restartNonce
//      so the App-level effect rebuilds a fresh session. Stop ALWAYS clears
//      the UI; codex's cooperation is optional.
//
// All three converge on `forceRestartSession()` — a single recovery path so
// future flow additions don't accidentally drift from the invariant.

const WATCHDOG_TIMEOUT_MS = 10 * 60 * 1000;   // 10 min
const WATCHDOG_INTERVAL_MS = 30 * 1000;        // 30 s tick
const STOP_ESCALATION_GRACE_MS = 3_000;        // wait this long for codex to ack turn/interrupt before tree-killing

// Restart-loop breaker. If the watchdog keeps firing because the new session
// also wedges (broken network, persistent backend trouble), bail out instead
// of looping forever. After RESTART_LIMIT auto-restarts inside RESTART_WINDOW_MS,
// the watchdog stops trying to auto-recover and leaves the session in idle —
// the user gets a clear error and can recover via Settings (which bumps
// restartNonce manually) or a fresh app launch.
const RESTART_LIMIT = 3;
const RESTART_WINDOW_MS = 30 * 60 * 1000;      // 30 min sliding window
const restartHistory: number[] = [];           // module-scope: timestamps of recent auto-restarts

/** CodexEvent kinds that mean "the turn made progress". Used by handleEvent
 *  to decide whether to touch the inactivity watchdog. Diagnostics / metadata
 *  (log, rateLimits, usage) and waiting states (permissionRequest) are
 *  deliberately excluded — a wedged sidecar that happens to emit a periodic
 *  rate-limit ping shouldn't silence our timeout. */
const PROGRESS_EVENT_KINDS = new Set<CodexEvent["kind"]>([
  "sessionInit",
  "textDelta",
  "textStop",
  "thinkingStart",
  "thinkingDelta",
  "thinkingStop",
  "toolStart",
  "toolStop",
  "toolResult",
  "generationStarted",
  "imageGenerated",
  "planUpdated",
  "status",
]);

export type SessionStatus = "idle" | "starting" | "ready" | "error";
export type TurnStatus = "idle" | "running";

/** An assistant message is an ORDERED stream of blocks (Riff model): they are
 *  appended in arrival order so the UI renders think → tool → text → think → …
 *  top-to-bottom exactly as it streamed, never grouped by type. */
export type ChatBlock =
  | { type: "text"; text: string }
  | { type: "thinking"; text: string; active: boolean; startedAt?: number; durationMs?: number }
  | { type: "tool"; id: string; name: string; status: "running" | "done"; detail?: string | null }
  /** Image block lifecycle:
   *   • `generating` — codex emitted `generationStarted`; the canvas already
   *     has a placeholder at (x, y, w, h) but chat needs a visible "image
   *     coming…" indicator so the user knows what the agent is doing.
   *   • `done` — `imageGenerated` patched the matching block (by placementId)
   *     with the final placement + caption. Renders the thumbnail link. */
  | { type: "image"; placementId: string; caption?: string | null; status: "generating" | "done"; startedAt?: number }
  | { type: "note"; level: string; text: string };

export interface PlanStep {
  step: string;
  status: string; // pending | inProgress | completed
}
export interface Plan {
  explanation?: string | null;
  steps: PlanStep[];
}
export interface RateLimit {
  /** Primary window = 5-hour rolling. */
  usedPercent: number;
  resetsAt?: number | null;
  /** Secondary window = weekly. */
  secondaryUsedPercent?: number | null;
  secondaryResetsAt?: number | null;
  /** Which window has been hit, if any: "primary" / "secondary". */
  reached?: string | null;
}

export type ChatMessage =
  | { id: string; role: "user"; text: string; refs: string[] }
  | { id: string; role: "assistant"; blocks: ChatBlock[]; status: "streaming" | "done" | "error"; error?: string };

export interface TurnScope {
  boardId: string | null;
  sessionId: string | null;
  assistantId: string;
}

interface ChatState {
  sessionStatus: SessionStatus;
  turnStatus: TurnStatus;
  messages: ChatMessage[];
  error: string | null;
  /** The agent's current plan/todo (turn/plan/updated). */
  plan: Plan | null;
  /** Subscription rate-limit usage (primary window). */
  rateLimit: RateLimit | null;
  /** Sessions for the current Board + which is active (multi-session). */
  sessions: SessionMeta[];
  activeSessionId: string | null;
  /** Cache for chat-image resolution. Key = the raw path string as extracted
   *  from the assistant's text (preserved verbatim so re-renders look it up
   *  the same way). Value is `null` while a resolve is in flight, a full
   *  resolution object once settled, or `false` if the path was attempted
   *  and definitively isn't an image (so we don't keep retrying). */
  imageResolutions: Map<string, ChatImageResolution | "pending" | "missing">;

  reset: () => void;
  setSessionStatus: (s: SessionStatus, error?: string) => void;
  startTurn: (text: string, refs: string[]) => TurnScope;
  /** Mark the optimistic pre-send turn as failed without restarting Codex. Used
   *  when overlay rendering or the turn/start IPC rejects before any runtime
   *  terminal event can arrive. */
  failTurn: (reason: string, scope?: TurnScope) => void;
  /** Best-effort interrupt + guaranteed UI clear. Sends turn/interrupt, then
   *  if codex doesn't ack within STOP_ESCALATION_GRACE_MS, tree-kills the
   *  sidecar and rebuilds the session. UI ALWAYS exits "running" state. */
  stopTurn: () => void;
  handleEvent: (e: CodexEvent) => void;
  /** Load the session list + the active session's timeline (on board open). */
  initSessions: (expectedBoardId?: string, expectedRestartNonce?: number) => Promise<void>;
  /** Lazily resolve a path referenced in chat text. First call kicks the
   *  Rust resolver and writes "pending"; on completion the Map gets the full
   *  resolution. Subsequent calls return the cached entry. */
  resolveChatImage: (path: string) => void;
  refreshSessions: () => Promise<void>;
  newSession: () => Promise<void>;
  switchSession: (id: string) => Promise<void>;
  /** Persist the latest assistant message to the active session (on turn end). */
  persistAssistant: () => void;
}

const newId = () => Math.random().toString(36).slice(2, 10);

/** Immutably update the last assistant message (the streaming one). */
function updateLast(
  messages: ChatMessage[],
  fn: (m: Extract<ChatMessage, { role: "assistant" }>) => Extract<ChatMessage, { role: "assistant" }>
): ChatMessage[] {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "assistant") {
      const next = messages.slice();
      next[i] = fn(m);
      return next;
    }
  }
  return messages;
}

type AsstMsg = Extract<ChatMessage, { role: "assistant" }>;

/** Append text — coalesce into a trailing text block, else open a new one. */
function appendText(m: AsstMsg, text: string): AsstMsg {
  const blocks = m.blocks.slice();
  const last = blocks[blocks.length - 1];
  if (last && last.type === "text") {
    blocks[blocks.length - 1] = { type: "text", text: last.text + text };
  } else {
    blocks.push({ type: "text", text });
  }
  return { ...m, blocks };
}

function appendThinking(m: AsstMsg, text: string): AsstMsg {
  const blocks = m.blocks.slice();
  const last = blocks[blocks.length - 1];
  if (last && last.type === "thinking") {
    // Preserve startedAt/durationMs — only append text + keep it active.
    blocks[blocks.length - 1] = { ...last, text: last.text + text, active: true };
  } else {
    blocks.push({ type: "thinking", text, active: true, startedAt: Date.now() });
  }
  return { ...m, blocks };
}

function pushBlock(m: AsstMsg, block: ChatBlock): AsstMsg {
  return { ...m, blocks: [...m.blocks, block] };
}

/** Mark the most recent matching block (by predicate) via a transform. */
function patchLast(
  m: AsstMsg,
  match: (b: ChatBlock) => boolean,
  fn: (b: ChatBlock) => ChatBlock
): AsstMsg {
  const blocks = m.blocks.slice();
  for (let i = blocks.length - 1; i >= 0; i--) {
    if (match(blocks[i])) {
      blocks[i] = fn(blocks[i]);
      break;
    }
  }
  return { ...m, blocks };
}

// ── runtime stability — module-local helpers ────────────────────────────────
//
// All three convergence points (watchdog fire / explicit Stop escalation /
// fatal error notification) end up here. Centralising the recovery is the
// reason `turnStatus` can't get wedged: any single path is exhaustive enough.

/** Convergence point. Splits into two halves:
 *
 *   • Synchronous (return value): Local state update — user sees turnStatus
 *     flip to idle immediately and the failed assistant message annotated
 *     with the reason. This is the visible-now state.
 *
 *   • Async (the IIFE): Persist the failed message to disk → kill the
 *     sidecar → bump restartNonce. The ordering matters: appendMessage MUST
 *     complete before restartNonce bumps, otherwise App.tsx's reset → reload
 *     would race the disk write and lose the explanation. */
function forceRestartSession(
  st: ChatState,
  reason: string,
  opts: { autoRestart?: boolean } = { autoRestart: true },
): Partial<ChatState> {
  const boardId = useBoardStore.getState().boardId;
  const { activeSessionId, messages } = st;

  // Snapshot the failed assistant message NOW (before we mutate state). If
  // the watchdog fired before ANY runtime event arrived, `blocks` will be
  // empty — inject a note block so the error bubble has visible content
  // instead of rendering as an empty red box.
  let failedAsst: AsstMsg | null = null;
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "assistant") {
      const settled = settle(m);
      const withNote: AsstMsg =
        settled.blocks.length === 0
          ? { ...settled, blocks: [{ type: "note", level: "error", text: reason }] }
          : settled;
      failedAsst = { ...withNote, status: "error", error: reason };
      break;
    }
  }

  // C2 fix — capture boardId NOW; the IIFE's async chain may take 100s of ms
  // and the user might switch boards in the meantime. We only restart THIS
  // board's session, never the new one's.
  const startedOnBoard = boardId;

  void (async () => {
    // 1. Persist the failed message FIRST so the post-restart reload retains
    //    the explanation rather than dropping a silently-failed turn.
    if (startedOnBoard && activeSessionId && failedAsst) {
      await ipc
        .appendMessage(startedOnBoard, activeSessionId, failedAsst)
        .catch(() => { /* best effort */ });
    }
    // 2. Tree-kill the wedged sidecar so the next startSession spawns fresh.
    if (startedOnBoard) {
      await ipc.stopSession(startedOnBoard).catch(() => { /* best effort */ });
    }
    // 3. Bump restartNonce — triggers App.tsx's session-rebuild effect chain.
    //    Skip if (a) the user has navigated away (different board active) or
    //    (b) the caller explicitly asked NOT to auto-restart (loop breaker
    //    tripped). In either case the local state is already idle so the UI
    //    is usable; just no auto-recovery.
    if (!opts.autoRestart) return;
    if (useBoardStore.getState().boardId !== startedOnBoard) return;
    useSettingsStore.setState((s) => ({ restartNonce: s.restartNonce + 1 }));
  })();

  return {
    turnStatus: "idle",
    messages: updateLast(messages, (m) => {
      const settled = settle(m);
      const withNote =
        settled.blocks.length === 0
          ? { ...settled, blocks: [{ type: "note" as const, level: "error", text: reason }] }
          : settled;
      return { ...withNote, status: "error" as const, error: reason };
    }),
  };
}

/** Local failure path for an optimistic turn that never reached Codex. This keeps
 *  the same user/assistant message shape as runtime failures, but deliberately
 *  does not kill or restart the sidecar: the transport failure is already
 *  surfaced and the next send can retry against the current session. */
function failLocalTurn(st: ChatState, reason: string, scope?: TurnScope): Partial<ChatState> {
  const currentBoardId = useBoardStore.getState().boardId;
  if (scope?.boardId && currentBoardId !== scope.boardId) return {};

  const boardId = scope?.boardId ?? currentBoardId;
  const sessionId = scope?.sessionId ?? st.activeSessionId;
  const { messages } = st;

  let targetIndex = -1;
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "assistant" && (!scope || m.id === scope.assistantId)) {
      targetIndex = i;
      break;
    }
  }
  if (targetIndex < 0) return {};

  const failedAsst = (() => {
    const m = messages[targetIndex] as AsstMsg;
    const settled = settle(m);
    const withNote =
      settled.blocks.length === 0
        ? { ...settled, blocks: [{ type: "note" as const, level: "error", text: reason }] }
        : settled;
    return { ...withNote, status: "error" as const, error: reason };
  })();
  const nextMessages = messages.slice();
  nextMessages[targetIndex] = failedAsst;
  const isLatestAssistant = !messages.slice(targetIndex + 1).some((m) => m.role === "assistant");

  if (isLatestAssistant) watchdog.stop();

  if (boardId && sessionId) {
    void ipc.appendMessage(boardId, sessionId, failedAsst).catch(() => { /* best effort */ });
  }

  return {
    turnStatus: isLatestAssistant ? "idle" : st.turnStatus,
    error: isLatestAssistant ? reason : st.error,
    messages: nextMessages,
  };
}

/** Record an auto-restart and decide whether the loop breaker should trip. */
function shouldBreakRestartLoop(): boolean {
  const now = Date.now();
  // Prune entries older than the window.
  while (restartHistory.length > 0 && now - restartHistory[0] > RESTART_WINDOW_MS) {
    restartHistory.shift();
  }
  restartHistory.push(now);
  return restartHistory.length > RESTART_LIMIT;
}

/** End-of-turn: stop spinners on any still-active block. */
function settle(m: AsstMsg): AsstMsg {
  const blocks = m.blocks.map((b) =>
    b.type === "thinking" && b.active
      ? { ...b, active: false, durationMs: b.startedAt ? Date.now() - b.startedAt : b.durationMs }
      : b.type === "tool" && b.status === "running"
        ? { ...b, status: "done" as const }
        : b
  );
  return { ...m, blocks };
}

/** Monotonic counter used by `stopTurn` to invalidate its own orphan
 *  escalation timer when a fresh turn starts before the timer fires. */
let stopEpoch = 0;

/** Singleton watchdog — armed on startTurn, disarmed on every terminal event.
 *  Lives at module scope so the `setTimeout` inside doesn't get garbage-
 *  collected with the store reference. */
const watchdog = new InactivityWatchdog({
  timeoutMs: WATCHDOG_TIMEOUT_MS,
  intervalMs: WATCHDOG_INTERVAL_MS,
  onFire: () => {
    const st = useChatStore.getState();
    if (st.turnStatus !== "running") return; // already settled — false alarm

    const looping = shouldBreakRestartLoop();
    if (looping) {
      void ipc.frontLog(
        "error",
        `watchdog fired ${RESTART_LIMIT}+ times within ${RESTART_WINDOW_MS / 60000}min — refusing to auto-restart`,
      ).catch(() => {});
      useChatStore.setState((s) =>
        forceRestartSession(
          s,
          `Codex did not respond after ${RESTART_LIMIT} auto-restarts. Please check your network. Save Settings to retry.`,
          { autoRestart: false },
        ),
      );
      return;
    }

    void ipc.frontLog(
      "warn",
      `chat watchdog fired after ${WATCHDOG_TIMEOUT_MS / 1000}s of silence — force-restarting session`,
    ).catch(() => {});
    useChatStore.setState((s) =>
      forceRestartSession(s, "Codex did not respond for 10 minutes — session was restarted automatically."),
    );
  },
});

export const useChatStore = create<ChatState>((set, get) => ({
  sessionStatus: "idle",
  turnStatus: "idle",
  messages: [],
  error: null,
  plan: null,
  rateLimit: null,
  sessions: [],
  activeSessionId: null,
  imageResolutions: new Map(),

  reset: () => {
    // Board switch / session rebuild — any in-flight watchdog from a previous
    // turn must die or it could fire against the new session.
    watchdog.stop();
    set({
      sessionStatus: "idle",
      turnStatus: "idle",
      messages: [],
      error: null,
      plan: null,
      sessions: [],
      activeSessionId: null,
      imageResolutions: new Map(),
    });
  },

  setSessionStatus: (s, error) => set({ sessionStatus: s, error: error ?? null }),

  startTurn: (text, refs) => {
    const userMsg: ChatMessage = { id: newId(), role: "user", text, refs };
    const asstMsg: ChatMessage = { id: newId(), role: "assistant", blocks: [], status: "streaming" };
    set((st) => ({ turnStatus: "running", messages: [...st.messages, userMsg, asstMsg] }));
    // Arm the inactivity watchdog for this turn. Every subsequent runtime
    // event will touch() it; absent any event for WATCHDOG_TIMEOUT_MS we'll
    // force-restart the session (see forceRestartSession + watchdog.onFire).
    watchdog.start();
    // Bump stopEpoch so any in-flight escalation timer from a prior Stop click
    // gets invalidated — it must NOT fire its tree-kill against THIS new turn.
    stopEpoch++;
    // Persist the user message immediately (crash-safe) + auto-title the session.
    const boardId = useBoardStore.getState().boardId;
    const { activeSessionId, sessions } = get();
    if (boardId && activeSessionId) {
      void ipc.appendMessage(boardId, activeSessionId, userMsg);
      const sess = sessions.find((s) => s.id === activeSessionId);
      if (sess && (!sess.title || sess.title === "New session") && text.trim()) {
        void ipc.renameSession(boardId, activeSessionId, text.trim().slice(0, 24)).then(() => get().refreshSessions());
      }
    }
    return { boardId, sessionId: activeSessionId, assistantId: asstMsg.id };
  },

  failTurn: (reason, scope) => set((st) => failLocalTurn(st, reason, scope)),

  stopTurn: () => {
    const st = get();
    if (st.turnStatus !== "running") return;
    const boardId = useBoardStore.getState().boardId;
    if (!boardId) return;

    // C1 fix — capture our epoch so this specific escalation can be cancelled
    // by a subsequent `stopTurn` OR by `startTurn` opening a fresh turn. Without
    // this, a 3 s-old orphan timer could fire against an unrelated new turn.
    const myEpoch = ++stopEpoch;

    // Phase 1: graceful — ask codex to interrupt. If the sidecar is healthy
    // it'll respond with `turn/completed status=interrupted` which flips
    // turnStatus to idle through the normal event path.
    void ipc.interruptTurn(boardId).catch(() => { /* best effort */ });

    // Phase 2: escalation watchdog. If turnStatus is still "running" after
    // STOP_ESCALATION_GRACE_MS, codex is wedged — tree-kill the sidecar and
    // rebuild the session. The user explicitly asked to stop; we honor it
    // regardless of whether codex cooperates.
    setTimeout(() => {
      if (stopEpoch !== myEpoch) return; // a newer stop / a new turn invalidated us
      const cur = get();
      if (cur.turnStatus !== "running") return; // graceful path won the race
      void ipc.frontLog(
        "warn",
        "stopTurn: codex did not ack interrupt within grace window — escalating to tree-kill",
      ).catch(() => {});
      set((s) =>
        forceRestartSession(s, "Codex did not respond to Stop — session was restarted."),
      );
    }, STOP_ESCALATION_GRACE_MS);
  },

  handleEvent: (e) => {
    // Reset the inactivity clock on PROGRESS events only — `log` warnings,
    // `rateLimits` pings, `usage` summaries and `permissionRequest` waiting
    // are not proof the turn is making progress, so they don't count. Terminal
    // events (turnComplete / error / sessionComplete) stop the watchdog inside
    // their own case body. New event kinds added in the future must opt in
    // here AND, if terminal, call `watchdog.stop()` in the case body.
    if (PROGRESS_EVENT_KINDS.has(e.kind)) watchdog.touch();
    set((st) => {
      switch (e.kind) {
        case "sessionInit":
          return { sessionStatus: "ready" };
        case "textDelta":
          return { messages: updateLast(st.messages, (m) => appendText(m, e.text)) };
        case "thinkingStart":
          return {
            messages: updateLast(st.messages, (m) =>
              pushBlock(m, { type: "thinking", text: "", active: true, startedAt: Date.now() })
            ),
          };
        case "thinkingDelta":
          return { messages: updateLast(st.messages, (m) => appendThinking(m, e.text)) };
        case "thinkingStop":
          return {
            messages: updateLast(st.messages, (m) =>
              patchLast(
                m,
                (b) => b.type === "thinking" && b.active,
                (b) =>
                  b.type === "thinking"
                    ? { ...b, active: false, durationMs: b.startedAt ? Date.now() - b.startedAt : b.durationMs }
                    : b
              )
            ),
          };
        case "toolStart":
          return {
            messages: updateLast(st.messages, (m) =>
              pushBlock(m, { type: "tool", id: e.toolUseId, name: e.toolName, status: "running", detail: e.detail ?? undefined })
            ),
          };
        case "toolStop":
          return {
            messages: updateLast(st.messages, (m) =>
              patchLast(m, (b) => b.type === "tool" && b.id === e.toolUseId, (b) => ({ ...b, status: "done" }))
            ),
          };
        case "permissionRequest":
          return {
            messages: updateLast(st.messages, (m) =>
              pushBlock(m, {
                type: "note",
                level: "info",
                text: `Approved Codex request: ${e.summary || `request ${e.requestId}`}`,
              })
            ),
          };
        case "generationStarted":
          // Codex began generating an image — show a "generating" block in chat
          // so the agent's state change is visible (the canvas placeholder alone
          // isn't enough — users with the chat panel focused need a signal here).
          // `imageGenerated` later finds and promotes this block by placementId.
          return {
            messages: updateLast(st.messages, (m) =>
              pushBlock(m, {
                type: "image",
                placementId: e.placeholderId,
                status: "generating",
                startedAt: Date.now(),
              }),
            ),
          };
        case "imageGenerated":
          // Promote the matching `generating` block to `done`. The pending
          // block was keyed on the codex-minted `placeholderId` (a nanoid),
          // NOT `placement.id` — those are two different ids, so match on
          // placeholderId here. On promotion we swap the stored placementId
          // to the real one so the "show on canvas" reveal works.
          //
          // Fall back to push-new if no placeholder match: covers replays /
          // out-of-order delivery where generationStarted was lost.
          return {
            messages: updateLast(st.messages, (m) => {
              const idx = e.placeholderId
                ? m.blocks.findIndex(
                    (b) => b.type === "image" && b.placementId === e.placeholderId,
                  )
                : -1;
              if (idx >= 0) {
                const blocks = m.blocks.slice();
                blocks[idx] = {
                  type: "image",
                  placementId: e.placement.id,
                  status: "done",
                  caption: e.caption,
                };
                return { ...m, blocks };
              }
              return pushBlock(m, {
                type: "image",
                placementId: e.placement.id,
                status: "done",
                caption: e.caption,
              });
            }),
          };
        case "turnComplete":
          watchdog.stop();
          return {
            turnStatus: "idle",
            messages: updateLast(st.messages, (m) => ({
              ...settle(m),
              status: e.status === "completed" ? "done" : "error",
              error: e.error ?? undefined,
            })),
          };
        case "error":
          // CRITICAL: an `error` notification means this turn is dead. Flip
          // turnStatus to idle so the UI can recover. Previously this only
          // settled the message blocks while leaving turnStatus="running",
          // which is how users got stuck in the "Working…" state.
          watchdog.stop();
          return {
            turnStatus: "idle",
            messages: updateLast(st.messages, (m) => ({ ...settle(m), status: "error", error: e.message })),
          };
        case "planUpdated":
          return { plan: e.steps.length ? { explanation: e.explanation, steps: e.steps } : null };
        case "rateLimits":
          return {
            rateLimit: {
              usedPercent: e.usedPercent,
              resetsAt: e.resetsAt,
              secondaryUsedPercent: e.secondaryUsedPercent,
              secondaryResetsAt: e.secondaryResetsAt,
              reached: e.reached,
            },
          };
        case "log":
          // Surface warnings/errors as an inline note block in the stream.
          if (e.level === "warn" || e.level === "error") {
            return { messages: updateLast(st.messages, (m) => pushBlock(m, { type: "note", level: e.level, text: e.message })) };
          }
          return {};
        case "sessionComplete": {
          watchdog.stop();
          // Idle-exit suppression: if the sidecar died BETWEEN turns (no turn
          // in flight), don't surface an error block — this is usually the OS
          // killing the long-idle subprocess to reclaim memory, and the next
          // user send will transparently re-spawn + thread/resume the session.
          // If a turn WAS in flight, settle that turn's assistant message as
          // an error so the user understands what happened.
          if (st.turnStatus !== "running") {
            return { sessionStatus: "idle", turnStatus: "idle" };
          }
          return {
            sessionStatus: "idle",
            turnStatus: "idle",
            messages: updateLast(st.messages, (m) => ({
              ...settle(m),
              status: "error" as const,
              error: e.message || "Codex session ended unexpectedly.",
            })),
          };
        }
        default:
          return {};
      }
    });
  },

  initSessions: async (expectedBoardId, expectedRestartNonce) => {
    const boardId = expectedBoardId ?? useBoardStore.getState().boardId;
    if (!boardId) return;
    const doc = await ipc.listSessions(boardId);
    if (useBoardStore.getState().boardId !== boardId) return;
    if (expectedRestartNonce !== undefined && useSettingsStore.getState().restartNonce !== expectedRestartNonce) return;
    const active = doc.activeSessionId ?? doc.sessions[0]?.id ?? null;
    let messages: ChatMessage[] = [];
    if (active) messages = (await ipc.loadSession(boardId, active)) as ChatMessage[];
    if (useBoardStore.getState().boardId !== boardId) return;
    if (expectedRestartNonce !== undefined && useSettingsStore.getState().restartNonce !== expectedRestartNonce) return;
    set({ sessions: doc.sessions, activeSessionId: active, messages });
  },

  refreshSessions: async () => {
    const boardId = useBoardStore.getState().boardId;
    if (!boardId) return;
    const doc = await ipc.listSessions(boardId);
    set((st) => ({ sessions: doc.sessions, activeSessionId: doc.activeSessionId ?? st.activeSessionId }));
  },

  newSession: async () => {
    const boardId = useBoardStore.getState().boardId;
    // Never switch sessions mid-turn: late events would land on the wrong
    // session's timeline and the canvas.
    if (!boardId || get().turnStatus === "running") return;
    const id = await ipc.newSession(boardId);
    set({ messages: [], activeSessionId: id, plan: null });
    await get().refreshSessions();
  },

  switchSession: async (id) => {
    const boardId = useBoardStore.getState().boardId;
    if (!boardId || get().turnStatus === "running" || id === get().activeSessionId) return;
    await ipc.switchSession(boardId, id);
    const messages = (await ipc.loadSession(boardId, id)) as ChatMessage[];
    set({ messages, activeSessionId: id, plan: null });
    await get().refreshSessions();
  },

  persistAssistant: () => {
    const boardId = useBoardStore.getState().boardId;
    const { activeSessionId, messages } = get();
    if (!boardId || !activeSessionId) return;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role === "assistant") {
        void ipc.appendMessage(boardId, activeSessionId, messages[i]);
        break;
      }
    }
  },

  resolveChatImage: (path: string) => {
    const existing = get().imageResolutions.get(path);
    if (existing !== undefined) return; // pending OR settled — never re-resolve
    const boardId = useBoardStore.getState().boardId;
    if (!boardId) return;

    // Mark pending FIRST so concurrent callers from the same render pass
    // don't all fire the IPC. Map mutated in-place but we still call set()
    // to trigger React re-renders for subscribers.
    set((s) => {
      const next = new Map(s.imageResolutions);
      next.set(path, "pending");
      return { imageResolutions: next };
    });

    void ipc
      .resolveChatImage(boardId, path)
      .then((res) => {
        set((s) => {
          const next = new Map(s.imageResolutions);
          next.set(path, res.exists && res.isImage ? res : "missing");
          return { imageResolutions: next };
        });
      })
      .catch(() => {
        set((s) => {
          const next = new Map(s.imageResolutions);
          next.set(path, "missing");
          return { imageResolutions: next };
        });
      });
  },
}));
