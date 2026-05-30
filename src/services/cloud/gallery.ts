/**
 * Gallery API client — thin wrappers around the Cameo cloud API's
 * /api/v1/gallery/* + the shapes the server returns.
 *
 * List items carry the FULL prompt + every field needed by the detail view —
 * there is no separate /items/:id round-trip on the click path. The endpoint
 * still exists server-side (SEO / direct-link) but the client never calls it.
 */

import { cloudFetch } from "./index";

export interface GalleryItem {
  id: string;
  t_en: string;
  t_zh: string;
  uc: string;
  tags: string[];
  lang: string;
  inp: boolean; // requires_input_image
  repo: string;
  author: string | null;
  url: string | null;
  platform: string | null;
  prompt: string;
  img: string;
  img_full: string;
  w: number | null;
  h: number | null;
}

export interface Facet { k: string; n: number }

export interface ItemsResponse {
  offset: number;
  limit: number;
  items: GalleryItem[];
  /** Server-chosen seeded-shuffle bucket. Pin this in client state and pass
   *  back as `_b` on subsequent pages so the order stays consistent across
   *  the visit. */
  bucket: number;
}

export interface ItemsParams {
  limit?: number;
  offset?: number;
  q?: string;
  use_case?: string;
  lang?: string;
  requires_input_image?: boolean;
  /** Bucket id echoed from a previous /items response. */
  _b?: number;
}

interface FetchOpts {
  signal?: AbortSignal;
}

export async function fetchItems(params: ItemsParams = {}, opts?: FetchOpts): Promise<ItemsResponse> {
  const qs = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === "" || v === null) continue;
    qs.set(k, String(v));
  }
  return cloudFetch<ItemsResponse>(`/api/v1/gallery/items?${qs.toString()}`, { signal: opts?.signal });
}

export async function fetchFacets(opts?: FetchOpts): Promise<{ use_case: Facet[]; lang: Facet[] }> {
  return cloudFetch<{ use_case: Facet[]; lang: Facet[] }>("/api/v1/gallery/facets", { signal: opts?.signal });
}

export async function fetchRandom(opts?: { use_case?: string; requires_input_image?: boolean; signal?: AbortSignal }): Promise<GalleryItem> {
  const qs = new URLSearchParams();
  if (opts?.use_case) qs.set("use_case", opts.use_case);
  if (opts?.requires_input_image) qs.set("requires_input_image", "true");
  const path = qs.toString() ? `/api/v1/gallery/random?${qs}` : "/api/v1/gallery/random";
  return cloudFetch<GalleryItem>(path, { signal: opts?.signal });
}
