import { AlertTriangle, Download, History, Loader2, RotateCw } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useUpdaterStore } from "../store/updater";
import { useToastStore } from "../store/toast";
import { appVersion } from "../services/cloud";
import { ipc } from "../lib/ipc";
import { useT } from "../i18n/locale";

/** Whole-project release notes page (every version's changelog). */
const RELEASES_URL = "https://github.com/hAcKlyc/cameo/releases";

/**
 * Settings → Version section. Shows the running version plus two actions:
 *   - 检查更新: manually triggers the (otherwise silent, automatic) update
 *     pipeline. While downloading, this button is replaced by a live progress
 *     bar; once ready it becomes 重启更新. Up-to-date / failure → toast.
 *   - 更新记录: opens the GitHub releases page (full changelog) in the browser.
 *
 * State lives in store/updater.ts, shared with the topbar UpdateIndicator, so a
 * download started here also lights up the topbar button (and vice versa).
 */
export function VersionSection() {
  const t = useT();
  const phase = useUpdaterStore((s) => s.phase);
  const progress = useUpdaterStore((s) => s.progress);
  const error = useUpdaterStore((s) => s.error);
  const check = useUpdaterStore((s) => s.check);
  const install = useUpdaterStore((s) => s.install);
  const showToast = useToastStore((s) => s.show);

  const onCheck = async () => {
    const result = await check();
    if (result === "latest") showToast(t("update.upToDate"), "success");
    else if (result === "error") showToast(t("update.checkFailed"), "error");
  };

  const onInstall = async () => {
    const result = await install();
    // Failure where the bytes were discarded drops the button back to "检查更新";
    // a silent flip after a click is confusing, so surface it. A retryable
    // failure keeps the red retry button (its own feedback) — no toast.
    if (result === "cleared") showToast(t("update.installFailed"), "error");
  };

  const onReleaseNotes = () => {
    void openUrl(RELEASES_URL).catch((e) => {
      showToast(t("update.openFailed"), "error");
      void ipc.frontLog("warn", `open release notes failed: ${e instanceof Error ? e.message : String(e)}`).catch(() => {});
    });
  };

  // Determinate only when Rust gave us a content-length; chunked/proxied
  // responses arrive without one, so fall back to an indeterminate bar rather
  // than a bar frozen at 0%.
  const determinate = !!progress && progress.total > 0;
  const pct = determinate
    ? Math.min(100, Math.round((progress!.downloaded / progress!.total) * 100))
    : 0;

  return (
    <section className="cm-set-section">
      <h3 className="cm-set-section__title">{t("settings.about")}</h3>
      <div className="cm-set-card">
        <div className="cm-set-row">
          <span className="cm-set-row__label">{t("settings.version")}</span>
          <span className="cm-set-version">v{appVersion()}</span>
        </div>
      </div>

      <div className="cm-set-update">
        {phase === "downloading" ? (
          <div
            className="cm-progress"
            role="progressbar"
            aria-valuenow={determinate ? pct : undefined}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <div className="cm-progress__track">
              {determinate ? (
                <div className="cm-progress__fill" style={{ width: `${pct}%` }} />
              ) : (
                <div className="cm-progress__fill cm-progress__fill--indeterminate" />
              )}
            </div>
            <span className="cm-progress__label">
              {determinate ? t("update.downloading", { pct: String(pct) }) : t("update.downloadingWait")}
            </span>
          </div>
        ) : phase === "ready" || phase === "installing" ? (
          <button
            type="button"
            className={`cm-btn cm-btn--primary${error ? " is-error" : ""}`}
            onClick={() => void onInstall()}
            disabled={phase === "installing"}
          >
            {phase === "installing" ? (
              <>
                <Loader2 size={14} className="cm-update-btn__spin" />
                {t("update.installing")}
              </>
            ) : error ? (
              <>
                <AlertTriangle size={14} />
                {t("update.retryButton")}
              </>
            ) : (
              <>
                <RotateCw size={14} />
                {t("update.button")}
              </>
            )}
          </button>
        ) : (
          <button
            type="button"
            className="cm-btn cm-btn--primary"
            onClick={() => void onCheck()}
            disabled={phase === "checking"}
          >
            {phase === "checking" ? (
              <>
                <Loader2 size={14} className="cm-update-btn__spin" />
                {t("update.checking")}
              </>
            ) : (
              <>
                <Download size={14} />
                {t("update.check")}
              </>
            )}
          </button>
        )}

        <button type="button" className="cm-btn" onClick={onReleaseNotes}>
          <History size={14} />
          {t("update.releaseNotes")}
        </button>
      </div>
    </section>
  );
}
