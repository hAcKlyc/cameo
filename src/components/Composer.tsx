import { useEffect, useRef, useState } from "react";
import { ImagePlus, ArrowUp, Square } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { useT } from "../i18n/locale";
import { useBoardStore } from "../store/board";
import { useChatStore } from "../store/chat";
import { useComposerStore } from "../store/composer";
import { cameoUrl, ipc } from "../lib/ipc";
import { buildOverlays, buildMarkNotes, annotatedImages } from "../lib/overlay";
import { GalleryButton } from "./gallery/GalleryButton";

function stem(path: string): string {
  const base = path.split(/[/\\]/).pop() ?? path;
  const s = base.replace(/\.[^.]+$/, "");
  return s.length > 14 ? s.slice(0, 13) + "…" : s;
}

const IMAGE_EXTS = ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "avif"];

function sendErrorMessage(e: unknown): string {
  return `Could not send this turn: ${e instanceof Error ? e.message : String(e)}`;
}

/**
 * Lovart-style reference composer. References live INLINE in the text as atomic
 * pills (PRD §6.3):
 * - Clicking image(s) on the canvas drops a GHOST pill (grayed) at the caret;
 *   it tracks the selection (changes as you select others, vanishes on deselect).
 * - Clicking into the input COMMITS the ghost(s) — they turn solid and become
 *   part of the text. Selecting more on the canvas appends new ghosts after.
 * - A committed pill is atomic: backspace deletes the whole tag; it can be
 *   moved/copied like a character.
 *
 * On send we walk the text in order → instruction text (asset paths inlined) +
 * ordered reference placement ids (resolved to file paths by Rust, decision D4).
 */
export function Composer() {
  const editorRef = useRef<HTMLDivElement>(null);
  const lastRange = useRef<Range | null>(null);
  const syncing = useRef(false);

  const sessionStatus = useChatStore((s) => s.sessionStatus);
  const turnStatus = useChatStore((s) => s.turnStatus);
  const boardId = useBoardStore((s) => s.boardId);
  const [hasContent, setHasContent] = useState(false);
  const t = useT();

  const ready = !!boardId && sessionStatus === "ready";
  const running = turnStatus === "running";

  // ── pill resolution ────────────────────────────────────────────────────
  const resolve = (pid: string): { path: string; url: string } | null => {
    const { placements, assets, boardId } = useBoardStore.getState();
    const p = placements.get(pid);
    const a = p && assets.get(p.assetId);
    return a && boardId ? { path: a.path, url: cameoUrl(boardId, a.path) } : null;
  };

  const makePill = (pid: string, ghost: boolean): HTMLSpanElement => {
    const span = document.createElement("span");
    span.className = "cm-pill" + (ghost ? " cm-pill--ghost" : "");
    span.contentEditable = "false";
    span.dataset.pid = pid;
    if (ghost) span.dataset.ghost = "1";
    const r = resolve(pid);
    if (r) {
      const img = document.createElement("img");
      img.src = r.url;
      img.className = "cm-pill__img";
      span.appendChild(img);
    }
    const label = document.createElement("span");
    label.className = "cm-pill__label";
    label.textContent = r ? stem(r.path) : t("composer.imagePill");
    span.appendChild(label);
    return span;
  };

  // ── caret / ghost management ─────────────────────────────────────────────
  const anchorRange = (editor: HTMLElement): Range => {
    const r = lastRange.current;
    if (r && r.startContainer && editor.contains(r.startContainer)) return r.cloneRange();
    const end = document.createRange();
    end.selectNodeContents(editor);
    end.collapse(false);
    return end;
  };

  const refreshHasContent = () => {
    const editor = editorRef.current;
    setHasContent(!!editor && (!!editor.textContent?.trim() || !!editor.querySelector(".cm-pill")));
  };

  /** Move the visible caret to `range` (collapsed). */
  const setCaret = (range: Range) => {
    const sel = window.getSelection();
    if (!sel) return;
    sel.removeAllRanges();
    sel.addRange(range);
  };

  /** Drop the caret at the very end of the editor (after the pills). */
  const moveCaretToEnd = () => {
    const editor = editorRef.current;
    if (!editor) return;
    editor.focus();
    const r = document.createRange();
    r.selectNodeContents(editor);
    r.collapse(false);
    lastRange.current = r.cloneRange();
    setCaret(r);
  };

  /** Clicking the box's empty area drops the caret AFTER the pills (they're
   *  atomic input entities) rather than letting the browser snap it before one.
   *  Clicks on real text or directly on a pill are left to the browser. */
  const onEditorMouseDown = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest(".cm-pill")) return;
    const r = document.caretRangeFromPoint(e.clientX, e.clientY);
    if (r && r.startContainer.nodeType === Node.TEXT_NODE && (r.startContainer.textContent ?? "").length > 0) return;
    e.preventDefault();
    moveCaretToEnd();
  };

  const syncGhosts = (ids: string[]) => {
    const editor = editorRef.current;
    if (!editor) return;
    syncing.current = true;
    editor.querySelectorAll("[data-ghost]").forEach((g) => g.remove());
    if (ids.length) {
      const range = anchorRange(editor);
      for (const id of ids) {
        const pill = makePill(id, true);
        range.insertNode(pill);
        range.setStartAfter(pill);
        range.collapse(true);
      }
      // Keep the caret to the RIGHT of the inserted reference(s): remember it as
      // the anchor, and move the visible caret there if the editor is focused.
      lastRange.current = range.cloneRange();
      if (document.activeElement === editor) setCaret(range.cloneRange());
    }
    syncing.current = false;
    refreshHasContent();
  };

  const commitGhosts = () => {
    const editor = editorRef.current;
    if (!editor) return;
    const ghosts = editor.querySelectorAll("[data-ghost]");
    if (!ghosts.length) return;
    let last: Element | null = null;
    ghosts.forEach((g) => {
      g.removeAttribute("data-ghost");
      g.classList.remove("cm-pill--ghost");
      last = g;
    });
    if (last) {
      const r = document.createRange();
      r.setStartAfter(last);
      r.collapse(true);
      lastRange.current = r.cloneRange();
      // On focus, drop the caret to the RIGHT of the just-committed reference
      // so typing continues after it (browser would otherwise leave it before).
      setCaret(r.cloneRange());
    }
    refreshHasContent();
  };

  /** Insert COMMITTED (solid) reference pills for newly added images. */
  const insertRefs = (ids: string[]) => {
    const editor = editorRef.current;
    if (!editor || ids.length === 0) return;
    editor.focus();
    const range = anchorRange(editor);
    for (const id of ids) {
      const pill = makePill(id, false);
      range.insertNode(pill);
      range.setStartAfter(pill);
      range.collapse(true);
    }
    lastRange.current = range.cloneRange();
    refreshHasContent();
  };

  /** Add-image button: pick image file(s) → add to canvas + pill them. */
  const addImages = async () => {
    const sel = await open({
      multiple: true,
      title: "Add images",
      filters: [{ name: "Images", extensions: IMAGE_EXTS }],
    });
    const paths = Array.isArray(sel) ? sel : sel ? [sel] : [];
    if (!paths.length) return;
    const placements = await useBoardStore.getState().importFiles(paths as string[]);
    insertRefs(placements.map((p) => p.id));
  };

  /** Paste handler — two branches:
   *  1. Image bytes in clipboard → import to canvas + insert a pill.
   *  2. Anything else → force plain-text paste. The browser's default for
   *     contentEditable would honor clipboardData's `text/html` and bring along
   *     foreign styles (background colors, fonts, custom CSS) — e.g. text
   *     copied from a chat bubble would paste WITH its red background. The
   *     composer's "rich" elements are exactly two: text, and image-reference
   *     pills (which only come from clicking the canvas, never from paste).
   *     So plain-text is the right whitelist. */
  const onPaste = (e: React.ClipboardEvent) => {
    const cd = e.clipboardData;
    if (!cd) return;

    const item = [...(cd.items ?? [])].find((it) => it.type.startsWith("image/"));
    if (item) {
      e.preventDefault();
      e.stopPropagation();
      const file = item.getAsFile();
      if (!file) return;
      void (async () => {
        const bytes = new Uint8Array(await file.arrayBuffer());
        const ext = item.type.split("/")[1] || "png";
        const placements = await useBoardStore.getState().importBytes(bytes, ext, "pasted");
        insertRefs(placements.map((p) => p.id));
      })();
      return;
    }

    // Plain-text only — strip HTML / styles. `insertText` (deprecated but
    // universally supported in 2026) preserves the undo stack and respects
    // selection/range correctly; the Selection-API equivalent is uglier and
    // has edge cases around adjacent text nodes.
    e.preventDefault();
    e.stopPropagation();
    const text = cd.getData("text/plain");
    if (!text) return;
    document.execCommand("insertText", false, text);
    refreshHasContent();
  };

  // Save the caret whenever it's inside the editor (anchor for ghosts).
  useEffect(() => {
    const onSel = () => {
      if (syncing.current) return;
      const sel = window.getSelection();
      const editor = editorRef.current;
      if (!sel || !editor || sel.rangeCount === 0) return;
      const r = sel.getRangeAt(0);
      if (editor.contains(r.startContainer)) lastRange.current = r.cloneRange();
    };
    document.addEventListener("selectionchange", onSel);
    return () => document.removeEventListener("selectionchange", onSel);
  }, []);

  // Ghost pills follow the canvas selection.
  useEffect(() => {
    syncGhosts([...useBoardStore.getState().selection]);
    return useBoardStore.subscribe((s, p) => {
      if (s.selection !== p.selection) syncGhosts([...s.selection]);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── dispatch ───────────────────────────────────────────────────────────
  const extract = (): { text: string; refs: string[] } => {
    const editor = editorRef.current;
    if (!editor) return { text: "", refs: [] };
    let text = "";
    const refs: string[] = [];
    editor.childNodes.forEach((node) => {
      if (node.nodeType === Node.TEXT_NODE) {
        text += node.nodeValue ?? "";
      } else if (node.nodeType === Node.ELEMENT_NODE) {
        const el = node as HTMLElement;
        if (el.classList.contains("cm-pill")) {
          const pid = el.dataset.pid;
          if (pid) {
            refs.push(pid);
            const r = resolve(pid);
            text += r ? ` ${r.path} ` : " ";
          }
        } else if (el.tagName === "BR") {
          text += "\n";
        } else {
          text += el.textContent ?? "";
        }
      }
    });
    return { text: text.replace(/[ \t]+/g, " ").trim(), refs };
  };

  const clearEditor = () => {
    if (editorRef.current) editorRef.current.innerHTML = "";
    lastRange.current = null;
    refreshHasContent();
  };

  const dispatch = () => {
    if (!ready || !boardId) return;
    commitGhosts();
    const { text, refs } = extract();
    // Any image carrying marks is auto-referenced this turn (marks → context).
    const annotated = annotatedImages();
    const allRefs = [...refs, ...annotated.filter((id) => !refs.includes(id))];
    // Mark notes go at the START of the message (formatted), then the free text.
    const notes = buildMarkNotes(allRefs);
    const instruction = [notes, text].filter((s) => s.trim()).join("\n\n");
    if (!instruction.trim() && allRefs.length === 0) return;
    const turn = useChatStore.getState().startTurn(instruction, allRefs);
    clearEditor();
    void (async () => {
      try {
        const overlays = await buildOverlays(boardId, allRefs);
        await ipc.sendMessage(boardId, instruction, allRefs, overlays);
        // Consume marks only after the turn was actually sent (not on failure).
        useBoardStore.getState().consumeMarks(allRefs);
      } catch (e) {
        const message = sendErrorMessage(e);
        useChatStore.getState().failTurn(message, turn);
        void ipc.frontLog("error", message).catch(() => {});
      }
    })();
    // A still-selected canvas image re-shows its ghost for the next turn.
    syncGhosts([...useBoardStore.getState().selection]);
  };

  // Stop goes through the chat store's `stopTurn` rather than calling
  // ipc.interruptTurn directly — that path guarantees the UI exits "running"
  // state even when codex ignores the interrupt RPC (graceful → tree-kill
  // escalation; see chat.ts).
  const stop = () => {
    useChatStore.getState().stopTurn();
  };

  const placeholder = ready
    ? t("composer.placeholder")
    : sessionStatus === "starting"
      ? t("composer.starting")
      : t("composer.notReady");

  // External → Composer one-shot injections (Gallery prompt OR chat
  // inline-image pill). Watching `nonce` (not the payloads themselves) makes
  // repeat injections of the same value re-fire.
  const pendingNonce = useComposerStore((s) => s.nonce);
  useEffect(() => {
    const { pendingPrompt, pendingPill, consume } = useComposerStore.getState();
    const editor = editorRef.current;
    if (!editor) return;

    if (pendingPrompt) {
      const existing = editor.textContent ?? "";
      const sep = existing && !existing.endsWith("\n") ? "\n\n" : "";
      editor.textContent = existing + sep + pendingPrompt;
      setHasContent((editor.textContent ?? "").length > 0);
      editor.focus();
      const r = document.createRange();
      r.selectNodeContents(editor);
      r.collapse(false);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(r);
      consume();
      return;
    }

    if (pendingPill) {
      // Mirror the canvas-click path: append a committed (non-ghost) pill at
      // the END of the editor (after whatever the user has typed). Same DOM
      // shape as makePill, plus contentEditable=false so backspace deletes
      // it as one atom.
      editor.focus();
      const pill = makePill(pendingPill, false);
      // Move caret to end, then insert the pill + a trailing space so the
      // user can immediately keep typing.
      const range = document.createRange();
      range.selectNodeContents(editor);
      range.collapse(false);
      range.insertNode(pill);
      range.setStartAfter(pill);
      const trailingSpace = document.createTextNode(" ");
      range.insertNode(trailingSpace);
      range.setStartAfter(trailingSpace);
      range.collapse(true);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(range);
      setHasContent((editor.textContent ?? "").length > 0);
      consume();
    }
    // makePill is intentionally not a dep — it's a closure that reads
    // useBoardStore via getState() and is effectively stable; adding it
    // would fire this effect on every render, which is wrong.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingNonce]);

  return (
    <div className="cm-composer">
      <GalleryButton />
      <div className="cm-composer__box">
        <div
          ref={editorRef}
          className="cm-rich"
          contentEditable
          role="textbox"
          aria-multiline="true"
          data-placeholder={placeholder}
          suppressContentEditableWarning
          onMouseDown={onEditorMouseDown}
          onFocus={commitGhosts}
          onInput={refreshHasContent}
          onPaste={onPaste}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              dispatch();
            }
          }}
        />
        <div className="cm-composer__bar">
          <button
            className="cm-composer__add"
            data-tip={t("composer.addImages")}
            aria-label={t("composer.addImages")}
            disabled={!boardId}
            onClick={() => void addImages()}
          >
            <ImagePlus size={17} />
          </button>
          <div className="cm-composer__barspacer" />
          {running && (
            <button className="cm-send cm-send--stop" title={t("composer.stop")} onClick={stop}>
              <Square size={12} fill="currentColor" strokeWidth={0} />
            </button>
          )}
          <button
            className="cm-send"
            title={ready ? t("composer.send") : t("composer.notReady")}
            onClick={() => dispatch()}
            disabled={!ready || !hasContent}
          >
            <ArrowUp size={18} />
          </button>
        </div>
      </div>
    </div>
  );
}
