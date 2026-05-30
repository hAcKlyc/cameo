import { AlertCircle } from "lucide-react";
import type { GalleryItem } from "../../services/cloud/gallery";
import { proxiedImg } from "../../services/cloud";
import { useT } from "../../i18n/locale";

/**
 * One card in the masonry. Uses the server-returned `w/h` to draw an aspect-
 * ratio placeholder (the image then loads into the same box, no reflow).
 */
export function GalleryCard({ item, onOpen }: { item: GalleryItem; onOpen: () => void }) {
  const t = useT();
  const ratio = item.w && item.h && item.w > 0 ? `${item.w} / ${item.h}` : "1 / 1";
  const title = item.t_zh || item.t_en;

  return (
    <button type="button" className="cm-gcard" onClick={onOpen}>
      <div className="cm-gcard__media" style={{ aspectRatio: ratio }}>
        {item.img ? (
          <img src={proxiedImg(item.img)} alt={title || item.id} loading="lazy" />
        ) : (
          <div className="cm-gcard__noimg" />
        )}
        {item.inp && (
          <span className="cm-gcard__badge" title={t("gallery.needsInputImage")}>
            <AlertCircle size={11} />
          </span>
        )}
      </div>
      <div className="cm-gcard__meta">
        {title && <p className="cm-gcard__title">{title}</p>}
      </div>
    </button>
  );
}
