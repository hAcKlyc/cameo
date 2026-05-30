import { RotateCw, Loader2, TriangleAlert } from "lucide-react";
import { useT } from "../i18n/locale";
import { useUpdaterStore } from "../store/updater";

/**
 * Topbar "重启更新" button. Renders only when there's a pending update on
 * disk (silent download has completed). One click → graceful Codex sidecar
 * shutdown → install + relaunch.
 *
 * Shares state with the Settings → Version section via store/updater.ts, so a
 * download triggered from Settings lights this button up too. Stays hidden
 * while a download is still in progress (that progress shows in Settings).
 *
 * Mounted in the topbar to the LEFT of the Settings gear so users see it
 * exactly where they're already looking when they click anything in the
 * top-right cluster.
 */
export function UpdateIndicator() {
  const t = useT();
  const phase = useUpdaterStore((s) => s.phase);
  const pendingVersion = useUpdaterStore((s) => s.pendingVersion);
  const error = useUpdaterStore((s) => s.error);
  const install = useUpdaterStore((s) => s.install);

  if ((phase !== "ready" && phase !== "installing") || !pendingVersion) return null;
  const installing = phase === "installing";

  return (
    <button
      type="button"
      className={`cm-update-btn${error ? " is-error" : ""}`}
      title={error ? t("update.errorTooltip", { error }) : t("update.tooltip", { version: pendingVersion })}
      onClick={() => void install()}
      disabled={installing}
    >
      {installing ? (
        <Loader2 size={13} className="cm-update-btn__spin" />
      ) : error ? (
        <TriangleAlert size={13} />
      ) : (
        <RotateCw size={13} />
      )}
      <span>{error ? t("update.retryButton") : t("update.button")}</span>
    </button>
  );
}
