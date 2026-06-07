import { useEffect, useMemo, useRef, useState } from "react";
import { ImagePlus, ArrowUp, Square, Package } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { useT } from "../i18n/locale";
import { useBoardStore } from "../store/board";
import { useChatStore } from "../store/chat";
import { useComposerStore, type PendingFrameRef } from "../store/composer";
import { cameoUrl, ipc } from "../lib/ipc";
import { stillPathOf } from "../lib/media";
import { buildOverlays, buildMarkNotes, annotatedImages } from "../lib/overlay";
import { GalleryButton } from "./gallery/GalleryButton";
import { GenSettingsMenu } from "./GenSettingsMenu";
import { useGenStore } from "../store/genSettings";
import { usePermissionStore } from "../store/permissionSettings";
import type { CodexSkillInfo, CodexSkillRef } from "../types";

function stem(path: string): string {
  const base = path.split(/[/\\]/).pop() ?? path;
  const s = base.replace(/\.[^.]+$/, "");
  return s.length > 14 ? s.slice(0, 13) + "…" : s;
}

const IMAGE_EXTS = ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "avif"];
const SKILL_MENU_LIMIT = 9;
const CAMEO_SKILL_ALLOWLIST = new Set(["imagegen"]);
const CARET_SENTINEL = "\u200b";
const SVG_NS = "http://www.w3.org/2000/svg";

function sendErrorMessage(e: unknown): string {
  return `Could not send this turn: ${e instanceof Error ? e.message : String(e)}`;
}

function skillDescription(skill: CodexSkillInfo): string {
  return skill.shortDescription || skill.description;
}

function scopeRank(scope: string): number {
  if (scope === "repo") return 0;
  if (scope === "user") return 1;
  if (scope === "system") return 2;
  return 3;
}

function skillMatches(skill: CodexSkillInfo, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [skill.name, skill.displayName, skillDescription(skill)]
    .some((value) => value.toLowerCase().includes(q));
}

function sortSkills(skills: CodexSkillInfo[], query: string): CodexSkillInfo[] {
  const q = query.trim().toLowerCase();
  return [...skills].sort((a, b) => {
    const rank = (s: CodexSkillInfo) => {
      const name = s.name.toLowerCase();
      const display = s.displayName.toLowerCase();
      if (q && (name.startsWith(q) || display.startsWith(q))) return -2;
      if (CAMEO_SKILL_ALLOWLIST.has(s.name)) return -1;
      return scopeRank(s.scope);
    };
    const byRank = rank(a) - rank(b);
    if (byRank !== 0) return byRank;
    return a.displayName.localeCompare(b.displayName);
  });
}

function isBlankText(value: string): boolean {
  return value.replace(/\u00a0/g, " ").replace(/\u200b/g, "").trim().length === 0;
}

function isPillNode(node: Node | null): node is HTMLElement {
  return node instanceof HTMLElement && node.classList.contains("cm-pill");
}

function isBlankTextNode(node: Node | null): node is Text {
  return node?.nodeType === Node.TEXT_NODE && isBlankText(node.nodeValue ?? "");
}

/** The pill immediately before a (collapsed) position, skipping blank text /
 *  caret sentinels. Null when real text — or nothing — precedes. Used to avoid
 *  inserting a reference right next to an identical one (a consecutive
 *  duplicate); the same item elsewhere in the text is fine. */
function pillBefore(container: Node, offset: number): HTMLElement | null {
  let node: Node | null;
  if (container.nodeType === Node.TEXT_NODE) {
    if (!isBlankText((container.nodeValue ?? "").slice(0, offset))) return null;
    node = container.previousSibling;
  } else {
    node = container.childNodes[offset - 1] ?? null;
  }
  while (node && isBlankTextNode(node)) node = node.previousSibling;
  return isPillNode(node) ? node : null;
}

/** The trailing pill at the very end of the editor (skipping blank text). */
function trailingPill(editor: HTMLElement): HTMLElement | null {
  let node: ChildNode | null = editor.lastChild;
  while (node && isBlankTextNode(node)) node = node.previousSibling;
  return isPillNode(node) ? node : null;
}

function editorHasMeaningfulContent(editor: HTMLElement): boolean {
  for (const node of editor.childNodes) {
    if (node.nodeType === Node.TEXT_NODE) {
      if (!isBlankText(node.nodeValue ?? "")) return true;
      continue;
    }
    if (node.nodeType !== Node.ELEMENT_NODE) continue;
    const el = node as HTMLElement;
    if (el.tagName === "BR") continue;
    if (el.classList.contains("cm-pill")) return true;
    if (!isBlankText(el.textContent ?? "")) return true;
  }
  return false;
}

function normalizeEmptyEditor(editor: HTMLElement): void {
  if (!editorHasMeaningfulContent(editor)) editor.innerHTML = "";
}

function cleanExtractedText(text: string): string {
  return text.replace(/\u200b/g, "").replace(/\u00a0/g, " ").replace(/[ \t]+/g, " ").trim();
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
  const composerRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<HTMLDivElement>(null);
  const lastRange = useRef<Range | null>(null);
  const skillsRequestRef = useRef<{ boardId: string; seq: number } | null>(null);
  const skillRequestSeqRef = useRef(0);
  const syncing = useRef(false);

  const sessionStatus = useChatStore((s) => s.sessionStatus);
  const turnStatus = useChatStore((s) => s.turnStatus);
  const boardId = useBoardStore((s) => s.boardId);
  const [hasContent, setHasContent] = useState(false);
  const [skills, setSkills] = useState<CodexSkillInfo[]>([]);
  const [skillsLoadedFor, setSkillsLoadedFor] = useState<string | null>(null);
  const [skillsLoading, setSkillsLoading] = useState(false);
  const [skillsError, setSkillsError] = useState<string | null>(null);
  const [skillMenu, setSkillMenu] = useState({ open: false, query: "", selected: 0 });
  const slashRangeRef = useRef<Range | null>(null);
  const t = useT();

  const ready = !!boardId && sessionStatus === "ready";
  const running = turnStatus === "running";

  // ── pill resolution ────────────────────────────────────────────────────
  // `path` is the reference inlined into the sent text (the real file — a video
  // stays its .mp4 so Codex reads/ffprobes it). `url` is just the pill's <img>
  // thumbnail, so use the still (a video's poster) to avoid a broken-image pill.
  const resolve = (pid: string): { path: string; url: string } | null => {
    const { placements, assets, boardId } = useBoardStore.getState();
    const p = placements.get(pid);
    const a = p && assets.get(p.assetId);
    return a && boardId ? { path: a.path, url: cameoUrl(boardId, stillPathOf(a)) } : null;
  };

  const makePill = (pid: string, ghost: boolean, frame?: PendingFrameRef): HTMLSpanElement => {
    const span = document.createElement("span");
    span.className = "cm-pill" + (ghost ? " cm-pill--ghost" : "");
    span.contentEditable = "false";
    span.dataset.pid = pid;
    if (ghost) span.dataset.ghost = "1";
    // A frame reference carries the extracted still + timestamp so `extract`
    // can point the agent at that exact moment (PRD §17/F2).
    if (frame) {
      span.dataset.frame = frame.path;
      span.dataset.label = frame.label;
      span.dataset.atms = String(frame.atMs);
    }
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
    if (frame) {
      const ts = document.createElement("span");
      ts.className = "cm-pill__ts";
      ts.textContent = frame.label;
      span.appendChild(ts);
    }
    return span;
  };

  const makeSkillPill = (skill: CodexSkillInfo): HTMLSpanElement => {
    const span = document.createElement("span");
    span.className = "cm-pill cm-pill--skill";
    span.contentEditable = "false";
    span.dataset.skillName = skill.name;
    span.dataset.skillPath = skill.path;
    const icon = document.createElementNS(SVG_NS, "svg");
    icon.classList.add("cm-pill__icon");
    icon.setAttribute("viewBox", "0 0 24 24");
    icon.setAttribute("aria-hidden", "true");
    icon.setAttribute("fill", "none");
    icon.setAttribute("stroke", "currentColor");
    icon.setAttribute("stroke-width", "2");
    icon.setAttribute("stroke-linecap", "round");
    icon.setAttribute("stroke-linejoin", "round");
    [
      "m7.5 4.27 9 5.15",
      "M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z",
      "m3.3 7 8.7 5 8.7-5",
      "M12 22V12",
    ].forEach((d) => {
      const path = document.createElementNS(SVG_NS, "path");
      path.setAttribute("d", d);
      icon.appendChild(path);
    });
    span.appendChild(icon);
    const label = document.createElement("span");
    label.className = "cm-pill__label";
    label.textContent = skill.displayName || skill.name;
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
    setHasContent(!!editor && editorHasMeaningfulContent(editor));
  };

  const closeSkillMenu = () => {
    slashRangeRef.current = null;
    setSkillMenu({ open: false, query: "", selected: 0 });
  };

  const ensureSkills = (forceReload = false) => {
    if (!boardId || sessionStatus !== "ready") return;
    if (!forceReload && skillsLoadedFor === boardId) return;
    if (!forceReload && skillsRequestRef.current?.boardId === boardId) return;
    const requestBoardId = boardId;
    const seq = ++skillRequestSeqRef.current;
    skillsRequestRef.current = { boardId: requestBoardId, seq };
    setSkillsLoading(true);
    setSkillsError(null);
    void ipc
      .listSkills(requestBoardId, forceReload)
      .then((list) => {
        if (skillsRequestRef.current?.seq !== seq || useBoardStore.getState().boardId !== requestBoardId) return;
        setSkills(list);
        setSkillsLoadedFor(requestBoardId);
      })
      .catch((e) => {
        if (skillsRequestRef.current?.seq !== seq || useBoardStore.getState().boardId !== requestBoardId) return;
        setSkillsError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (skillsRequestRef.current?.seq !== seq) return;
        skillsRequestRef.current = null;
        setSkillsLoading(false);
      });
  };

  const detectSlashRange = (): { query: string; range: Range } | null => {
    const editor = editorRef.current;
    const sel = window.getSelection();
    if (!editor || !sel || sel.rangeCount === 0) return null;
    const range = sel.getRangeAt(0);
    if (!range.collapsed || !editor.contains(range.startContainer)) return null;
    if (range.startContainer.nodeType !== Node.TEXT_NODE) return null;
    const node = range.startContainer;
    const text = node.nodeValue ?? "";
    const before = text.slice(0, range.startOffset);
    const match = /(?:^|\s)\/([^\s/]*)$/.exec(before.replace(/\u200b/g, " "));
    if (!match) return null;
    const slashIndex = before.lastIndexOf("/");
    if (slashIndex < 0) return null;
    const replace = range.cloneRange();
    replace.setStart(node, slashIndex);
    return { query: match[1], range: replace };
  };

  const refreshSkillMenu = () => {
    const next = detectSlashRange();
    if (!next) {
      slashRangeRef.current = null;
      setSkillMenu((s) => (s.open ? { open: false, query: "", selected: 0 } : s));
      return;
    }
    slashRangeRef.current = next.range;
    ensureSkills(false);
    setSkillMenu((s) => ({
      open: true,
      query: next.query,
      selected: s.query === next.query ? s.selected : 0,
    }));
  };

  /** Move the visible caret to `range` (collapsed). */
  const setCaret = (range: Range) => {
    const sel = window.getSelection();
    if (!sel) return;
    sel.removeAllRanges();
    sel.addRange(range);
  };

  const caretRangeAfterAtomic = (node: ChildNode): Range | null => {
    const parent = node.parentNode;
    if (!parent) return null;
    const range = document.createRange();
    const next = node.nextSibling;

    if (next?.nodeType === Node.TEXT_NODE) {
      const text = next as Text;
      if (isBlankText(text.nodeValue ?? "")) {
        text.nodeValue = CARET_SENTINEL;
        range.setStart(text, CARET_SENTINEL.length);
      } else {
        range.setStart(text, 0);
      }
    } else {
      const sink = document.createTextNode(CARET_SENTINEL);
      parent.insertBefore(sink, next);
      range.setStart(sink, CARET_SENTINEL.length);
    }

    range.collapse(true);
    lastRange.current = range.cloneRange();
    return range;
  };

  /** Drop the caret at the very end of the editor (after the pills). */
  const moveCaretToEnd = () => {
    const editor = editorRef.current;
    if (!editor) return;
    editor.focus();
    const last = editor.lastChild;
    if (isPillNode(last)) {
      const r = caretRangeAfterAtomic(last);
      if (r) setCaret(r);
      return;
    }
    const r = document.createRange();
    r.selectNodeContents(editor);
    r.collapse(false);
    lastRange.current = r.cloneRange();
    setCaret(r);
  };

  const setCaretAfterNodeRemoval = (parent: Node, next: ChildNode | null) => {
    const range = document.createRange();
    if (next && next.parentNode === parent) {
      range.setStartBefore(next);
    } else {
      range.selectNodeContents(parent);
      range.collapse(false);
    }
    lastRange.current = range.cloneRange();
    setCaret(range);
  };

  const removeAdjacentPill = (direction: "backward" | "forward"): boolean => {
    const editor = editorRef.current;
    const sel = window.getSelection();
    if (!editor || !sel || sel.rangeCount === 0) return false;
    const range = sel.getRangeAt(0);
    if (!range.collapsed || !editor.contains(range.startContainer)) return false;

    let pill: HTMLElement | null = null;
    let spacer: Text | null = null;
    const { startContainer, startOffset } = range;

    if (startContainer.nodeType === Node.TEXT_NODE) {
      const text = startContainer as Text;
      if (direction === "backward") {
        if (startOffset === 0 && isPillNode(text.previousSibling)) pill = text.previousSibling;
        if (!pill && isBlankText(text.nodeValue ?? "") && isPillNode(text.previousSibling)) {
          pill = text.previousSibling;
          spacer = text;
        }
      } else {
        if (startOffset === text.length && isPillNode(text.nextSibling)) pill = text.nextSibling;
        if (!pill && isBlankText(text.nodeValue ?? "") && isPillNode(text.nextSibling)) {
          pill = text.nextSibling;
          spacer = text;
        }
      }
    } else if (startContainer === editor) {
      const child = editor.childNodes[direction === "backward" ? startOffset - 1 : startOffset] ?? null;
      if (isPillNode(child)) {
        pill = child;
      } else if (isBlankTextNode(child)) {
        const neighbor = direction === "backward" ? child.previousSibling : child.nextSibling;
        if (isPillNode(neighbor)) {
          pill = neighbor;
          spacer = child;
        }
      }
    }

    if (!pill) return false;
    const parent = pill.parentNode;
    if (!parent) return false;
    const next = pill.nextSibling;
    spacer?.remove();
    pill.remove();
    normalizeEmptyEditor(editor);
    setCaretAfterNodeRemoval(parent, next);
    closeSkillMenu();
    refreshHasContent();
    return true;
  };

  /** Clicking the box's empty area drops the caret AFTER the pills (they're
   *  atomic input entities) rather than letting the browser snap it before one.
   *  Clicks on real text or directly on a pill are left to the browser. */
  const onEditorMouseDown = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest(".cm-pill")) return;
    const r = document.caretRangeFromPoint(e.clientX, e.clientY);
    if (r && r.startContainer.nodeType === Node.TEXT_NODE && !isBlankText(r.startContainer.textContent ?? "")) return;
    e.preventDefault();
    moveCaretToEnd();
  };

  const syncGhosts = (ids: string[]) => {
    const editor = editorRef.current;
    if (!editor) return;
    syncing.current = true;
    editor.querySelectorAll("[data-ghost]").forEach((g) => g.remove());
    normalizeEmptyEditor(editor);
    if (ids.length) {
      const range = anchorRange(editor);
      let lastPill: HTMLSpanElement | null = null;
      for (const id of ids) {
        // Don't add a ghost that would sit right after an identical reference
        // (e.g. re-clicking the image you just referenced) — consecutive dupes
        // are avoided; the same item elsewhere in the text is fine.
        if (pillBefore(range.startContainer, range.startOffset)?.dataset.pid === id) continue;
        const pill = makePill(id, true);
        range.insertNode(pill);
        range.setStartAfter(pill);
        range.collapse(true);
        lastPill = pill;
      }
      const caret = lastPill ? caretRangeAfterAtomic(lastPill) : range.cloneRange();
      // Keep the caret to the RIGHT of the inserted reference(s): remember it as
      // the anchor, and move the visible caret there if the editor is focused.
      if (caret) lastRange.current = caret.cloneRange();
      if (document.activeElement === editor && caret) setCaret(caret.cloneRange());
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
      const r = caretRangeAfterAtomic(last);
      // On focus, drop the caret to the RIGHT of the just-committed reference
      // so typing continues after it (browser would otherwise leave it before).
      if (r) setCaret(r.cloneRange());
    }
    refreshHasContent();
  };

  /** Insert COMMITTED (solid) reference pills for newly added images. */
  const insertRefs = (ids: string[]) => {
    const editor = editorRef.current;
    if (!editor || ids.length === 0) return;
    editor.focus();
    normalizeEmptyEditor(editor);
    const range = anchorRange(editor);
    let lastPill: HTMLSpanElement | null = null;
    for (const id of ids) {
      const pill = makePill(id, false);
      range.insertNode(pill);
      range.setStartAfter(pill);
      range.collapse(true);
      lastPill = pill;
    }
    const caret = lastPill ? caretRangeAfterAtomic(lastPill) : range.cloneRange();
    if (caret) {
      lastRange.current = caret.cloneRange();
      setCaret(caret.cloneRange());
    }
    refreshHasContent();
  };

  const insertSkill = (skill: CodexSkillInfo) => {
    const editor = editorRef.current;
    if (!editor) return;
    editor.focus();
    const range = slashRangeRef.current?.cloneRange() ?? anchorRange(editor);
    range.deleteContents();
    const pill = makeSkillPill(skill);
    range.insertNode(pill);
    const caret = caretRangeAfterAtomic(pill);
    if (caret) setCaret(caret.cloneRange());
    slashRangeRef.current = null;
    setSkillMenu({ open: false, query: "", selected: 0 });
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
    closeSkillMenu();
    const cd = e.clipboardData;
    if (!cd) return;

    const item = [...(cd.items ?? [])].find(
      (it) => it.type.startsWith("image/") || it.type.startsWith("video/"),
    );
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

  // Input-box settings: load the Board's saved choices on board change; fetch
  // the model list once the session is ready (model/list needs a live app-server).
  useEffect(() => {
    if (!boardId) return;
    void useGenStore.getState().load(boardId);
    void usePermissionStore.getState().load(boardId);
  }, [boardId]);
  useEffect(() => {
    if (boardId && sessionStatus === "ready") void useGenStore.getState().fetchModels(boardId);
  }, [boardId, sessionStatus]);
  useEffect(() => {
    closeSkillMenu();
    skillsRequestRef.current = null;
    setSkills([]);
    setSkillsLoadedFor(null);
    setSkillsLoading(false);
    setSkillsError(null);
  }, [boardId]);

  useEffect(() => {
    if (!skillMenu.open) return;
    const onPointerDown = (e: PointerEvent) => {
      const root = composerRef.current;
      if (root && e.target instanceof Node && root.contains(e.target)) return;
      closeSkillMenu();
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    return () => window.removeEventListener("pointerdown", onPointerDown, true);
  }, [skillMenu.open]);

  const skillResults = useMemo(() => {
    const q = skillMenu.query.trim();
    const visible = q
      ? skills.filter((skill) => skillMatches(skill, q))
      : skills.filter((skill) => skill.scope === "repo" || CAMEO_SKILL_ALLOWLIST.has(skill.name));
    return sortSkills(visible, q).slice(0, SKILL_MENU_LIMIT);
  }, [skillMenu.query, skills]);

  useEffect(() => {
    if (!skillMenu.open || skillResults.length === 0) return;
    if (skillMenu.selected >= skillResults.length) {
      setSkillMenu((s) => ({ ...s, selected: skillResults.length - 1 }));
    }
  }, [skillMenu.open, skillMenu.selected, skillResults.length]);

  // ── dispatch ───────────────────────────────────────────────────────────
  const extract = (): { text: string; refs: string[]; skills: CodexSkillRef[] } => {
    const editor = editorRef.current;
    if (!editor) return { text: "", refs: [], skills: [] };
    let text = "";
    const refs: string[] = [];
    const selectedSkills: CodexSkillRef[] = [];
    editor.childNodes.forEach((node) => {
      if (node.nodeType === Node.TEXT_NODE) {
        text += node.nodeValue ?? "";
      } else if (node.nodeType === Node.ELEMENT_NODE) {
        const el = node as HTMLElement;
        if (el.classList.contains("cm-pill--skill")) {
          const name = el.dataset.skillName;
          const path = el.dataset.skillPath;
          if (name && path) selectedSkills.push({ name, path });
          text += " ";
        } else if (el.classList.contains("cm-pill")) {
          const pid = el.dataset.pid;
          if (pid) {
            refs.push(pid);
            const r = resolve(pid);
            const framePath = el.dataset.frame;
            const label = el.dataset.label;
            if (framePath && r) {
              // Frame reference: point the agent at the exact still plus the
              // source video + timestamp (it can read the still AND ffmpeg the
              // video). The video path itself is also listed in the reference
              // block build_turn_prompt prepends (Rust side).
              text += ` (the frame at ${label ?? ""} of ${r.path}, saved as an image at ${framePath} — read it to see exactly what I mean) `;
            } else {
              text += r ? ` ${r.path} ` : " ";
            }
          }
        } else if (el.tagName === "BR") {
          text += "\n";
        } else {
          text += el.textContent ?? "";
        }
      }
    });
    return { text: cleanExtractedText(text), refs, skills: selectedSkills };
  };

  const clearEditor = () => {
    if (editorRef.current) editorRef.current.innerHTML = "";
    lastRange.current = null;
    closeSkillMenu();
    refreshHasContent();
  };

  const dispatch = () => {
    if (!ready || !boardId) return;
    commitGhosts();
    const { text, refs, skills: selectedSkills } = extract();
    // Any image carrying marks is auto-referenced this turn (marks → context).
    const annotated = annotatedImages();
    const allRefs = [...refs, ...annotated.filter((id) => !refs.includes(id))];
    // Mark notes go at the START of the message (formatted), then the free text.
    const notes = buildMarkNotes(allRefs);
    const instruction = [notes, text].filter((s) => s.trim()).join("\n\n");
    if (!instruction.trim() && allRefs.length === 0 && selectedSkills.length === 0) return;
    const skillLabel = selectedSkills.map((skill) => `/${skill.name}`).join(" ");
    const visibleInstruction = [skillLabel, instruction].filter((s) => s.trim()).join("\n\n");
    const turn = useChatStore.getState().startTurn(visibleInstruction, allRefs);
    clearEditor();
    void (async () => {
      try {
        const overlays = await buildOverlays(boardId, allRefs);
        await ipc.sendMessage(boardId, instruction, allRefs, overlays, selectedSkills);
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

  const skillScopeLabel = (scope: string) => {
    if (scope === "repo") return t("composer.skills.scope.repo");
    if (scope === "user") return t("composer.skills.scope.user");
    if (scope === "system") return t("composer.skills.scope.system");
    if (scope === "admin") return t("composer.skills.scope.admin");
    return scope;
  };

  // External → Composer one-shot injections (Gallery prompt OR chat
  // inline-image pill). Watching `nonce` (not the payloads themselves) makes
  // repeat injections of the same value re-fire.
  const pendingNonce = useComposerStore((s) => s.nonce);
  useEffect(() => {
    const { pendingPrompt, pendingPill, consume } = useComposerStore.getState();
    const editor = editorRef.current;
    if (!editor) return;
    normalizeEmptyEditor(editor);

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
      editor.focus(); // commits the selection's auto-ghost (incl. this video's)
      // If that leaves an identical reference as the trailing pill (the
      // just-committed whole-file ghost of the video being referenced),
      // replace it so "Reference" yields ONE pill reflecting the current state
      // (whole file, or the scrubbed frame) instead of two adjacent dupes.
      const trailing = trailingPill(editor);
      if (trailing && trailing.dataset.pid === pendingPill.placementId) trailing.remove();
      normalizeEmptyEditor(editor);
      const pill = makePill(pendingPill.placementId, false, pendingPill.frame);
      // Move caret to end, then insert the pill + a trailing space so the
      // user can immediately keep typing.
      const range = document.createRange();
      range.selectNodeContents(editor);
      range.collapse(false);
      range.insertNode(pill);
      const caret = caretRangeAfterAtomic(pill);
      const sel = window.getSelection();
      if (caret) {
        sel?.removeAllRanges();
        sel?.addRange(caret);
      }
      refreshHasContent();
      consume();
    }
    // makePill is intentionally not a dep — it's a closure that reads
    // useBoardStore via getState() and is effectively stable; adding it
    // would fire this effect on every render, which is wrong.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingNonce]);

  return (
    <div ref={composerRef} className="cm-composer">
      <GalleryButton />
      <div className="cm-composer__box">
        {skillMenu.open && (
          <div className="cm-skill-menu" role="listbox" aria-label={t("composer.skills.menu")}>
            {skillsLoading && skillResults.length === 0 ? (
              <div className="cm-skill-menu__state">{t("composer.skills.loading")}</div>
            ) : skillsError ? (
              <div className="cm-skill-menu__state cm-skill-menu__state--error">
                {t("composer.skills.error")}
              </div>
            ) : skillResults.length === 0 ? (
              <div className="cm-skill-menu__state">{t("composer.skills.empty")}</div>
            ) : (
              skillResults.map((skill, idx) => (
                <button
                  key={`${skill.name}:${skill.path}`}
                  type="button"
                  className={`cm-skill-menu__item${idx === skillMenu.selected ? " is-active" : ""}`}
                  role="option"
                  aria-selected={idx === skillMenu.selected}
                  onMouseEnter={() => setSkillMenu((s) => ({ ...s, selected: idx }))}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    insertSkill(skill);
                  }}
                >
                  <Package size={15} className="cm-skill-menu__icon" />
                  <span className="cm-skill-menu__main">
                    <span className="cm-skill-menu__name">{skill.displayName || skill.name}</span>
                    <span className="cm-skill-menu__desc">{skillDescription(skill)}</span>
                  </span>
                  <span className="cm-skill-menu__scope">{skillScopeLabel(skill.scope)}</span>
                </button>
              ))
            )}
          </div>
        )}
        <div
          ref={editorRef}
          className="cm-rich"
          contentEditable
          role="textbox"
          aria-multiline="true"
          data-placeholder={placeholder}
          data-empty={hasContent ? undefined : "true"}
          suppressContentEditableWarning
          onMouseDown={onEditorMouseDown}
          onFocus={commitGhosts}
          onInput={() => {
            refreshHasContent();
            refreshSkillMenu();
          }}
          onPaste={onPaste}
          onKeyDown={(e) => {
            if (skillMenu.open) {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setSkillMenu((s) => ({
                  ...s,
                  selected: skillResults.length ? (s.selected + 1) % skillResults.length : 0,
                }));
                return;
              }
              if (e.key === "ArrowUp") {
                e.preventDefault();
                setSkillMenu((s) => ({
                  ...s,
                  selected: skillResults.length
                    ? (s.selected - 1 + skillResults.length) % skillResults.length
                    : 0,
                }));
                return;
              }
              if (e.key === "Escape") {
                e.preventDefault();
                closeSkillMenu();
                return;
              }
              if (e.key === "Enter" || e.key === "Tab") {
                e.preventDefault();
                const skill = skillResults[skillMenu.selected];
                if (skill) {
                  insertSkill(skill);
                }
                return;
              }
            }
            if (e.key === "Backspace" && removeAdjacentPill("backward")) {
              e.preventDefault();
              return;
            }
            if (e.key === "Delete" && removeAdjacentPill("forward")) {
              e.preventDefault();
              return;
            }
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
          <GenSettingsMenu />
          {running ? (
            <button className="cm-send cm-send--stop" title={t("composer.stop")} onClick={stop}>
              <Square size={12} fill="currentColor" strokeWidth={0} />
            </button>
          ) : (
            <button className="cm-send" title={t("composer.send")} onClick={() => dispatch()} disabled={!ready || !hasContent}>
              <ArrowUp size={18} />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
