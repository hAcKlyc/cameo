import { useEffect, useState } from "react";
import { cameoUrl, ipc } from "./ipc";

const FALLBACK_OBJECT_URL_CACHE_MAX = 48;
const FALLBACK_OBJECT_URL_CACHE_MAX_BYTES = 128 * 1024 * 1024;

interface CachedObjectUrl {
  url: string;
  refs: number;
  lastUsed: number;
  bytes: number;
}

interface InflightObjectUrl {
  promise: Promise<string>;
  refs: number;
}

const fallbackObjectUrlCache = new Map<string, CachedObjectUrl>();
const fallbackObjectUrlInflight = new Map<string, InflightObjectUrl>();
let fallbackObjectUrlCachedBytes = 0;

function loadImageUrl(url: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve();
    img.onerror = () => reject(new Error(`load ${url}`));
    img.src = url;
  });
}

function fallbackObjectUrlKey(boardId: string, relPath: string, mime: string): string {
  return `${boardId}\0${relPath}\0${mime}`;
}

function evictFallbackObjectUrls() {
  if (
    fallbackObjectUrlCache.size <= FALLBACK_OBJECT_URL_CACHE_MAX &&
    fallbackObjectUrlCachedBytes <= FALLBACK_OBJECT_URL_CACHE_MAX_BYTES
  ) {
    return;
  }
  const evictable = [...fallbackObjectUrlCache.entries()]
    .filter(([, entry]) => entry.refs === 0)
    .sort((a, b) => a[1].lastUsed - b[1].lastUsed);

  for (const [key, entry] of evictable) {
    if (
      fallbackObjectUrlCache.size <= FALLBACK_OBJECT_URL_CACHE_MAX &&
      fallbackObjectUrlCachedBytes <= FALLBACK_OBJECT_URL_CACHE_MAX_BYTES
    ) {
      break;
    }
    URL.revokeObjectURL(entry.url);
    fallbackObjectUrlCachedBytes -= entry.bytes;
    fallbackObjectUrlCache.delete(key);
  }
}

async function createAssetObjectUrl(boardId: string, relPath: string, mime: string): Promise<{ url: string; bytes: number }> {
  const bytes = await ipc.readAssetBytes(boardId, relPath);
  const blob = new Blob([new Uint8Array(bytes)], { type: mime });
  return { url: URL.createObjectURL(blob), bytes: bytes.length };
}

async function acquireFallbackObjectUrl(boardId: string, relPath: string, mime = "image/png"): Promise<string> {
  const key = fallbackObjectUrlKey(boardId, relPath, mime);
  const cached = fallbackObjectUrlCache.get(key);
  if (cached) {
    cached.refs += 1;
    cached.lastUsed = Date.now();
    return cached.url;
  }

  const inflight = fallbackObjectUrlInflight.get(key);
  if (inflight) {
    inflight.refs += 1;
    return inflight.promise;
  }

  const nextInflight: InflightObjectUrl = {
    refs: 1,
    promise: Promise.resolve(""),
  };
  nextInflight.promise = createAssetObjectUrl(boardId, relPath, mime).then(({ url, bytes }) => {
    fallbackObjectUrlCache.set(key, { url, refs: nextInflight.refs, lastUsed: Date.now(), bytes });
    fallbackObjectUrlCachedBytes += bytes;
    evictFallbackObjectUrls();
    return url;
  });
  fallbackObjectUrlInflight.set(key, nextInflight);

  try {
    return await nextInflight.promise;
  } catch (error) {
    fallbackObjectUrlCache.delete(key);
    throw error;
  } finally {
    fallbackObjectUrlInflight.delete(key);
  }
}

function releaseFallbackObjectUrl(boardId: string, relPath: string, mime = "image/png") {
  const entry = fallbackObjectUrlCache.get(fallbackObjectUrlKey(boardId, relPath, mime));
  if (!entry) return;
  entry.refs = Math.max(0, entry.refs - 1);
  entry.lastUsed = Date.now();
  evictFallbackObjectUrls();
}

export function loadAssetObjectUrl(boardId: string, relPath: string, mime = "image/png"): Promise<string> {
  return createAssetObjectUrl(boardId, relPath, mime).then(({ url }) => url);
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
    let acquiredObjectUrl: { boardId: string; relPath: string; mime: string } | null = null;
    setUrl(null);
    const protocolUrl = cameoUrl(boardId, relPath);
    const fallbackMime = mime ?? "image/png";
    void (async () => {
      try {
        await loadImageUrl(protocolUrl);
        if (live) setUrl(protocolUrl);
        return;
      } catch {
        // WebView2 can fail to load the custom protocol for local images.
        // Fall back to a scoped IPC byte read backed by a shared Blob URL cache.
      }

      try {
        const next = await acquireFallbackObjectUrl(boardId, relPath, fallbackMime);
        if (!live) {
          releaseFallbackObjectUrl(boardId, relPath, fallbackMime);
          return;
        }
        acquiredObjectUrl = { boardId, relPath, mime: fallbackMime };
        setUrl(next);
      } catch {
        if (live) setUrl(protocolUrl);
      }
    })();

    return () => {
      live = false;
      if (acquiredObjectUrl) {
        releaseFallbackObjectUrl(acquiredObjectUrl.boardId, acquiredObjectUrl.relPath, acquiredObjectUrl.mime);
      }
    };
  }, [boardId, relPath, mime]);

  return url;
}
