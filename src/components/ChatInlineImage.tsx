import { useEffect, useRef, useState } from "react";
import { useChatStore } from "../store/chat";
import { useBoardStore } from "../store/board";
import { useComposerStore } from "../store/composer";
import { cameoUrl, ipc } from "../lib/ipc";
import { useT } from "../i18n/locale";

/**
 * Renders an image path the AI emitted in chat text. Resolution states:
 *
 *   • not yet resolved / pending → small placeholder card with the basename
 *   • missing (file gone / not an image) → strikethrough basename, no image
 *   • in-workspace → thumbnail via `cameo://`
 *   • out-of-workspace → base64 thumb data URL from `resolveChatImage`
 *
 * Right-click → context menu with platform-appropriate actions (different
 * for in-workspace vs out-of-workspace — "Add to canvas" only makes sense
 * for outside-the-workspace images).
 */
export function ChatInlineImage({ path }: { path: string }) {
  const t = useT();
  const boardId = useBoardStore((s) => s.boardId);
  const resolution = useChatStore((s) => s.imageResolutions.get(path));
  const resolveChatImage = useChatStore((s) => s.resolveChatImage);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  // After "添加到画布" succeeds we want this card to behave like the canvas
  // variant — store the new placement id locally (chat store's resolution
  // map only tracks Rust's read-only classification).
  const [addedPlacementId, setAddedPlacementId] = useState<string | null>(null);

  // Kick the resolver on mount and any time the path changes (we never
  // resolve twice — `resolveChatImage` short-circuits on cached entries).
  useEffect(() => {
    resolveChatImage(path);
  }, [path, resolveChatImage]);

  const onContext = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setMenu({ x: e.clientX, y: e.clientY });
  };

  const basename = path.split(/[/\\]/).pop() ?? path;
  const isPending = resolution === undefined || resolution === "pending";
  const isMissing =
    resolution === "missing" || (typeof resolution === "object" && !resolution.exists);

  if (isPending) {
    return (
      <span className="cm-chatimg cm-chatimg--pending" title={path}>
        <span className="cm-chatimg__spin" />
        <span className="cm-chatimg__name">{basename}</span>
      </span>
    );
  }
  if (isMissing) {
    return (
      <span className="cm-chatimg cm-chatimg--missing" title={path}>
        <span className="cm-chatimg__name">{basename}</span>
        <span className="cm-chatimg__hint">· {t("chatImg.missing")}</span>
      </span>
    );
  }

  // Resolution is a real ChatImageResolution at this point.
  const res = resolution as Exclude<typeof resolution, "pending" | "missing" | undefined>;
  const thumbSrc = res.inWorkspace && res.workspaceRelPath && boardId
    ? cameoUrl(boardId, res.workspaceRelPath)
    : res.thumbDataUrl ?? "";

  // Resolved → render as an actual visible thumbnail (not a chip). Right-click
  // still surfaces the context menu (copy / use as ref / add to canvas / reveal).
  return (
    <span className="cm-chatimg-wrap">
      <span
        className="cm-chatimg-thumb"
        onContextMenu={onContext}
        title={basename}
      >
        {thumbSrc ? (
          <img src={thumbSrc} alt={basename} />
        ) : (
          <span className="cm-chatimg-thumb__blank" />
        )}
      </span>
      {menu && (
        <ChatImageMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          absPath={res.absPath}
          inWorkspace={res.inWorkspace}
          addedPlacementId={addedPlacementId}
          onAddedPlacement={setAddedPlacementId}
        />
      )}
    </span>
  );
}

interface MenuProps {
  x: number;
  y: number;
  onClose: () => void;
  absPath: string;
  inWorkspace: boolean;
  addedPlacementId: string | null;
  onAddedPlacement: (id: string) => void;
}

function ChatImageMenu({ x, y, onClose, absPath, inWorkspace, addedPlacementId, onAddedPlacement }: MenuProps) {
  const t = useT();
  const ref = useRef<HTMLDivElement>(null);

  // Outside-click + Esc close.
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const doCopy = () => {
    void ipc.copyImageFromPath(absPath).catch(() => { /* silent */ });
    onClose();
  };

  const doReveal = () => {
    void ipc.revealPathInFinder(absPath).catch(() => { /* silent */ });
    onClose();
  };

  const doAddToCanvas = async () => {
    const boardId = useBoardStore.getState().boardId;
    if (!boardId) {
      onClose();
      return;
    }
    try {
      const result = await ipc.importChatImageToCanvas(boardId, absPath);
      // Update the canvas store so the new placement renders. Mirrors the
      // pattern in board.ts addPlacements / addAssets after import_paths.
      // `assets` is intentionally empty when content-addressed import dedupes;
      // a new placement is still the thing the canvas must merge.
      const placement = result.placements[0];
      if (placement) {
        useBoardStore.getState().applyImportResult(result);
        onAddedPlacement(placement.id);
      }
    } catch {
      /* silent */
    }
    onClose();
  };

  const doUseAsRef = async () => {
    // Reference = a composer pill. We need an actual placementId. For
    // in-workspace images that aren't yet on the canvas we still need to
    // import (a pill references a Placement). Path:
    //   1. If already added via "Add to canvas" earlier → use that id.
    //   2. Else → import (chat-import command works in both cases since
    //      content-addressed dedup means in-workspace files don't double).
    let placementId = addedPlacementId;
    if (!placementId) {
      const boardId = useBoardStore.getState().boardId;
      if (!boardId) {
        onClose();
        return;
      }
      try {
        const result = await ipc.importChatImageToCanvas(boardId, absPath);
        useBoardStore.getState().applyImportResult(result);
        placementId = result.placements[0]?.id ?? null;
        if (placementId) onAddedPlacement(placementId);
      } catch {
        onClose();
        return;
      }
    }
    if (placementId) {
      useComposerStore.getState().injectPill(placementId);
    }
    onClose();
  };

  // Add-to-canvas item only meaningful for outside-workspace images that
  // haven't already been imported this session.
  const showAddToCanvas = !inWorkspace && !addedPlacementId;

  return (
    <div
      ref={ref}
      className="cm-ctx cm-ctx--chatimg"
      style={{ left: x, top: y }}
      role="menu"
    >
      <button className="cm-ctx__item" onClick={doCopy} role="menuitem">
        {t("chatImg.copy")}
      </button>
      <button className="cm-ctx__item" onClick={() => void doUseAsRef()} role="menuitem">
        {t("chatImg.useAsRef")}
      </button>
      {showAddToCanvas && (
        <button className="cm-ctx__item" onClick={() => void doAddToCanvas()} role="menuitem">
          {t("chatImg.addToCanvas")}
        </button>
      )}
      <button className="cm-ctx__item" onClick={doReveal} role="menuitem">
        {t("chatImg.reveal")}
      </button>
    </div>
  );
}
