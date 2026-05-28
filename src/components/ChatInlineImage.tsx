import { useEffect, useRef, useState } from "react";
import { Copy, FileText, FolderOpen, Image, ImagePlus, Maximize } from "lucide-react";
import { useChatStore } from "../store/chat";
import { useBoardStore } from "../store/board";
import { useComposerStore } from "../store/composer";
import { cameoUrl, ipc } from "../lib/ipc";
import { useAssetObjectUrl } from "../lib/asset-url";
import { useT } from "../i18n/locale";

/**
 * Renders an image path the AI emitted in chat text. Resolution states:
 *
 *   • not yet resolved / pending → small placeholder card with the basename
 *   • missing (file gone / not an image) → strikethrough basename, no image
 *   • in-workspace → thumbnail via the Cameo image protocol
 *   • out-of-workspace → base64 thumb data URL from `resolveChatImage`
 *
 * Right-click → context menu that bridges the chat artifact back into the
 * canvas/reference loop.
 */
export function ChatInlineImage({ path }: { path: string }) {
  const t = useT();
  const boardId = useBoardStore((s) => s.boardId);
  const placements = useBoardStore((s) => s.placements);
  const assets = useBoardStore((s) => s.assets);
  const resolution = useChatStore((s) => s.imageResolutions.get(path));
  const resolveChatImage = useChatStore((s) => s.resolveChatImage);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const missingRetries = useRef(0);
  // After "添加到画布" succeeds we want this card to behave like the canvas
  // variant — store the new placement id locally (chat store's resolution
  // map only tracks Rust's read-only classification).
  const [addedPlacementId, setAddedPlacementId] = useState<string | null>(null);

  useEffect(() => {
    missingRetries.current = 0;
  }, [path]);

  // Kick the resolver on mount and retry a few times after "missing" because
  // streamed model text can mention a file before the producing tool has
  // flushed it to disk. `resolveChatImage` still short-circuits pending/resolved.
  useEffect(() => {
    if (resolution === undefined) {
      resolveChatImage(path);
      return;
    }
    if (resolution !== "missing" || missingRetries.current >= 4) return;
    const timer = window.setTimeout(() => {
      missingRetries.current += 1;
      resolveChatImage(path);
    }, 1_200);
    return () => window.clearTimeout(timer);
  }, [path, resolution, resolveChatImage]);

  const onContext = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setMenu({ x: e.clientX, y: e.clientY });
  };

  const basename = path.split(/[/\\]/).pop() ?? path;
  const isPending = resolution === undefined || resolution === "pending";
  const isMissing =
    resolution === "missing" || (typeof resolution === "object" && !resolution.exists);
  const res = typeof resolution === "object" && resolution.exists ? resolution : null;
  const existingPlacementId = addedPlacementId ?? res?.existingPlacementId ?? null;
  const existingPlacement = existingPlacementId ? placements.get(existingPlacementId) : null;
  const existingAsset = existingPlacement ? assets.get(existingPlacement.assetId) ?? null : null;
  const workspaceAssetThumb = useAssetObjectUrl(
    res?.inWorkspace && existingAsset ? boardId : null,
    existingAsset?.path ?? null,
    existingAsset?.mime ?? "image/png",
  );

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

  if (!res) return null;
  const workspaceProtocolThumb =
    res.inWorkspace && boardId && res.workspaceRelPath ? cameoUrl(boardId, res.workspaceRelPath) : "";
  const thumbSrc = res.inWorkspace ? workspaceAssetThumb ?? workspaceProtocolThumb : res.thumbDataUrl ?? "";

  // Resolved → render as an actual visible thumbnail (not a chip). Right-click
  // surfaces canvas/reference and file actions.
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
          existingPlacementId={existingPlacementId}
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
  existingPlacementId: string | null;
  onAddedPlacement: (id: string) => void;
}

function ChatImageMenu({ x, y, onClose, absPath, existingPlacementId, onAddedPlacement }: MenuProps) {
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

  const doCopyPath = () => {
    void navigator.clipboard.writeText(absPath).catch(() => { /* silent */ });
    onClose();
  };

  const doShowOnCanvas = () => {
    if (existingPlacementId) {
      useBoardStore.getState().revealPlacement(existingPlacementId);
    }
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
    //   1. If resolver found an existing placement, or this menu imported
    //      one earlier, use that id.
    //   2. Else → import (chat-import command works in both cases since
    //      content-addressed dedup means in-workspace files don't double).
    let placementId = existingPlacementId;
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

  return (
    <div
      ref={ref}
      className="cm-ctx cm-ctx--chatimg"
      style={{ left: x, top: y }}
      role="menu"
    >
      {existingPlacementId ? (
        <button className="cm-ctx__item" onClick={doShowOnCanvas} role="menuitem">
          <Maximize size={14} />
          {t("chatImg.showOnCanvas")}
        </button>
      ) : (
        <button className="cm-ctx__item" onClick={() => void doAddToCanvas()} role="menuitem">
          <ImagePlus size={14} />
          {t("chatImg.addToCanvas")}
        </button>
      )}
      <button className="cm-ctx__item" onClick={() => void doUseAsRef()} role="menuitem">
        <Image size={14} />
        {t("chatImg.useAsRef")}
      </button>
      <div className="cm-ctx__sep" />
      <button className="cm-ctx__item" onClick={doCopy} role="menuitem">
        <Copy size={14} />
        {t("chatImg.copy")}
      </button>
      <button className="cm-ctx__item" onClick={doCopyPath} role="menuitem">
        <FileText size={14} />
        {t("chatImg.copyPath")}
      </button>
      <button className="cm-ctx__item" onClick={doReveal} role="menuitem">
        <FolderOpen size={14} />
        {t("chatImg.reveal")}
      </button>
    </div>
  );
}
