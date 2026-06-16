import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AlertCircle, CheckCircle2, Loader2, X } from "lucide-react";
import { useSettingsStore } from "../store/settings";
import { useBoardStore } from "../store/board";
import { VersionSection } from "./VersionSection";
import { useT, useLocaleStore, type LocaleChoice } from "../i18n/locale";
import { ipc } from "../lib/ipc";
import type { MsgKey } from "../i18n/messages";
import type { CloudAgentStatus, FfmpegStatus, ProxyProbeResult, ProxySettings } from "../types";

const PROTOCOLS: ProxySettings["protocol"][] = ["http", "socks5"];

const isValidHost = (h: string) => {
  const v = h.trim();
  return !!v && !v.includes("://") && !v.includes("@") && !v.includes("/") && v.length <= 253;
};
const isValidPort = (p: number) => p >= 1 && p <= 65535;

/** "ffmpeg version 8.0.1 Copyright (c) 2000-2025 …" → "ffmpeg 8.0.1". The full
 *  banner is long and noisy in the panel; we keep it in the row's title tooltip
 *  and show just the number. Falls back to the raw string if it doesn't match. */
const shortFfmpegVersion = (full: string): string => {
  const m = full.match(/version\s+(\S+)/i);
  return m ? `ffmpeg ${m[1]}` : full;
};

type ProxyProbeState =
  | { status: "idle" }
  | { status: "checking" }
  | { status: "ok"; result: ProxyProbeResult }
  | { status: "error"; result: ProxyProbeResult | null; detail: string | null };

function proxyProbeMessageKey(kind: string): MsgKey {
  switch (kind) {
    case "invalid_proxy":
      return "settings.proxyProbe.invalid";
    case "proxy_unreachable":
      return "settings.proxyProbe.unreachable";
    case "timeout":
      return "settings.proxyProbe.timeout";
    case "protocol_mismatch":
      return "settings.proxyProbe.protocolMismatch";
    case "proxy_auth_required":
      return "settings.proxyProbe.authRequired";
    case "upstream_unreachable":
    case "internet_unreachable":
    case "network_error":
    case "unexpected_status":
    case "captive_portal":
      return "settings.proxyProbe.upstreamBlocked";
    default:
      return "settings.proxyProbe.error";
  }
}

/** App settings: UI language and the network proxy (injected into the Codex
 *  sidecar). Everything applies live — there is no Save button. Language is
 *  instant; proxy edits commit on change/blur and restart the session. */
export function SettingsModal({ onClose }: { onClose: () => void }) {
  const config = useSettingsStore((s) => s.config);
  const loaded = useSettingsStore((s) => s.loaded);
  const applying = useSettingsStore((s) => s.applying);
  const localeChoice = useLocaleStore((s) => s.choice);
  const proxy = config.proxy;
  const [proxyProbe, setProxyProbe] = useState<ProxyProbeState>({ status: "idle" });
  const proxyProbeGenerationRef = useRef(0);
  const t = useT();

  useEffect(() => {
    if (!loaded) void useSettingsStore.getState().load();
  }, [loaded]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const set = (patch: Partial<ProxySettings>) => useSettingsStore.getState().setProxy(patch);
  const hostOk = isValidHost(proxy.host);
  const portOk = isValidPort(proxy.port);

  useEffect(() => {
    if (!proxy.enabled || !hostOk || !portOk) {
      proxyProbeGenerationRef.current += 1;
      setProxyProbe({ status: "idle" });
      return;
    }

    const generation = proxyProbeGenerationRef.current + 1;
    proxyProbeGenerationRef.current = generation;
    setProxyProbe({ status: "checking" });

    const timer = window.setTimeout(() => {
      void ipc
        .probeProxy(proxy.protocol, proxy.host, proxy.port)
        .then((result) => {
          if (proxyProbeGenerationRef.current !== generation) return;
          if (result.ok) {
            setProxyProbe({ status: "ok", result });
          } else {
            setProxyProbe({ status: "error", result, detail: result.detail });
          }
        })
        .catch((error) => {
          if (proxyProbeGenerationRef.current !== generation) return;
          setProxyProbe({
            status: "error",
            result: null,
            detail: error instanceof Error ? error.message : String(error),
          });
        });
    }, 250);

    return () => {
      window.clearTimeout(timer);
      proxyProbeGenerationRef.current += 1;
    };
  }, [hostOk, portOk, proxy.enabled, proxy.host, proxy.port, proxy.protocol]);

  // Apply live: persist + restart the session, but only when the (now-current)
  // proxy is coherent so we never spawn the sidecar with a half-typed endpoint.
  const commit = () => {
    const p = useSettingsStore.getState().config.proxy;
    if (!p.enabled || (isValidHost(p.host) && isValidPort(p.port))) {
      void useSettingsStore.getState().commitProxy();
    }
  };
  const proxyProbeDetail =
    proxyProbe.status === "ok"
      ? proxyProbe.result.detail
      : proxyProbe.status === "error"
        ? proxyProbe.detail
        : undefined;

  // Live status line under the host:port row — invalid input, "applying", or
  // the connectivity probe. null when there is nothing to say (idle), so the
  // bordered status slot collapses instead of rendering an empty strip.
  const proxyStatus =
    !hostOk || !portOk ? (
      <p className="cm-set-hint cm-set-hint--err">{t("settings.invalid")}</p>
    ) : applying ? (
      <p className="cm-set-hint">{t("settings.proxyApplying")}</p>
    ) : proxyProbe.status !== "idle" ? (
      <div
        className={`cm-set-probe cm-set-probe--${proxyProbe.status}`}
        title={proxyProbeDetail ?? undefined}
      >
        {proxyProbe.status === "checking" ? (
          <Loader2 className="cm-set-probe__icon cm-set-probe__spin" size={14} />
        ) : proxyProbe.status === "ok" ? (
          <CheckCircle2 className="cm-set-probe__icon" size={14} />
        ) : (
          <AlertCircle className="cm-set-probe__icon" size={14} />
        )}
        <span className="cm-set-probe__text">
          {proxyProbe.status === "checking"
            ? t("settings.proxyProbe.checking")
            : proxyProbe.status === "ok"
              ? t("settings.proxyProbe.ok")
              : t(proxyProbeMessageKey(proxyProbe.result?.kind ?? "error"))}
        </span>
      </div>
    ) : null;

  return (
    <div className="cm-modal-backdrop" onClick={onClose}>
      <div className="cm-modal" onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true">
        <div className="cm-modal__head">
          <h2 className="cm-modal__title">{t("settings.title")}</h2>
          <button className="cm-modal__close" onClick={onClose}>
            <X size={16} />
          </button>
        </div>

        <div className="cm-modal__body">
          <section className="cm-set-section">
            <div className="cm-set-section__head">
              <div className="cm-set-section__text">
                <h3 className="cm-set-section__title">{t("settings.language")}</h3>
                <p className="cm-set-section__desc">{t("settings.languageDesc")}</p>
              </div>
              <div className="cm-set-section__control">
                <select
                  className="cm-select"
                  value={localeChoice}
                  aria-label={t("settings.language")}
                  onChange={(e) => useLocaleStore.getState().setChoice(e.target.value as LocaleChoice)}
                >
                  <option value="system">{t("settings.language.system")}</option>
                  <option value="zh">中文</option>
                  <option value="en">English</option>
                </select>
              </div>
            </div>
          </section>

          <section className="cm-set-section">
            <div className="cm-set-section__head">
              <div className="cm-set-section__text">
                <h3 className="cm-set-section__title">{t("settings.tray")}</h3>
                <p className="cm-set-section__desc">{t("settings.trayDesc")}</p>
              </div>
              <div className="cm-set-section__control">
                <button
                  type="button"
                  className={`cm-toggle${config.close_to_tray ? " is-on" : ""}`}
                  role="switch"
                  aria-checked={config.close_to_tray}
                  aria-label={t("settings.tray")}
                  onClick={() => void useSettingsStore.getState().setCloseToTray(!config.close_to_tray)}
                >
                  <span className="cm-toggle__knob" />
                </button>
              </div>
            </div>
          </section>

          <section className="cm-set-section">
            <div className="cm-set-section__head">
              <div className="cm-set-section__text">
                <h3 className="cm-set-section__title">{t("settings.proxy")}</h3>
                <p className="cm-set-section__desc">{t("settings.proxyDesc")}</p>
              </div>
              <div className="cm-set-section__control">
                <button
                  type="button"
                  className={`cm-toggle${proxy.enabled ? " is-on" : ""}`}
                  role="switch"
                  aria-checked={proxy.enabled}
                  aria-label={t("settings.enableProxy")}
                  onClick={() => {
                    set({ enabled: !proxy.enabled });
                    commit();
                  }}
                >
                  <span className="cm-toggle__knob" />
                </button>
              </div>
            </div>

            {proxy.enabled && (
              <div className="cm-set-proxy">
                {/* protocol · host : port — one line */}
                <div className="cm-set-proxy__row">
                  <select
                    className="cm-select cm-select--proto"
                    value={proxy.protocol}
                    aria-label={t("settings.protocol")}
                    onChange={(e) => {
                      set({ protocol: e.target.value as ProxySettings["protocol"] });
                      commit();
                    }}
                  >
                    {PROTOCOLS.map((p) => (
                      <option key={p} value={p}>
                        {p}
                      </option>
                    ))}
                  </select>
                  <input
                    className={`cm-input cm-input--host${hostOk ? "" : " is-invalid"}`}
                    value={proxy.host}
                    placeholder="127.0.0.1"
                    spellCheck={false}
                    aria-label={t("settings.host")}
                    onChange={(e) => set({ host: e.target.value })}
                    onBlur={commit}
                  />
                  <span className="cm-set-endpoint__colon">:</span>
                  <input
                    className={`cm-input cm-input--port${portOk ? "" : " is-invalid"}`}
                    type="number"
                    min={1}
                    max={65535}
                    value={proxy.port}
                    aria-label={t("settings.port")}
                    onChange={(e) => set({ port: Number(e.target.value) || 0 })}
                    onBlur={commit}
                  />
                </div>

                {proxyStatus && (
                  <div className="cm-set-proxy__status" aria-live="polite">
                    {proxyStatus}
                  </div>
                )}
              </div>
            )}
          </section>

          <CloudAgentSection />

          <FfmpegSection />

          <VersionSection />

          <AboutFooterSection />
        </div>
      </div>
    </div>
  );
}

const CAMEO_WEBSITE = "https://cameo.ink";
const CAMEO_CONTACT = "myagents.io@gmail.com";
const CAMEO_GITHUB = "https://github.com/hAcKlyc/cameo";
const CAMEO_CLOUD_API_BASE = "https://cameo-cloud.myagents-io.workers.dev";
/** The WeChat group QR is the same image cameo.ink serves for the website's
 *  download CTA. We reference it by URL (CSP is null, so the remote <img> loads
 *  directly) rather than bundling it — `npm run qr:update` over in cameo_web
 *  then refreshes both the site and every installed client at once, no release
 *  needed. */
const COMMUNITY_QR_URL = "https://cameo.ink/images/userfeedback_wechat_group_qr.webp";

/** Bottom-of-settings "about" footer, mirroring the website footer: a WeChat
 *  community-group QR card plus developer/contact links and a copyright line.
 *  Kept separate from VersionSection so the version/update controls stay
 *  focused. Links open in the system browser via the opener plugin. */
function AboutFooterSection() {
  const t = useT();
  // The WeChat community group is China-only, so the QR card shows only when the
  // UI is in Chinese; English users see just the developer/contact footer.
  const lang = useLocaleStore((s) => s.lang);
  // If the QR can't be fetched (offline / asset moved) hide the card rather than
  // showing a broken-image glyph.
  const [qrFailed, setQrFailed] = useState(false);

  const open = (url: string) => {
    void openUrl(url).catch(() => {});
  };

  return (
    <>
      {lang === "zh" && !qrFailed && (
        <section className="cm-set-section cm-about-qr">
          <p className="cm-about-qr__title">{t("settings.community")}</p>
          <p className="cm-about-qr__desc">{t("settings.communityDesc")}</p>
          <img
            className="cm-about-qr__img"
            src={COMMUNITY_QR_URL}
            alt={t("settings.communityQrAlt")}
            width={168}
            height={168}
            loading="lazy"
            onError={() => setQrFailed(true)}
          />
        </section>
      )}

      <section className="cm-set-section cm-about-meta">
        <div className="cm-about-meta__grid">
          <div className="cm-about-meta__cell">
            <p className="cm-about-meta__label">{t("settings.metaDeveloper")}</p>
            <p className="cm-about-meta__value">Ethan L</p>
          </div>
          <div className="cm-about-meta__cell">
            <p className="cm-about-meta__label">{t("settings.metaWebsite")}</p>
            <button type="button" className="cm-about-meta__link" onClick={() => open(CAMEO_WEBSITE)}>
              cameo.ink
            </button>
          </div>
          <div className="cm-about-meta__cell">
            <p className="cm-about-meta__label">{t("settings.metaContact")}</p>
            <button
              type="button"
              className="cm-about-meta__link"
              onClick={() => open(`mailto:${CAMEO_CONTACT}`)}
            >
              {CAMEO_CONTACT}
            </button>
          </div>
          <div className="cm-about-meta__cell">
            <p className="cm-about-meta__label">{t("settings.metaGithub")}</p>
            <button type="button" className="cm-about-meta__link" onClick={() => open(CAMEO_GITHUB)}>
              github.com/hAcKlyc/cameo
            </button>
          </div>
        </div>
        <p className="cm-about-meta__copyright">© 2026 Ethan L. All rights reserved.</p>
      </section>
    </>
  );
}

function CloudAgentSection() {
  const [status, setStatus] = useState<CloudAgentStatus | null>(null);
  const [apiBase, setApiBase] = useState(CAMEO_CLOUD_API_BASE);
  const [pairingCode, setPairingCode] = useState("");
  const [name, setName] = useState("Cameo Agent");
  const [maxConcurrentSessions, setMaxConcurrentSessions] = useState(6);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = () => {
    void ipc.cloudAgentStatus().then((next) => {
      setStatus(next);
      if (next.apiBase) setApiBase(next.apiBase);
      if (next.name) setName(next.name);
      if (next.maxConcurrentSessions) setMaxConcurrentSessions(next.maxConcurrentSessions);
    }).catch((e) => setError(String(e)));
  };

  useEffect(() => {
    refresh();
    const unlisten = listen<{ kind: string; message: string }>("cloud-agent-event", (event) => {
      if (event.payload.kind === "error") setError(event.payload.message);
      refresh();
    });
    return () => {
      void unlisten.then((f) => f());
    };
  }, []);

  const pair = () => {
    setBusy(true);
    setError(null);
    void ipc
      .cloudAgentPair(apiBase, pairingCode, name, maxConcurrentSessions)
      .then((next) => {
        setStatus(next);
        setPairingCode("");
      })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };

  const toggle = () => {
    setBusy(true);
    setError(null);
    const op = status?.running ? ipc.cloudAgentStop() : ipc.cloudAgentStart();
    void op
      .then(refresh)
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };

  return (
    <section className="cm-set-section">
      <div className="cm-set-section__head">
        <div className="cm-set-section__text">
          <h3 className="cm-set-section__title">Cloud Agent</h3>
          <p className="cm-set-section__desc">
            Bind this machine to a Cameo Cloud Team and run assigned workspace jobs locally.
          </p>
        </div>
        <div className="cm-set-section__control">
          <span className={`cm-status cm-status--${status?.running ? "ok" : status?.configured ? "idle" : "err"}`}>
            {status?.running ? "Running" : status?.configured ? "Paired" : "Not paired"}
          </span>
          {status?.configured && (
            <button className="cm-btn cm-btn--secondary" onClick={toggle} disabled={busy}>
              {status.running ? "Stop" : "Start"}
            </button>
          )}
        </div>
      </div>

      <div className="cm-set-proxy">
        <div className="cm-set-proxy__row">
          <input
            className="cm-input cm-input--host"
            value={apiBase}
            spellCheck={false}
            aria-label="Cloud API base"
            onChange={(e) => setApiBase(e.target.value)}
          />
          <input
            className="cm-input"
            value={pairingCode}
            placeholder="Pairing code"
            spellCheck={false}
            aria-label="Pairing code"
            onChange={(e) => setPairingCode(e.target.value)}
          />
        </div>
        <div className="cm-set-proxy__row">
          <input
            className="cm-input cm-input--host"
            value={name}
            placeholder="Agent name"
            aria-label="Agent name"
            onChange={(e) => setName(e.target.value)}
          />
          <input
            className="cm-input cm-input--port"
            type="number"
            min={1}
            max={24}
            value={maxConcurrentSessions}
            aria-label="Max concurrent sessions"
            onChange={(e) => setMaxConcurrentSessions(Math.max(1, Math.min(24, Number(e.target.value) || 1)))}
          />
          <button className="cm-btn cm-btn--primary" onClick={pair} disabled={busy || !pairingCode.trim()}>
            Pair
          </button>
        </div>
        {status?.agentId && (
          <p className="cm-set-hint">
            Agent {status.agentId} · Team {status.teamId}
          </p>
        )}
        {error && <p className="cm-set-hint cm-set-hint--err">{error}</p>}
      </div>
    </section>
  );
}

/** Managed ffmpeg/ffprobe status + one-click install (decision D1: detect the
 *  user's own install first, else download a pinned build into ~/.cameo/bin).
 *  The video modality is unavailable until this is ready. */
function FfmpegSection() {
  const t = useT();
  const [status, setStatus] = useState<FfmpegStatus | null>(null);
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    void ipc.toolStatus().then(setStatus).catch(() => setStatus(null));
  };

  useEffect(() => {
    refresh();
    const unlisten = [
      listen<{ downloaded: number; total: number }>("ffmpeg:progress", (e) => {
        const { downloaded, total } = e.payload;
        setProgress(total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : null);
      }),
      listen("ffmpeg:done", () => {
        setProgress(null);
        setError(null);
        refresh();
        // A manual install can finish while a poster-less video sits on the
        // board — backfill its poster now so it stops showing as a placeholder.
        void useBoardStore.getState().backfillVideoPosters();
      }),
      listen<string>("ffmpeg:failed", (e) => {
        setProgress(null);
        setError(String(e.payload));
        refresh();
      }),
    ];
    return () => {
      void Promise.all(unlisten).then((fns) => fns.forEach((f) => f()));
    };
  }, []);

  const installing = status?.state === "installing" || progress !== null;
  const install = () => {
    setError(null);
    setProgress(0);
    void ipc.toolInstall().catch((e) => {
      setProgress(null);
      // A concurrent install (e.g. the auto-trigger fired first) isn't a failure
      // — don't show it as a red error; the progress events will drive the UI.
      const msg = String(e);
      if (!msg.includes("install already in progress")) setError(msg);
    });
  };

  const stateLabel = (): string => {
    if (installing) return t("ffmpeg.installing");
    switch (status?.state) {
      case "ready":
        return t("ffmpeg.ready");
      case "failed":
        return t("ffmpeg.failed");
      default:
        return t("ffmpeg.missing");
    }
  };

  const ready = status?.state === "ready" && !!status.version;
  const tone = status?.state === "ready" ? "ok" : status?.state === "failed" ? "err" : "idle";

  return (
    <section className="cm-set-section">
      <div className="cm-set-section__head">
        <div className="cm-set-section__text">
          <h3 className="cm-set-section__title">{t("ffmpeg.title")}</h3>
          <p
            className={`cm-set-section__desc${ready ? " cm-set-section__desc--mono" : ""}`}
            title={ready ? status!.version! : undefined}
          >
            {ready ? shortFfmpegVersion(status!.version!) : t("ffmpeg.desc")}
          </p>
        </div>
        <div className="cm-set-section__control">
          <span className={`cm-status cm-status--${tone}`}>
            {stateLabel()}
            {installing && progress !== null ? ` · ${progress}%` : ""}
          </span>
          {status?.state !== "ready" && !installing && (
            <button className="cm-btn cm-btn--primary" onClick={install}>
              {t("ffmpeg.install")}
            </button>
          )}
        </div>
      </div>
      {(error || status?.error) && (
        <p className="cm-set-hint cm-set-hint--err">{error || status?.error}</p>
      )}
    </section>
  );
}
