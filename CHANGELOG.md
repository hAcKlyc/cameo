# Changelog

User-facing release notes for Cameo. This file describes what changed from a
user's point of view, rather than listing internal implementation details.

## [0.1.6] - 2026-05-30

### Improvements

- A new **Codex setup panel** makes getting started clearer: Cameo tells you
  whether the Codex CLI is installed and signed in, and gives one-tap guidance
  (install / `codex login`) with a re-check button when something's missing.
- You can now choose how Codex generates right from the input box — pick the
  **model**, the **intelligence** (reasoning effort: low / medium / high / extra
  high), and the **speed** (standard or fast), mirroring the official Codex app.
  Your choice is remembered per board.
- Image turns feel faster out of the box: Cameo now sends a balanced reasoning
  setting by default instead of inheriting whatever heavier setting your Codex
  CLI happened to be configured with.
- The **network proxy** in Settings now covers *everything* Cameo does — the
  Gallery (listings and images), usage stats, and update checks — not just the
  Codex agent. Set it once and all of Cameo's traffic follows it.
- You can **rename a workspace** from its menu.

### Fixes

- Your chat history is now saved reliably. Replies that finished while you were
  on another board (or during a flaky connection) could previously go missing
  from a board's history; the app now records each turn itself as it happens.

---

## [0.1.3] - 2026-05-29

### Improvements

- Canvas navigation feels steadier on desktop and trackpads: Space-drag panning
  works consistently, zoom gestures are smoother, and the HUD now has quick
  zoom-out, reset-to-100%, and zoom-in controls.
- You can keep steering Codex while a turn is running by typing a follow-up and
  pressing Enter; the visible Stop button now only stops the active conversation.
- Codex connection recovery is clearer, with better reconnect behavior, proxy
  diagnostics, and an in-chat warning when Cameo detects that Codex cannot reach
  the network.
- Image previews and board image operations are more reliable on Windows,
  including fallback loading for thumbnails, marked images, and crops.

---

## [0.1.2] - 2026-05-28

### Improvements

- You can now export several selected images at once into a folder, with Cameo
  preserving the original filenames and avoiding collisions automatically.
- Image loading is more reliable across macOS and Windows, especially for board
  images served through Cameo's local image protocol.
- App updates and website download links now use Cameo's dedicated release CDN,
  while GitHub Releases remain available as a manual installer mirror.

---

## [0.1.1] - 2026-05-27

### Improvements

- The AI chat now renders Markdown more naturally, including readable headings,
  lists, inline code, and lightweight code blocks with copy actions.
- Image paths in AI replies can turn into image previews, and those previews can
  be added to the canvas, used as references, copied, revealed in Finder, or
  focused on the canvas when already present.
- The chat panel is easier to read with larger message and input text, plus a
  resizable width for longer replies.
- Canvas selection, snapping, resizing, and drag thresholds feel steadier when
  arranging images.
- Codex session handling is more resilient, including better active-turn
  steering, runtime discovery, and Windows image handling.

---

## [0.1.0] - 2026-05-25

### First Public Release

Cameo is now available as an image-first desktop canvas for your local Codex
agent. It gives Codex a spatial workspace for image generation and editing:
open a folder, point at the images you mean, mark the region you want to change,
and let the results fan out across the board instead of disappearing into a chat
thread.

### What You Can Do

- Open any local folder as a board, with images laid out on an infinite canvas.
- Select one or more images, draw region marks, add notes, and send that visual
  context to Codex.
- Keep a continuous Codex conversation per board, so follow-up instructions like
  "warmer", "try three versions", or "now change the background" keep their
  context.
- Preserve originals automatically: every output becomes a new image placed next
  to its source, with visible lineage on the canvas.
- Pan, zoom, drag, resize, rotate, marquee-select, fit the board, and zoom to
  the current selection.
- Compare edits with before/after and side-by-side views.
- Crop, copy, export, reveal files in the system file manager, and keep working
  with normal local image files.
- Use built-in presets for common image operations alongside free-form prompts.
- Browse the prompt gallery from the app or at cameo.ink/gallery for examples
  and starting points.

### Desktop App

- Native app for macOS and Windows, built with a GPU-backed canvas for large
  image boards.
- Uses your own logged-in Codex CLI and ChatGPT subscription. Cameo does not
  bundle Codex, sell tokens, or require an API key.
- Includes workspace restore, multiple sessions per board, timeline persistence,
  streaming Codex responses, clarifying questions, settings, proxy support,
  unified logs, tray behavior, and app update support.
- Ships with the completed v1 visual system: light, focused, image-first UI with
  English and Chinese localization.

### Notes

This is the first public open-source release of Cameo under AGPL-3.0. The core
loop is in place: open a folder, work on images spatially, talk to Codex with
visual references, and keep every result as a non-destructive local artifact.
