import { useEffect, useMemo, useRef, useState } from "react";
import { Loader2, WifiOff } from "lucide-react";
import { useChatStore, type ChatMessage, type TransportStatus } from "../store/chat";
import { useLocaleStore } from "../i18n/locale";
import { pickPhrase, fixedPhrase, type PhraseLocale } from "../lib/streamingPhrases";

/**
 * Ambient "agent is working" indicator. Visible exactly when a turn is
 * running. Shows a spinner, a context-aware phrase (themed around image
 * generation when that's the current block, or a random pool otherwise), and
 * the elapsed seconds.
 *
 * Phrase rotation is event-driven + clock-driven:
 *   • Each new block on the active assistant message → bump seed (so the
 *     phrase changes when the agent visibly moves on).
 *   • Every PHRASE_ROTATE_MS while the turn is still running → bump seed
 *     (so the user never sees the same line for too long during a slow phase
 *     like a 60s image generation).
 *
 * The state-priority cascade (starting > thinking > tool > image > random)
 * means semantically loaded phases (`Codex starting up`, `Thinking`,
 * `Calling <toolName>`) display real status instead of a random pool phrase
 * — which would be confusing.
 */

const PHRASE_ROTATE_MS = 12_000;

export function StreamingStatus() {
  const turnStatus = useChatStore((s) => s.turnStatus);
  const sessionStatus = useChatStore((s) => s.sessionStatus);
  const transportStatus = useChatStore((s) => s.transportStatus);
  const lastBlock = useChatStore((s) => {
    const m = lastAssistant(s.messages);
    return m && m.blocks.length > 0 ? m.blocks[m.blocks.length - 1] : null;
  });
  const blockCount = useChatStore((s) => {
    const m = lastAssistant(s.messages);
    return m ? m.blocks.length : 0;
  });
  const lang = useLocaleStore((s) => s.lang);
  const locale: PhraseLocale = lang === "zh" ? "zh" : "en";

  // Seed drives phrase rotation. Bumped on (a) block count change and (b)
  // every PHRASE_ROTATE_MS while running. We start at a small random offset
  // so two simultaneous launches don't display the same phrase.
  const [seed, setSeed] = useState(() => Math.floor(Math.random() * 1000));
  const seedRef = useRef(seed);
  seedRef.current = seed;

  // Bump seed on block transitions.
  useEffect(() => {
    if (turnStatus === "running") setSeed((s) => s + 1);
  }, [blockCount, turnStatus]);

  // Bump seed periodically while running.
  useEffect(() => {
    if (turnStatus !== "running") return;
    const id = setInterval(() => setSeed((s) => s + 1), PHRASE_ROTATE_MS);
    return () => clearInterval(id);
  }, [turnStatus]);

  // Elapsed-time counter. Resets when a new turn starts (turnStatus 0→1).
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    if (turnStatus !== "running") {
      setElapsed(0);
      return;
    }
    setElapsed(0);
    const startedAt = Date.now();
    const id = setInterval(() => setElapsed(Math.floor((Date.now() - startedAt) / 1000)), 1000);
    return () => clearInterval(id);
  }, [turnStatus]);

  const phrase = useMemo(() => {
    if (transportStatus) return transportPhrase(locale, transportStatus);
    if (sessionStatus === "starting") return fixedPhrase(locale, "starting");
    if (!lastBlock) return pickPhrase(locale, "general", seed);
    if (lastBlock.type === "thinking" && lastBlock.active) {
      return fixedPhrase(locale, "thinking");
    }
    if (lastBlock.type === "tool" && lastBlock.status === "running") {
      return fixedPhrase(locale, "tool", lastBlock.name);
    }
    if (lastBlock.type === "image" && lastBlock.status === "generating") {
      return pickPhrase(locale, "image", seed);
    }
    return pickPhrase(locale, "general", seed);
  }, [locale, transportStatus, sessionStatus, lastBlock, seed]);

  if (turnStatus !== "running") return null;
  const isTransportNotice = Boolean(transportStatus);

  return (
    <div
      className={`cm-streaming-status${isTransportNotice ? " cm-streaming-status--transport" : ""}`}
      role="status"
      aria-live="polite"
    >
      {isTransportNotice ? (
        <WifiOff size={12} className="cm-streaming-status__transport-ico" />
      ) : (
        <Loader2 size={12} className="cm-streaming-status__spin" />
      )}
      <span className="cm-streaming-status__text">{phrase}</span>
      {elapsed >= 3 && (
        <span className="cm-streaming-status__elapsed">{formatElapsed(locale, elapsed)}</span>
      )}
    </div>
  );
}

function transportPhrase(locale: PhraseLocale, status: TransportStatus): string {
  if (status.phase === "fallback") {
    return locale === "zh" ? "连接不稳，已切换 HTTPS 继续" : "Connection unstable, continuing over HTTPS";
  }
  const suffix = status.attempt && status.max ? ` ${status.attempt}/${status.max}` : "";
  return locale === "zh" ? `连接不稳，正在重连${suffix}` : `Connection unstable, reconnecting${suffix}`;
}

function lastAssistant(messages: ChatMessage[]): Extract<ChatMessage, { role: "assistant" }> | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === "assistant") return m;
  }
  return null;
}

function formatElapsed(locale: PhraseLocale, sec: number): string {
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  if (locale === "zh") {
    if (h > 0) return `(${h}小时${m}分${s}秒)`;
    if (m > 0) return `(${m}分${s}秒)`;
    return `(${s}秒)`;
  }
  if (h > 0) return `(${h}h ${m}m ${s}s)`;
  if (m > 0) return `(${m}m ${s}s)`;
  return `(${s}s)`;
}
