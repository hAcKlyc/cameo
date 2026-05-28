import { useEffect, useState } from "react";
import { cameoUrl, ipc } from "./ipc";

function loadImageUrl(url: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve();
    img.onerror = () => reject(new Error(`load ${url}`));
    img.src = url;
  });
}

export function loadAssetObjectUrl(boardId: string, relPath: string, mime = "image/png"): Promise<string> {
  return ipc.readAssetBytes(boardId, relPath).then((bytes) => {
    const blob = new Blob([new Uint8Array(bytes)], { type: mime });
    return URL.createObjectURL(blob);
  });
}

export function loadAssetImage(boardId: string, relPath: string, mime = "image/png"): Promise<HTMLImageElement> {
  const protocolUrl = cameoUrl(boardId, relPath);
  return loadImageElement(protocolUrl).catch(async () => {
    const objectUrl = await loadAssetObjectUrl(boardId, relPath, mime);
    try {
      return await loadImageElement(objectUrl);
    } finally {
      URL.revokeObjectURL(objectUrl);
    }
  });
}

function loadImageElement(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error(`load ${url}`));
    img.src = url;
  });
}

export function useAssetObjectUrl(boardId: string | null, relPath: string | null, mime?: string | null): string | null {
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!boardId || !relPath) {
      setUrl(null);
      return;
    }

    let live = true;
    let createdObjectUrl: string | null = null;
    setUrl(null);
    const protocolUrl = cameoUrl(boardId, relPath);
    void (async () => {
      try {
        await loadImageUrl(protocolUrl);
        if (live) setUrl(protocolUrl);
        return;
      } catch {
        // WebView2 can fail to load the custom protocol for local images.
        // Fall back to a scoped IPC byte read, but own and revoke the Blob URL.
      }

      try {
        const next = await loadAssetObjectUrl(boardId, relPath, mime ?? "image/png");
        if (!live) {
          URL.revokeObjectURL(next);
          return;
        }
        createdObjectUrl = next;
        setUrl(next);
      } catch {
        if (live) setUrl(protocolUrl);
      }
    })();

    return () => {
      live = false;
      if (createdObjectUrl) URL.revokeObjectURL(createdObjectUrl);
    };
  }, [boardId, relPath, mime]);

  return url;
}
