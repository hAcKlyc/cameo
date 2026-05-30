import { useEffect, useState } from "react";
import { X, Copy, AlertCircle, ExternalLink } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useT } from "../../i18n/locale";
import type { GalleryItem } from "../../services/cloud/gallery";
import { proxiedImg } from "../../services/cloud";
import { useComposerStore } from "../../store/composer";

interface Props {
  /** The full list-item record. Carries prompt + img_full + source — no
   *  network round-trip needed when this opens. */
  item: GalleryItem;
  onClose: () => void;
  /** Called after "use this prompt" so the parent overlay can close too. */
  onUsePrompt: () => void;
}

/**
 * Detail panel for one gallery prompt. Stacks ABOVE the GalleryOverlay
 * (separate backdrop, Esc closes only this, not the overlay underneath).
 * Renders synchronously from the item prop — no loading state, no fetch.
 */
export function GalleryDetail({ item, onClose, onUsePrompt }: Props) {
  const t = useT();
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.stopPropagation(); onClose() }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [onClose]);

  const usePrompt = () => {
    useComposerStore.getState().injectPrompt(item.prompt);
    onUsePrompt();
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(item.prompt);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* ignore */
    }
  };

  const title = item.t_zh || item.t_en;

  return (
    <div
      className="cm-gdetail-backdrop"
      // stopPropagation so this click doesn't also bubble up to GalleryOverlay's
      // backdrop (the detail is rendered as a child of the overlay in the React
      // tree — without this we'd close both layers in one click).
      onClick={(e) => { e.stopPropagation(); onClose() }}
    >
      <div className="cm-gdetail" onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true">
        <button className="cm-gdetail__close" onClick={onClose} aria-label={t("gallery.close")}>
          <X size={16} />
        </button>

        <div className="cm-gdetail__media">
          <img src={proxiedImg(item.img_full || item.img)} alt={title || item.id} />
        </div>

        <div className="cm-gdetail__body">
          {title && <h3 className="cm-gdetail__title">{title}</h3>}

          <div className="cm-gdetail__tags">
            <span className="cm-gdetail__tag cm-gdetail__tag--uc">{item.uc}</span>
            {item.tags.slice(0, 6).map((tg) => (
              <span key={tg} className="cm-gdetail__tag">{tg}</span>
            ))}
            {item.inp && (
              <span className="cm-gdetail__tag cm-gdetail__tag--warn" title={t("gallery.needsInputImage")}>
                <AlertCircle size={11} /> {t("gallery.needsInputImage")}
              </span>
            )}
          </div>

          <pre className="cm-gdetail__prompt">{item.prompt}</pre>

          {(item.repo || (item.author && item.url)) && (
            <dl className="cm-gdetail__source">
              {item.repo && (
                <div className="cm-gdetail__sourcerow">
                  <dt>{t("gallery.repo")}</dt>
                  <dd>
                    <button
                      type="button"
                      className="cm-gdetail__sourcelink"
                      onClick={() => void openUrl(`https://github.com/${item.repo}`)}
                    >
                      {item.repo}
                      <ExternalLink size={11} />
                    </button>
                  </dd>
                </div>
              )}
              {item.author && item.url && (
                <div className="cm-gdetail__sourcerow">
                  <dt>{t("gallery.author")}</dt>
                  <dd>
                    <button
                      type="button"
                      className="cm-gdetail__sourcelink"
                      onClick={() => void openUrl(item.url!)}
                    >
                      {item.author}
                      <ExternalLink size={11} />
                    </button>
                  </dd>
                </div>
              )}
            </dl>
          )}

          <div className="cm-gdetail__actions">
            <button className="cm-btn" onClick={() => void copy()}>
              <Copy size={13} />
              {copied ? t("settings.copied") : t("gallery.copyPrompt")}
            </button>
            <button className="cm-btn cm-btn--primary" onClick={usePrompt}>
              {item.inp ? t("gallery.usePromptInput") : t("gallery.usePrompt")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
