import { useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { Plus, History, ChevronDown, ChevronUp, X, TriangleAlert, Image as ImageIcon, RefreshCw, Copy, ExternalLink } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useChatStore, type ChatBlock, type ChatMessage, type RateLimit, type SessionStatus } from "../store/chat";
import { useBoardStore } from "../store/board";
import { useSettingsStore } from "../store/settings";
import { CHAT_PANEL_MAX_WIDTH, CHAT_PANEL_MIN_WIDTH, useUiStore } from "../store/ui";
import { ipc } from "../lib/ipc";
import { useAssetObjectUrl } from "../lib/asset-url";
import type { Shape, CodexInfo } from "../types";
import { Composer } from "./Composer";
import { StreamingStatus } from "./StreamingStatus";
import { AssistantMarkdown } from "./AssistantMarkdown";
import { useT, useLocaleStore } from "../i18n/locale";
import type { MsgKey } from "../i18n/messages";
import codexIcon from "../assets/codex.png";

/** "本轮标注" — staging area above the composer: every image carrying marks +
 *  their numbered notes, removable. This is the visible "what gets sent" with
 *  the next message (marks auto-reference their image + compose into the prompt). */
/** Filename → short label, matching Composer's `stem()`. */
function stem(path: string): string {
  const base = path.split(/[/\\]/).pop() ?? path;
  const s = base.replace(/\.[^.]+$/, "");
  return s.length > 14 ? s.slice(0, 13) + "…" : s;
}

function BoardAssetImg({
  boardId,
  relPath,
  mime,
  className,
}: {
  boardId: string | null;
  relPath: string | null;
  mime?: string | null;
  className: string;
}) {
  const src = useAssetObjectUrl(boardId, relPath, mime);
  return src ? <img className={className} src={src} alt="" /> : null;
}

/**
 * User-message body — reconstructs inline pills from the flat text + refs[].
 *
 * Composer.extract() inlines each pill as ` <asset.path> ` in the sent text.
 * We walk the refs in order, find each path in `text`, and replace it with a
 * pill (visual only — no contentEditable, no drag/remove). If a ref's
 * placement was deleted between send and render (assets store doesn't have it
 * anymore) or its path can't be located in the text, that ref degrades to its
 * raw substring — the worst case is the user sees a filename instead of a
 * pill, never broken layout.
 */
function UserMessageBody({ text, refs }: { text: string; refs: string[] }) {
  const placements = useBoardStore((s) => s.placements);
  const assets = useBoardStore((s) => s.assets);
  const boardId = useBoardStore((s) => s.boardId);

  if (refs.length === 0) return <>{text}</>;

  const segments: Array<{ kind: "text"; value: string } | { kind: "pill"; pid: string; path: string; url: string | null }> = [];
  let cursor = 0;
  for (const pid of refs) {
    const p = placements.get(pid);
    const a = p && assets.get(p.assetId);
    if (!a || !boardId) continue; // skip; the leftover text below will still render fine
    const idx = text.indexOf(a.path, cursor);
    if (idx < 0) continue; // path not in text (rare — manual edit?)
    if (idx > cursor) segments.push({ kind: "text", value: text.slice(cursor, idx) });
    segments.push({ kind: "pill", pid, path: a.path, url: null });
    cursor = idx + a.path.length;
  }
  if (cursor < text.length) segments.push({ kind: "text", value: text.slice(cursor) });

  return (
    <>
      {segments.map((seg, i) =>
        seg.kind === "text" ? (
          <span key={i}>{seg.value}</span>
        ) : (
          <span key={i} className="cm-pill cm-pill--inline" data-pid={seg.pid}>
            <BoardAssetImg boardId={boardId} relPath={seg.path} className="cm-pill__img" />
            <span className="cm-pill__label">{stem(seg.path)}</span>
          </span>
        ),
      )}
    </>
  );
}

function MarkStaging() {
  const annotations = useBoardStore((s) => s.annotations);
  const placements = useBoardStore((s) => s.placements);
  const assets = useBoardStore((s) => s.assets);
  const boardId = useBoardStore((s) => s.boardId);
  const [editing, setEditing] = useState<string | null>(null);
  const t = useT();
  const entries = [...annotations.entries()].filter(([, sh]) => sh.length > 0);
  if (!boardId || entries.length === 0) return null;
  const remove = (pid: string, shapes: Shape[], i: number) =>
    useBoardStore.getState().setAnnotation(pid, shapes.filter((_, j) => j !== i));
  const setNote = (pid: string, shapes: Shape[], i: number, note: string) =>
    useBoardStore.getState().setAnnotation(
      pid,
      shapes.map((s, j) => (j === i ? { ...s, note: note.trim() || undefined } : s))
    );
  return (
    <div className="cm-staging">
      <div className="cm-staging__head">{t("chat.staging")}</div>
      {entries.map(([pid, shapes]) => {
        const p = placements.get(pid);
        const a = p && assets.get(p.assetId);
        return (
          <div className="cm-staging__row" key={pid}>
            <BoardAssetImg boardId={boardId} relPath={a?.path ?? null} mime={a?.mime} className="cm-staging__thumb" />
            <div className="cm-staging__marks">
              {shapes.map((s, i) => {
                const key = s.id ?? `${pid}-${i}`;
                return (
                  <div className="cm-staging__mark" key={key}>
                    <span className="cm-staging__num">{i + 1}</span>
                    {editing === key ? (
                      <input
                        className="cm-staging__edit"
                        autoFocus
                        defaultValue={s.note ?? ""}
                        placeholder={t("chat.markComment")}
                        onBlur={(e) => {
                          setNote(pid, shapes, i, e.target.value);
                          setEditing(null);
                        }}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                          if (e.key === "Escape") setEditing(null);
                        }}
                      />
                    ) : (
                      <span
                        className={`cm-staging__note${s.note ? "" : " cm-staging__note--empty"}`}
                        onClick={() => setEditing(key)}
                      >
                        {s.note || t("chat.notePlaceholder")}
                      </span>
                    )}
                    <button className="cm-staging__x" title={t("chat.deleteMark")} onClick={() => remove(pid, shapes, i)}>
                      <X size={13} />
                    </button>
                  </div>
                );
              })}
            </div>
          </div>
        );
      })}
    </div>
  );
}

/** New-conversation + history (switch sessions). */
function SessionControls() {
  const sessions = useChatStore((s) => s.sessions);
  const activeId = useChatStore((s) => s.activeSessionId);
  const running = useChatStore((s) => s.turnStatus === "running");
  const [open, setOpen] = useState(false);
  const ordered = [...sessions].sort((a, b) => b.updatedAt - a.updatedAt);
  const t = useT();
  return (
    <div className="cm-sessions">
      <button
        className="cm-sessions__btn"
        title={running ? t("chat.finishTurnFirst") : t("chat.newConversation")}
        disabled={running}
        onClick={() => void useChatStore.getState().newSession()}
      >
        <Plus size={15} />
      </button>
      <button
        className="cm-sessions__btn"
        title={t("chat.history")}
        disabled={running}
        onClick={() => setOpen((o) => !o)}
      >
        <History size={15} />
      </button>
      {open && (
        <div className="cm-sessions__menu" onMouseLeave={() => setOpen(false)}>
          {ordered.length === 0 && <div className="cm-sessions__empty">{t("chat.noSessions")}</div>}
          {ordered.map((s) => (
            <button
              key={s.id}
              className={`cm-sessions__item${s.id === activeId ? " is-active" : ""}`}
              onClick={() => {
                setOpen(false);
                if (s.id !== activeId) void useChatStore.getState().switchSession(s.id);
              }}
            >
              {s.title || t("chat.newSession")}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** Floating todo above the composer (right-aligned). Collapsed → "Todo n/m";
 *  click expands UPWARD into the step list. Stays pinned. */
function TodoFloat() {
  const plan = useChatStore((s) => s.plan);
  const [open, setOpen] = useState(false);
  const t = useT();
  if (!plan || plan.steps.length === 0) return null;
  const total = plan.steps.length;
  const done = plan.steps.filter((s) => s.status === "completed").length;
  return (
    <div className={`cm-todo${open ? " is-open" : ""}`}>
      {open && (
        <ul className="cm-todo__list">
          {plan.steps.map((s, i) => (
            <li key={i} className={`cm-todo__item cm-todo__item--${s.status}`}>
              <span className="cm-todo__mark" aria-hidden />
              <span className="cm-todo__text">{s.step}</span>
            </li>
          ))}
        </ul>
      )}
      <button className="cm-todo__bar" onClick={() => setOpen((o) => !o)}>
        <span className="cm-todo__chev">{open ? <ChevronDown size={12} /> : <ChevronUp size={12} />}</span>
        <span>{t("chat.todo", { done, total })}</span>
      </button>
    </div>
  );
}

/** Generated image row backed by Board-scoped object URLs. */
function GeneratedImageBlock({ placementId }: { placementId: string }) {
  const t = useT();
  const boardId = useBoardStore((s) => s.boardId);
  const placement = useBoardStore((s) => s.placements.get(placementId));
  const asset = useBoardStore((s) => (placement ? s.assets.get(placement.assetId) : undefined));
  const url = useAssetObjectUrl(boardId, asset?.path ?? null, asset?.mime);

  return (
    <button
      className="cm-genimg"
      title={t("chat.showOnCanvas")}
      onClick={() => useBoardStore.getState().revealPlacement(placementId)}
    >
      {url && <img className="cm-genimg__thumb" src={url} alt="" />}
      <span className="cm-genimg__label">{t("chat.generated")}</span>
    </button>
  );
}

const CODEX_INSTALL_CMD = "npm install -g @openai/codex";
const CODEX_START_CMD = "codex";
const CODEX_DOCS = "https://developers.openai.com/codex/cli";

function CmdRow({ cmd }: { cmd: string }) {
  return (
    <div className="cm-agent-pop__cmd">
      <code>{cmd}</code>
      <button title="Copy" onClick={() => void navigator.clipboard.writeText(cmd)}>
        <Copy size={13} />
      </button>
    </div>
  );
}

/** Format the primary (5-hour) reset as a countdown `HH:MM` until reset.
 *  Returns null if the timestamp is missing or already past. Codex emits
 *  `resetsAt` as Unix epoch seconds. */
function formatResetCountdown(resetsAt?: number | null): string | null {
  if (resetsAt == null) return null;
  const ms = resetsAt * 1000 - Date.now();
  if (ms <= 0) return null;
  const totalMin = Math.round(ms / 60000);
  const h = Math.floor(totalMin / 60);
  const m = totalMin % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
}

/** Format the secondary (weekly) reset as a localized date `M月D日` / `Mon D`. */
function formatResetDate(resetsAt?: number | null, lang: "zh" | "en" = "en"): string | null {
  if (resetsAt == null) return null;
  const d = new Date(resetsAt * 1000);
  if (Number.isNaN(d.getTime())) return null;
  if (lang === "zh") return `${d.getMonth() + 1}月${d.getDate()}日`;
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

function QuotaRow({
  label,
  percent,
  reset,
  resetSuffix,
}: {
  label: string;
  percent: number;
  reset: string | null;
  resetSuffix: string;
}) {
  return (
    <div className="cm-agent-pop__quota">
      <span className="cm-agent-pop__k">{label}</span>
      <span className="cm-agent-pop__quotapct">{Math.round(percent)}%</span>
      {reset && (
        <span className="cm-agent-pop__quotareset">
          {reset} {resetSuffix}
        </span>
      )}
    </div>
  );
}

/** Clickable agent capsule that drops a status panel: Codex detection (path +
 *  version) + connection state, or install guidance when not found. Runtime-
 *  agnostic — a future agent just swaps the icon/name + detection source. */
function AgentStatus({ status, rateLimit }: { status: SessionStatus; rateLimit: RateLimit | null }) {
  const [open, setOpen] = useState(false);
  const [info, setInfo] = useState<CodexInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const boardId = useBoardStore((s) => s.boardId);
  const t = useT();
  const lang = useLocaleStore((s) => s.lang);
  const statusLabel = t(`agent.status.${status}` as MsgKey);

  const detect = useCallback(() => {
    setLoading(true);
    ipc
      .detectCodex()
      .then(setInfo)
      .catch(() => setInfo({ found: false }))
      .finally(() => setLoading(false));
  }, []);

  const reconnect = useCallback(() => {
    void (async () => {
      if (!boardId) return;
      await ipc.stopSession(boardId).catch(() => { /* best effort */ });
      if (useBoardStore.getState().boardId !== boardId) return;
      useSettingsStore.setState((s) => ({ restartNonce: s.restartNonce + 1 }));
    })();
    setOpen(false);
  }, [boardId]);

  useEffect(() => {
    if (open && !info) detect();
  }, [open, info, detect]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("pointerdown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="cm-agent-wrap" ref={ref}>
      <button className="cm-agent" title={`Codex · ${statusLabel}`} onClick={() => setOpen((o) => !o)}>
        <img className="cm-agent__icon" src={codexIcon} alt="" />
        <span className="cm-agent__name">Codex</span>
        <span className="cm-agent__dot" data-status={status} aria-hidden />
        <ChevronDown size={13} className="cm-agent__caret" />
      </button>
      {open && (
        <div className="cm-agent-pop">
          {loading && !info ? (
            <div className="cm-agent-pop__hint">{t("agent.detecting")}</div>
          ) : info?.found ? (
            <>
              <div className="cm-agent-pop__row">
                <span className="cm-agent-pop__k">{t("agent.connection")}</span>
                <span className="cm-agent-pop__status">
                  <span className="cm-agent__dot" data-status={status} aria-hidden />
                  {statusLabel}
                </span>
              </div>
              <div className="cm-agent-pop__row">
                <span className="cm-agent-pop__k">{t("agent.version")}</span>
                <span className="cm-agent-pop__v">{info.version ?? "—"}</span>
              </div>
              <div className="cm-agent-pop__row">
                <span className="cm-agent-pop__k">{t("agent.path")}</span>
                <span className="cm-agent-pop__v cm-agent-pop__path" title={info.path ?? ""}>
                  {info.path ?? "—"}
                </span>
              </div>
              {rateLimit && (rateLimit.usedPercent != null || rateLimit.secondaryUsedPercent != null) && (
                <div className="cm-agent-pop__quotas">
                  {rateLimit.usedPercent != null && (
                    <QuotaRow
                      label={t("agent.quota5h")}
                      percent={rateLimit.usedPercent}
                      reset={formatResetCountdown(rateLimit.resetsAt)}
                      resetSuffix={t("agent.quotaReset")}
                    />
                  )}
                  {rateLimit.secondaryUsedPercent != null && (
                    <QuotaRow
                      label={t("agent.quotaWeek")}
                      percent={rateLimit.secondaryUsedPercent}
                      reset={formatResetDate(rateLimit.secondaryResetsAt, lang)}
                      resetSuffix={t("agent.quotaReset")}
                    />
                  )}
                </div>
              )}
              <button className="cm-agent-pop__link" onClick={detect}>
                <RefreshCw size={12} />
                {t("agent.redetect")}
              </button>
              {status !== "ready" && status !== "starting" && (
                <button className="cm-agent-pop__link" onClick={reconnect}>
                  <RefreshCw size={12} />
                  {t("agent.reconnect")}
                </button>
              )}
            </>
          ) : (
            <>
              <div className="cm-agent-pop__title">
                <span className="cm-agent__dot" data-status="error" aria-hidden />
                {t("agent.notFound")}
              </div>
              <p className="cm-agent-pop__desc">{t("agent.installDesc")}</p>
              <div className="cm-agent-pop__step">
                <span className="cm-agent-pop__steplabel">{t("agent.step1")}</span>
                <CmdRow cmd={CODEX_INSTALL_CMD} />
              </div>
              <div className="cm-agent-pop__step">
                <span className="cm-agent-pop__steplabel">{t("agent.step2")}</span>
                <CmdRow cmd={CODEX_START_CMD} />
                <p className="cm-agent-pop__hint2">{t("agent.step2note")}</p>
              </div>
              <div className="cm-agent-pop__actions">
                <button className="cm-agent-pop__link" onClick={() => void openUrl(CODEX_DOCS)}>
                  <ExternalLink size={12} />
                  {t("agent.docs")}
                </button>
                <button className="cm-agent-pop__link" onClick={detect}>
                  <RefreshCw size={12} />
                  {t("agent.redetect")}
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}

/** Single-line status row: status dot + label + optional gray detail subtitle. */
function Row({
  status,
  label,
  detail,
}: {
  status: "running" | "done" | "error";
  label: string;
  detail?: string | null;
}) {
  return (
    <div className="cm-row" data-status={status}>
      <span className="cm-row__dot" aria-hidden />
      <span className="cm-row__label">{label}</span>
      {detail ? <span className="cm-row__detail">{detail}</span> : null}
    </div>
  );
}

/** Thinking row with a live "Thinking · Ns" timer while active, frozen on stop. */
function ThinkingRow({ block }: { block: Extract<ChatBlock, { type: "thinking" }> }) {
  const [, tick] = useState(0);
  const t = useT();
  useEffect(() => {
    if (!block.active) return;
    const id = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, [block.active]);
  const ms = block.active ? (block.startedAt ? Date.now() - block.startedAt : 0) : block.durationMs ?? 0;
  const secs = Math.max(0, Math.round(ms / 1000));
  return (
    <Row
      status={block.active ? "running" : "done"}
      label={block.active ? t("chat.thinking") : t("chat.thought")}
      detail={secs > 0 ? `${secs}s` : null}
    />
  );
}

/** Render one block. Blocks come in stream order — think / tool / text /
 *  image interleaved — and each renders on its own line, top to bottom.
 *
 *  `seenPaths` (when present) is mutated by the text branch to deduplicate
 *  inline image refs across the whole assistant message — same path mentioned
 *  twice only renders an image at the first occurrence; subsequent ones stay
 *  as plain text. */
function Block({
  block,
  seenPaths,
}: {
  block: ChatBlock;
  seenPaths?: Set<string>;
}) {
  const t = useT();
  switch (block.type) {
    case "text":
      return <TextBlockRender text={block.text} seenPaths={seenPaths} />;
    case "thinking":
      return <ThinkingRow block={block} />;
    case "tool":
      return (
        <Row
          status={block.status === "running" ? "running" : "done"}
          label={block.name}
          detail={block.detail}
        />
      );
    case "image": {
      if (block.status === "generating") {
        // Codex started image gen — the canvas already shows a placeholder.
        // In the chat we render an inert pending card so the user (with the
        // chat panel focused) sees the state change WITHOUT a redundant
        // thumbnail (there's no thumb until generation finishes).
        return (
          <div className="cm-genimg cm-genimg--pending" aria-busy="true">
            <span className="cm-genimg__spin" />
            <span className="cm-genimg__label">{t("chat.imageGenerating")}</span>
          </div>
        );
      }
      return <GeneratedImageBlock placementId={block.placementId} />;
    }
    case "note":
      return (
        <div className={`cm-note cm-note--${block.level}`}>
          <TriangleAlert size={13} className="cm-note__ico" />
          <span>{block.text}</span>
        </div>
      );
  }
}

function AssistantMessage({ m }: {
  m: Extract<ChatMessage, { role: "assistant" }>;
}) {
  // No per-message "Working…" row anymore — replaced by the ambient
  // <StreamingStatus /> at the bottom of the chat panel which stays visible
  // for the entire turn (not just until the first block arrives).
  const t = useT();
  if (m.status === "done" && m.blocks.length === 0) return null;

  // Set is recreated on every render — that's correct for chat-image dedup:
  // we want first-occurrence-renders-as-image semantics within ONE message,
  // not persisted state. Mutated by Block's text branch as it walks blocks.
  const seenPaths = new Set<string>();
  return (
    <div className="cm-msg cm-msg--assistant">
      {m.blocks.map((b, i) => (
        <Block
          key={b.type === "tool" ? `tool-${b.id}` : `${b.type}-${i}`}
          block={b}
          seenPaths={seenPaths}
        />
      ))}
      {m.status === "error" && !m.blocks.length && (
        <div className="cm-msg__err">{m.error || t("chat.error")}</div>
      )}
    </div>
  );
}

/**
 * Text body of an assistant message. Markdown rendering stays isolated here
 * so runtime events remain plain text blocks. Image references still resolve
 * through ChatInlineImage inside AssistantMarkdown.
 *
 * Dedup: first occurrence of a path renders as an image; subsequent
 * mentions stay as text. The `seenPaths` set is shared across all text
 * blocks of one assistant message (passed down from AssistantMessage),
 * so an image mentioned in one paragraph + repeated in another only
 * renders once.
 */
function TextBlockRender({ text, seenPaths }: { text: string; seenPaths?: Set<string> }) {
  return <AssistantMarkdown text={text} seenPaths={seenPaths} />;
}

export function ChatPanel() {
  const sessionStatus = useChatStore((s) => s.sessionStatus);
  const messages = useChatStore((s) => s.messages);
  const rateLimit = useChatStore((s) => s.rateLimit);
  const chatWidth = useUiStore((s) => s.chatWidth);
  const setChatWidth = useUiStore((s) => s.setChatWidth);
  const scrollRef = useRef<HTMLDivElement>(null);
  const cleanupResizeRef = useRef<(() => void) | null>(null);
  const resizeFrameRef = useRef<number | null>(null);
  const pendingWidthRef = useRef(chatWidth);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages]);

  useEffect(() => {
    return () => {
      cleanupResizeRef.current?.();
      if (resizeFrameRef.current !== null) window.cancelAnimationFrame(resizeFrameRef.current);
    };
  }, []);

  const beginResize = useCallback((e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = chatWidth;

    const flushWidth = (next: number) => {
      pendingWidthRef.current = next;
      if (resizeFrameRef.current !== null) return;
      resizeFrameRef.current = window.requestAnimationFrame(() => {
        resizeFrameRef.current = null;
        setChatWidth(pendingWidthRef.current);
      });
    };

    const onMove = (move: PointerEvent) => {
      const viewportMax = Math.max(CHAT_PANEL_MIN_WIDTH, window.innerWidth - 32);
      const max = Math.min(CHAT_PANEL_MAX_WIDTH, viewportMax);
      const next = Math.min(max, Math.max(CHAT_PANEL_MIN_WIDTH, startWidth + startX - move.clientX));
      flushWidth(Math.round(next));
    };

    const finish = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      document.body.classList.remove("cm-chat-resizing");
      cleanupResizeRef.current = null;
    };

    cleanupResizeRef.current?.();
    cleanupResizeRef.current = finish;
    document.body.classList.add("cm-chat-resizing");
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
  }, [chatWidth, setChatWidth]);

  return (
    <div className="cm-chat" style={{ width: chatWidth }}>
      <div
        className="cm-chat__resize"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize chat"
        onPointerDown={beginResize}
      />
      <div className="cm-chat__header">
        <AgentStatus status={sessionStatus} rateLimit={rateLimit} />
        <div className="cm-chat__hspacer" />
        <SessionControls />
      </div>

      <div className="cm-chat__messages" ref={scrollRef}>
        {messages.map((m) =>
          m.role === "user" ? (
            <div className="cm-msg cm-msg--user" key={m.id}>
              {m.refs.length > 0 && (
                <span className="cm-msg__refcount">
                  <ImageIcon size={11} />
                  {m.refs.length}
                </span>
              )}
              <UserMessageBody text={m.text} refs={m.refs} />
            </div>
          ) : (
            <AssistantMessage m={m} key={m.id} />
          )
        )}
        {/* Ambient indicator — visible the ENTIRE time a turn is running,
            including the long thinking/tool/image phases the old per-message
            Working… row missed. Self-mounts/unmounts on turnStatus. */}
        <StreamingStatus />
      </div>

      <div className="cm-chat__bottom">
        <TodoFloat />
        <MarkStaging />
        <Composer />
      </div>
    </div>
  );
}
