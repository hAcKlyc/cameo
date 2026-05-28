import { useEffect, useRef, useState } from "react";
import { AlertCircle, CheckCircle2, Loader2, X } from "lucide-react";
import { useSettingsStore } from "../store/settings";
import { useT, useLocaleStore, type LocaleChoice } from "../i18n/locale";
import { ipc } from "../lib/ipc";
import type { MsgKey } from "../i18n/messages";
import type { ProxyProbeResult, ProxySettings } from "../types";

const PROTOCOLS: ProxySettings["protocol"][] = ["http", "socks5"];

const isValidHost = (h: string) => {
  const v = h.trim();
  return !!v && !v.includes("://") && !v.includes("@") && !v.includes("/") && v.length <= 253;
};
const isValidPort = (p: number) => p >= 1 && p <= 65535;

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
            <h3 className="cm-set-section__title">{t("settings.general")}</h3>
            <div className="cm-set-card">
              <div className="cm-set-row">
                <span className="cm-set-row__label">{t("settings.language")}</span>
                <select
                  className="cm-select"
                  value={localeChoice}
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
              <h3 className="cm-set-section__title">{t("settings.tray")}</h3>
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
            <p className="cm-set-section__desc">{t("settings.trayDesc")}</p>
          </section>

          <section className="cm-set-section">
            <div className="cm-set-section__head">
              <h3 className="cm-set-section__title">{t("settings.proxy")}</h3>
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
            <p className="cm-set-section__desc">{t("settings.proxyDesc")}</p>

            {proxy.enabled && (
              <>
                <div className="cm-set-card">
                  <div className="cm-set-row">
                    <span className="cm-set-row__label">{t("settings.protocol")}</span>
                    <select
                      className="cm-select"
                      value={proxy.protocol}
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
                  </div>

                  <div className="cm-set-row">
                    <span className="cm-set-row__label">{t("settings.address")}</span>
                    <div className="cm-set-endpoint">
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
                  </div>
                </div>

                {!hostOk || !portOk ? (
                  <p className="cm-set-hint cm-set-hint--err">{t("settings.invalid")}</p>
                ) : applying ? (
                  <p className="cm-set-hint">{t("settings.proxyApplying")}</p>
                ) : proxyProbe.status !== "idle" ? (
                  <div
                    className={`cm-set-probe cm-set-probe--${proxyProbe.status}`}
                    title={proxyProbeDetail ?? undefined}
                    aria-live="polite"
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
                ) : null}
              </>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}
