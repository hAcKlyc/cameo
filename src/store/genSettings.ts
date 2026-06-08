/**
 * Per-Board generation knobs the composer menu drives: model, reasoning effort
 * ("智能"), and service tier ("速度"). Mirrors the official Codex app's input-box
 * picker.
 *
 * Source of truth is Rust (meta.json + the live session); this store is a thin
 * cache so the composer renders instantly. Every mutation persists via
 * `ipc.setGenSettings`, which also pushes onto the live session so the next
 * `turn/start` uses it (see CODEX_PROTOCOL.md §4 — overrides are sticky, so the
 * standard tier is sent as explicit `null`).
 *
 * model/effort are always resolved to a concrete value in this store (product
 * default applied); serviceTier is `null` for the standard tier.
 */

import { create } from "zustand";
import { ipc } from "../lib/ipc";
import { useSettingsStore } from "./settings";
import type { ModelInfo, ServiceTierInfo } from "../types";

/** Reasoning-effort levels exposed in the menu, in display order. Codex also
 *  has `none`/`minimal`, which the official app doesn't surface — we match it. */
export const EXPOSED_EFFORTS = ["low", "medium", "high", "xhigh"] as const;
export type Effort = (typeof EXPOSED_EFFORTS)[number];

export const DEFAULT_MODEL = "gpt-5.5";
export const DEFAULT_EFFORT: Effort = "medium";

const MODEL_LIST_CACHE_TTL_MS = 2 * 60 * 1000;

const modelListCache = new Map<string, { loadedAt: number; models: ModelInfo[] }>();
const modelListInflight = new Map<string, Promise<ModelInfo[]>>();

interface GenState {
  /** Board these values belong to (guards async races across board switches). */
  boardId: string | null;
  model: string;
  effort: string;
  /** `null` = standard tier; otherwise a service-tier id (e.g. "priority"). */
  serviceTier: string | null;
  models: ModelInfo[];

  /** Load persisted knobs for a Board (no live session required). */
  load: (boardId: string) => Promise<void>;
  /** Fetch the model list (needs a ready session); reconcile a vanished model. */
  fetchModels: (boardId: string) => Promise<void>;
  setModel: (id: string) => void;
  setEffort: (effort: string) => void;
  setServiceTier: (tier: string | null) => void;
}

function persist(s: Pick<GenState, "boardId" | "model" | "effort" | "serviceTier">) {
  if (!s.boardId) return;
  void ipc
    .setGenSettings(s.boardId, { model: s.model, effort: s.effort, serviceTier: s.serviceTier })
    .catch((e) => {
      // Log so a persist failure (UI shows the new value, backend never got it)
      // leaves a breadcrumb instead of silently diverging until restart.
      void ipc.frontLog("warn", `persist gen settings failed: ${e}`).catch(() => {});
    });
}

function modelListCacheKey(boardId: string): string {
  return `${boardId}\0${useSettingsStore.getState().restartNonce}`;
}

function freshModelListCache(boardId: string): ModelInfo[] | null {
  const cached = modelListCache.get(modelListCacheKey(boardId));
  if (!cached || Date.now() - cached.loadedAt > MODEL_LIST_CACHE_TTL_MS) return null;
  return cached.models;
}

async function listModelsCached(boardId: string): Promise<ModelInfo[]> {
  const cached = freshModelListCache(boardId);
  if (cached) return cached;
  const key = modelListCacheKey(boardId);
  const inflight = modelListInflight.get(key);
  if (inflight) return inflight;

  const promise = ipc.listModels(boardId).then((models) => {
    modelListCache.set(key, { loadedAt: Date.now(), models });
    return models;
  });
  modelListInflight.set(key, promise);

  try {
    return await promise;
  } finally {
    modelListInflight.delete(key);
  }
}

/** Efforts to show for a model: its supported set ∩ the exposed levels, in
 *  display order. Empty `supported` (list not loaded yet) → all exposed. */
export function effortsFor(m: ModelInfo | undefined): Effort[] {
  if (!m || m.supportedEfforts.length === 0) return [...EXPOSED_EFFORTS];
  return EXPOSED_EFFORTS.filter((e) => m.supportedEfforts.includes(e));
}

/** Fast (non-standard) tiers a model offers. Standard is always implicit. */
export function tiersFor(m: ModelInfo | undefined): ServiceTierInfo[] {
  return m?.serviceTiers ?? [];
}

export const useGenStore = create<GenState>((set, get) => ({
  boardId: null,
  model: DEFAULT_MODEL,
  effort: DEFAULT_EFFORT,
  serviceTier: null,
  models: [],

  load: async (boardId) => {
    set({ boardId, models: freshModelListCache(boardId) ?? [] });
    try {
      const gs = await ipc.getGenSettings(boardId);
      if (get().boardId !== boardId) return; // board switched mid-flight
      set({
        model: gs.model ?? DEFAULT_MODEL,
        effort: gs.effort ?? DEFAULT_EFFORT,
        serviceTier: gs.serviceTier ?? null,
      });
    } catch {
      if (get().boardId !== boardId) return;
      set({ model: DEFAULT_MODEL, effort: DEFAULT_EFFORT, serviceTier: null });
    }
  },

  fetchModels: async (boardId) => {
    try {
      const models = await listModelsCached(boardId);
      if (get().boardId !== boardId) return;
      set({ models });
      // Vanished-model fallback: persisted model no longer offered → reset to a
      // sensible default before any dispatch can send an invalid id.
      const cur = get().model;
      if (models.length > 0 && !models.some((m) => m.id === cur)) {
        const fallback = models.find((m) => m.id === DEFAULT_MODEL) ?? models[0];
        get().setModel(fallback.id);
      }
    } catch {
      /* session not ready / list failed — keep the persisted selection visible */
    }
  },

  setModel: (id) => {
    const { models, effort, serviceTier } = get();
    const m = models.find((x) => x.id === id);
    // Clamp effort/tier to what the new model supports. `efforts` is already the
    // supported ∩ exposed set, so its first item is always a safe fallback —
    // never hardcode "medium", which a model might not support.
    const efforts = effortsFor(m);
    const def = m?.defaultReasoningEffort;
    const nextEffort = efforts.includes(effort as Effort)
      ? effort
      : def && efforts.includes(def as Effort)
        ? def
        : efforts[0] ?? DEFAULT_EFFORT;
    const tierOk = serviceTier === null || tiersFor(m).some((t) => t.id === serviceTier);
    const nextTier = tierOk ? serviceTier : null;
    set({ model: id, effort: nextEffort, serviceTier: nextTier });
    persist(get());
  },

  setEffort: (effort) => {
    set({ effort });
    persist(get());
  },

  setServiceTier: (tier) => {
    set({ serviceTier: tier });
    persist(get());
  },
}));
