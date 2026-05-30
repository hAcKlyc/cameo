import { create } from "zustand";
import { ipc } from "../lib/ipc";
import { useBoardStore } from "./board";
import type { ApiImageSettings, AppConfig, ProxySettings, RuntimeProvider } from "../types";

const DEFAULT_PROXY: ProxySettings = {
  enabled: false,
  protocol: "http",
  host: "127.0.0.1",
  port: 7897,
};

const DEFAULT_API: ApiImageSettings = {
  base_url: "https://api.openai.com/v1",
  api_key: "",
  model: "gpt-image-1",
  size: "1024x1024",
};

function defaultConfig(): AppConfig {
  return {
    provider: "codex",
    api: { ...DEFAULT_API },
    proxy: { ...DEFAULT_PROXY },
    telemetry_opt_out: false,
    last_telemetry_date: null,
    close_to_tray: true,
  };
}

const sameProxy = (a: ProxySettings, b: ProxySettings) =>
  a.enabled === b.enabled && a.protocol === b.protocol && a.host === b.host && a.port === b.port;

const sameRuntime = (a: Pick<AppConfig, "provider" | "api">, b: Pick<AppConfig, "provider" | "api">) =>
  a.provider === b.provider &&
  a.api.base_url === b.api.base_url &&
  a.api.api_key === b.api.api_key &&
  a.api.model === b.api.model &&
  a.api.size === b.api.size;

const proxyProtocol = (value: unknown): ProxySettings["protocol"] =>
  value === "socks5" ? "socks5" : "http";

const provider = (value: unknown): RuntimeProvider => (value === "api" ? "api" : "codex");

const runtimeSnapshot = (config: AppConfig): Pick<AppConfig, "provider" | "api"> => ({
  provider: config.provider,
  api: { ...config.api },
});

interface SettingsState {
  config: AppConfig;
  loaded: boolean;
  /** Last proxy state persisted + applied to the sidecar — used to skip no-op restarts. */
  applied: ProxySettings;
  /** Last runtime settings persisted + applied to the active session. */
  appliedRuntime: Pick<AppConfig, "provider" | "api">;
  /** Transient: a commit is persisting + restarting the session (inline feedback). */
  applying: boolean;
  /** Bumped after a commit so the active Codex session restarts (the proxy is
   *  injected at sidecar spawn — App.tsx watches this nonce). */
  restartNonce: number;

  load: () => Promise<void>;
  /** Switch between local Codex and direct API runtime. Persists + restarts. */
  setProvider: (provider: RuntimeProvider) => Promise<void>;
  /** Edit API controls in-memory. Call commitRuntime on blur/change commit. */
  setApi: (patch: Partial<ApiImageSettings>) => void;
  /** Persist runtime settings and restart the active session when needed. */
  commitRuntime: () => Promise<void>;
  /** Edit the in-memory proxy (controlled inputs). Does not persist or restart. */
  setProxy: (patch: Partial<ProxySettings>) => void;
  /** Persist the current proxy and restart the active session so it takes effect.
   *  No-ops when nothing changed since the last apply. There is no Save button —
   *  call this on commit (toggle/select change, input blur); settings apply live. */
  commitProxy: () => Promise<void>;
  /** Toggle telemetry opt-out and persist immediately (no restart needed —
   *  the next bootDailyPing reads the fresh value). */
  setTelemetryOptOut: (value: boolean) => Promise<void>;
  /** Toggle close-to-tray and persist immediately. The Rust window-close
   *  handler reads config from disk each close, so no restart is needed. */
  setCloseToTray: (value: boolean) => Promise<void>;
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  config: defaultConfig(),
  loaded: false,
  applied: { ...DEFAULT_PROXY },
  appliedRuntime: runtimeSnapshot(defaultConfig()),
  applying: false,
  restartNonce: 0,

  load: async () => {
    try {
      const cfg = await ipc.cfgLoad();
      const rawProxy = cfg?.proxy;
      const rawApi = cfg?.api;
      const merged: AppConfig = {
        provider: provider(cfg?.provider),
        api: { ...DEFAULT_API, ...rawApi },
        proxy: { ...DEFAULT_PROXY, ...rawProxy, protocol: proxyProtocol(rawProxy?.protocol) },
        telemetry_opt_out: !!cfg?.telemetry_opt_out,
        last_telemetry_date: cfg?.last_telemetry_date ?? null,
        close_to_tray: cfg?.close_to_tray ?? true,
      };
      set({
        config: merged,
        applied: { ...merged.proxy },
        appliedRuntime: runtimeSnapshot(merged),
        loaded: true,
      });
    } catch {
      set({ loaded: true });
    }
  },

  setProvider: async (nextProvider) => {
    set((s) => ({ config: { ...s.config, provider: nextProvider } }));
    await get().commitRuntime();
  },

  setApi: (patch) =>
    set((s) => ({ config: { ...s.config, api: { ...s.config.api, ...patch } } })),

  commitRuntime: async () => {
    const { config, appliedRuntime } = get();
    const nextRuntime = runtimeSnapshot(config);
    if (sameRuntime(nextRuntime, appliedRuntime)) return;
    set({ applying: true });
    try {
      await ipc.cfgSave(config);
      const boardId = useBoardStore.getState().boardId;
      if (boardId) await ipc.stopSession(boardId);
      set((s) => ({
        appliedRuntime: runtimeSnapshot(config),
        restartNonce: s.restartNonce + 1,
      }));
    } finally {
      set({ applying: false });
    }
  },

  setProxy: (patch) =>
    set((s) => ({ config: { ...s.config, proxy: { ...s.config.proxy, ...patch } } })),

  commitProxy: async () => {
    const { config, applied } = get();
    if (sameProxy(config.proxy, applied)) return; // nothing changed — no restart
    set({ applying: true });
    try {
      await ipc.cfgSave(config);
      // Apply to the running Codex sidecar. Tear the active session down FIRST
      // (stop_session removes it from the registry synchronously, then kills the
      // process) so the nonce-driven restart in App.tsx spawns a fresh sidecar —
      // start_session would otherwise early-return the still-registered session.
      const boardId = useBoardStore.getState().boardId;
      if (boardId) await ipc.stopSession(boardId);
      set((s) => ({ applied: { ...config.proxy }, restartNonce: s.restartNonce + 1 }));
    } finally {
      set({ applying: false });
    }
  },

  setTelemetryOptOut: async (value) => {
    const { config } = get();
    const next: AppConfig = { ...config, telemetry_opt_out: value };
    set({ config: next });
    try {
      await ipc.cfgSave(next);
    } catch {
      // Persist failure → roll back so UI reflects truth.
      set({ config });
      throw new Error("failed to save settings");
    }
  },

  setCloseToTray: async (value) => {
    const { config } = get();
    const next: AppConfig = { ...config, close_to_tray: value };
    set({ config: next });
    try {
      await ipc.cfgSave(next);
    } catch {
      set({ config }); // roll back so UI reflects truth
      throw new Error("failed to save settings");
    }
  },
}));
