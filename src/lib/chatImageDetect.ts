/**
 * Detect image references inside an assistant message's streamed text.
 *
 * Two recognition modes (decision Q1 = "B"):
 *
 *   • **Markdown**: `![alt](path)` (image syntax) AND `[text](path)` (plain
 *     link syntax) — both accepted when `path` ends in a known image
 *     extension. Models often emit the link form instead of the image form,
 *     and we still want the path consumed (otherwise the brackets and parens
 *     leak as raw text around the inline thumbnail). Closing `)` required so
 *     mid-stream partials like `[foo](./bar.` never half-render (Q5).
 *
 *   • **Plain path**: any whitespace-bounded token ending in a known image
 *     extension, including Windows absolute paths (`C:\...\x.png`),
 *     slash/backslash relative paths, and bare workspace filenames. The Rust
 *     resolver decides whether the file actually exists in the Board folder.
 *
 * Returns ordered, non-overlapping ranges into the source string so callers
 * can slice the original text and render each segment as either plain text
 * or an inline image card.
 */

export interface ImageRef {
  /** Start index in the source text (inclusive). */
  start: number;
  /** End index (exclusive). */
  end: number;
  kind: "markdown" | "plain";
  /** The raw path string as written by the model. May be relative, absolute,
   *  or tilde-prefixed — resolution happens server-side via `resolve_chat_image`. */
  path: string;
  /** Markdown alt text (kind === "markdown" only). */
  alt?: string;
}

const IMAGE_EXTS = "(?:png|jpe?g|webp|gif|bmp|tiff?|avif)";

// Markdown image / link to image: `![alt](image.png)` or `[text](image.png)`.
// `!?` allows both forms; the path *must* end in a known image extension so
// non-image links (`[docs](readme.md)`) don't get pulled in. Closing `)` is
// required (Q5: never half-render). `[^)]+` stops at the first `)` so the
// match doesn't run away across the rest of the message.
const MD_RE = new RegExp(
  String.raw`!?\[([^\]]*)\]\(([^)]+\.${IMAGE_EXTS})\)`,
  "gi",
);

const PATH_BODY = String.raw`[^\s\`"'()<>，。！？；：、]`;

// Plain path token: accepts Windows `C:\...`, slash/backslash paths, and bare
// workspace filenames. Capture the leading boundary instead of using lookbehind
// so start/end offsets stay explicit and robust across webviews.
const PLAIN_RE = new RegExp(
  String.raw`(^|[\s\`"'(（])((?:[A-Za-z]:[\\/]|~[\\/]|\.{1,2}[\\/]|${PATH_BODY}*[\\/])?${PATH_BODY}*\.${IMAGE_EXTS})(?=[\s\`"'),.!?:;，。！？；：、）\]]|$)`,
  "gi",
);

export function extractImageRefs(text: string): ImageRef[] {
  const refs: ImageRef[] = [];

  // Markdown first — they take precedence over plain (so the path inside `()`
  // is never re-matched as a plain ref).
  for (const m of text.matchAll(MD_RE)) {
    const idx = m.index ?? 0;
    refs.push({
      start: idx,
      end: idx + m[0].length,
      kind: "markdown",
      path: m[2].trim(),
      alt: m[1],
    });
  }

  // Plain — skip anything overlapping a markdown match.
  for (const m of text.matchAll(PLAIN_RE)) {
    const idx = (m.index ?? 0) + m[1].length;
    const start = idx;
    const end = start + m[2].length;
    if (refs.some((r) => r.kind === "markdown" && start < r.end && end > r.start)) continue;
    refs.push({ start, end, kind: "plain", path: m[2] });
  }

  refs.sort((a, b) => a.start - b.start);
  return refs;
}
