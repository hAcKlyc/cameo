import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { X } from "lucide-react";
import { useT } from "../../i18n/locale";
import { fetchItems, fetchFacets, type Facet, type GalleryItem, type ItemsResponse } from "../../services/cloud/gallery";
import { CloudError } from "../../services/cloud";
import { GalleryCard } from "./GalleryCard";
import { GalleryDetail } from "./GalleryDetail";

type ErrCode = "unauthorized" | "forbidden" | "rateLimited" | "network" | "generic";

function mapError(e: unknown): ErrCode {
  if (e instanceof CloudError) {
    if (e.status === 401) return "unauthorized";
    if (e.status === 403) return "forbidden";
    if (e.status === 429) return "rateLimited";
    if (e.code === "network_error") return "network";
  }
  return "generic";
}

const PAGE = 50;

// Match the prior CSS breakpoints (column-count media queries in app.css)
// so the visual rhythm is unchanged when resizing.
function columnsForWidth(w: number): number {
  if (w < 760) return 2;
  if (w < 1100) return 3;
  if (w < 1400) return 4;
  return 5;
}

/**
 * Full-screen Pinterest-style overlay. CSS-columns masonry (no JS layout),
 * IntersectionObserver-driven infinite scroll, server-side filtering via
 * facets + search. Backdrop blur + Esc / click-out to close.
 */
export function GalleryOverlay({ onClose }: { onClose: () => void }) {
  const t = useT();

  const [items, setItems] = useState<GalleryItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<ErrCode | null>(null);

  const [useCase, setUseCase] = useState<string>("");
  const [facets, setFacets] = useState<{ use_case: Facet[]; lang: Facet[] } | null>(null);
  // We show the detail modal with an item we already have in `items` — no
  // network round-trip on click. The list response carries every field the
  // detail view needs (prompt, full image url, source, etc.).
  const [detailItem, setDetailItem] = useState<GalleryItem | null>(null);
  // Forces the load effect to re-run even when filters haven't changed (Retry).
  const [retryNonce, setRetryNonce] = useState(0);
  // Pinned seeded-shuffle bucket. First /items response sets this; subsequent
  // page loads + filter changes pass it back so the order stays consistent
  // for the lifetime of one overlay open (closing and reopening picks fresh).
  const bucketRef = useRef<number | undefined>(undefined);
  const inFlightRef = useRef(false);
  const offsetRef = useRef(0);
  const hasMoreRef = useRef(true);
  const errorRef = useRef<ErrCode | null>(null);

  const sentinelRef = useRef<HTMLDivElement>(null);

  // Facets are static-ish; fetch once on open. Abort if user closes mid-load.
  useEffect(() => {
    const ctrl = new AbortController();
    void (async () => {
      try {
        const f = await fetchFacets({ signal: ctrl.signal });
        if (!ctrl.signal.aborted) setFacets(f);
      } catch {
        // Filter chips just won't render. Not blocking.
      }
    })();
    return () => ctrl.abort();
  }, []);

  const queryKey = useMemo(() => `${useCase}|${retryNonce}`, [useCase, retryNonce]);

  // Initial / filter-changed / retry load. AbortController aborts the in-flight
  // fetch when filters change or the overlay closes — saves a rate-limit slot
  // and prevents wasted bandwidth on slow connections.
  useEffect(() => {
    const ctrl = new AbortController();
    inFlightRef.current = true;
    offsetRef.current = 0;
    hasMoreRef.current = true;
    errorRef.current = null;
    setItems([]);
    setError(null);
    setLoading(true);
    void (async () => {
      try {
        const params: Record<string, string | number> = { limit: PAGE, offset: 0 };
        if (useCase) params.use_case = useCase;
        if (bucketRef.current !== undefined) params._b = bucketRef.current;
        const r: ItemsResponse = await fetchItems(params, { signal: ctrl.signal });
        if (ctrl.signal.aborted) return;
        if (bucketRef.current === undefined) bucketRef.current = r.bucket;
        setItems(r.items);
        offsetRef.current = r.items.length;
        const more = r.items.length >= PAGE;
        hasMoreRef.current = more;
      } catch (e) {
        if (ctrl.signal.aborted || (e as Error).name === "AbortError") return;
        const mapped = mapError(e);
        errorRef.current = mapped;
        setError(mapped);
      } finally {
        if (!ctrl.signal.aborted) {
          inFlightRef.current = false;
          setLoading(false);
        }
      }
    })();
    return () => {
      ctrl.abort();
      inFlightRef.current = false;
    };
  }, [queryKey, useCase]);

  const loadMore = useCallback(async () => {
    if (inFlightRef.current || errorRef.current || !hasMoreRef.current) return;
    inFlightRef.current = true;
    setLoading(true);
    try {
      const pageOffset = offsetRef.current;
      const params: Record<string, string | number> = { limit: PAGE, offset: pageOffset };
      if (useCase) params.use_case = useCase;
      if (bucketRef.current !== undefined) params._b = bucketRef.current;
      const r = await fetchItems(params);
      setItems((prev) => [...prev, ...r.items]);
      const nextOffset = pageOffset + r.items.length;
      offsetRef.current = nextOffset;
      if (r.items.length < PAGE) {
        hasMoreRef.current = false;
      }
    } catch (e) {
      const mapped = mapError(e);
      errorRef.current = mapped;
      setError(mapped);
    } finally {
      inFlightRef.current = false;
      setLoading(false);
    }
  }, [useCase]);

  // Infinite scroll: observe the sentinel at the bottom.
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el) return;
    const obs = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) void loadMore();
    }, { rootMargin: "200px" });
    obs.observe(el);
    return () => obs.disconnect();
  }, [loadMore]);

  // Esc to close.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose() };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // True masonry: N independent flex columns + greedy shortest-column
  // distribution. CSS `column-count` reshuffles ALL existing cards on every
  // infinite-scroll append (it's a flow layout, not a sticky one), which
  // makes the page visibly jump. We use server-provided w/h to predict each
  // card's height without waiting for image load, so order is stable and
  // append-only — a card placed in column 2 stays in column 2 forever.
  const [cols, setCols] = useState(() =>
    columnsForWidth(typeof window !== "undefined" ? window.innerWidth : 1280),
  );
  useEffect(() => {
    const onResize = () => setCols(columnsForWidth(window.innerWidth));
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const columns = useMemo(() => {
    const buckets: GalleryItem[][] = Array.from({ length: cols }, () => []);
    const heights = new Array(cols).fill(0);
    for (const it of items) {
      let minI = 0;
      for (let i = 1; i < cols; i++) if (heights[i] < heights[minI]) minI = i;
      buckets[minI].push(it);
      const aspect = it.w && it.h && it.w > 0 ? it.h / it.w : 1.25;
      heights[minI] += aspect + 0.12; // small constant for the meta/gap area
    }
    return buckets;
  }, [items, cols]);

  // 401 / 403 / 429 are not retriable from the client side — hiding Retry for
  // those (and surfacing only the explanatory copy) matches PRD §6.6. Only
  // transient errors (network / generic 5xx) get a Retry affordance.
  const isRetriable = error === "network" || error === "generic";
  const retry = () => {
    setError(null);
    setRetryNonce((n) => n + 1);
  };

  return (
    <div className="cm-gallery-backdrop" onClick={onClose}>
      <div className="cm-gallery" onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true">
        <header className="cm-gallery__head">
          <h2 className="cm-gallery__title">{t("gallery.title")}</h2>
          <button className="cm-gallery__close" onClick={onClose} aria-label={t("gallery.close")}>
            <X size={16} />
          </button>
        </header>

        {facets?.use_case?.length ? (
          <CategoryChips
            facets={facets.use_case}
            value={useCase}
            onChange={setUseCase}
            allLabel={t("gallery.allFilter")}
            moreLabel={t("gallery.more")}
          />
        ) : null}

        <div className="cm-gallery__body">
          {error ? (
            <div className="cm-gallery__error">
              <p>{t(`gallery.error.${error}` as const)}</p>
              {isRetriable && (
                <button className="cm-btn" onClick={retry}>{t("gallery.retry")}</button>
              )}
            </div>
          ) : items.length === 0 && !loading ? (
            <p className="cm-gallery__empty">{t("gallery.empty")}</p>
          ) : (
            <div className="cm-gallery__masonry" style={{ gridTemplateColumns: `repeat(${cols}, 1fr)` }}>
              {columns.map((col, ci) => (
                <div className="cm-gallery__col" key={ci}>
                  {col.map((it) => (
                    <GalleryCard key={it.id} item={it} onOpen={() => setDetailItem(it)} />
                  ))}
                </div>
              ))}
            </div>
          )}

          {!error && (
            <>
              <div ref={sentinelRef} className="cm-gallery__sentinel" />
              {loading && <p className="cm-gallery__loading">{items.length === 0 ? t("gallery.loading") : t("gallery.loadingMore")}</p>}
            </>
          )}
        </div>
      </div>

      {detailItem && (
        <GalleryDetail
          item={detailItem}
          onClose={() => setDetailItem(null)}
          onUsePrompt={() => {
            setDetailItem(null);
            onClose();
          }}
        />
      )}
    </div>
  );
}

/**
 * Single-row category chips with overflow into a "More" dropdown.
 *
 * Renders "All" + one chip per facet inline; chips that don't fit collapse
 * into a popover. The fit calculation runs against an offscreen measuring
 * row (same chip content, same fonts) — we measure each chip's right edge
 * vs the visible row width minus the More-button width, and cut at the
 * first overflow.
 *
 * The currently-selected chip is always hoisted into the visible row even
 * if it would otherwise overflow, so the active filter is one click away.
 */
function CategoryChips({
  facets, value, onChange, allLabel, moreLabel,
}: {
  facets: Facet[];
  value: string;
  onChange: (v: string) => void;
  allLabel: string;
  moreLabel: string;
}) {
  const rowRef = useRef<HTMLDivElement>(null);
  const measureRef = useRef<HTMLDivElement>(null);
  const moreBtnRef = useRef<HTMLDivElement>(null);
  const [visibleCount, setVisibleCount] = useState(facets.length);
  const [moreOpen, setMoreOpen] = useState(false);

  const recompute = useCallback(() => {
    const row = rowRef.current;
    const measure = measureRef.current;
    if (!row || !measure) return;
    const moreW = moreBtnRef.current?.offsetWidth ?? 78;
    const budget = row.getBoundingClientRect().right - moreW - 8;
    const chipNodes = Array.from(measure.querySelectorAll<HTMLElement>("[data-chip-idx]"));
    let fit = 0;
    for (const node of chipNodes) {
      if (node.getBoundingClientRect().right <= budget) fit++;
      else break;
    }
    setVisibleCount(fit);
  }, []);

  useLayoutEffect(() => { recompute() }, [recompute, facets]);

  useLayoutEffect(() => {
    const row = rowRef.current;
    if (!row || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => recompute());
    ro.observe(row);
    return () => ro.disconnect();
  }, [recompute]);

  useEffect(() => {
    if (!moreOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (!moreBtnRef.current?.contains(e.target as Node)) setMoreOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [moreOpen]);

  const selectedIdx = value ? facets.findIndex((f) => f.k === value) : -1;
  let visible = facets.slice(0, Math.max(visibleCount, 0));
  let overflow = facets.slice(Math.max(visibleCount, 0));
  if (selectedIdx >= visibleCount && selectedIdx >= 0) {
    const selected = facets[selectedIdx];
    visible = [...visible.slice(0, -1), selected];
    overflow = facets.filter((f) => f.k !== selected.k).slice(Math.max(visibleCount - 1, 0));
  }

  return (
    <div className="cm-gallery__filters" ref={rowRef}>
      <button
        type="button"
        className={`cm-gallery__chip${!value ? " is-on" : ""}`}
        onClick={() => onChange("")}
      >
        {allLabel}
      </button>
      {visible.map((f) => (
        <button
          key={f.k}
          type="button"
          className={`cm-gallery__chip${value === f.k ? " is-on" : ""}`}
          onClick={() => onChange(f.k)}
        >
          {f.k}
        </button>
      ))}
      {overflow.length > 0 && (
        <div className="cm-gallery__more" ref={moreBtnRef}>
          <button
            type="button"
            className={`cm-gallery__chip cm-gallery__chip--more${moreOpen ? " is-open" : ""}`}
            onClick={() => setMoreOpen((v) => !v)}
            aria-haspopup="true"
            aria-expanded={moreOpen}
          >
            {moreLabel} <span className="cm-gallery__caret">▾</span>
          </button>
          {moreOpen && (
            <div className="cm-gallery__menu" role="menu">
              {overflow.map((f) => (
                <button
                  key={f.k}
                  type="button"
                  className={`cm-gallery__menuitem${value === f.k ? " is-on" : ""}`}
                  onClick={() => { onChange(f.k); setMoreOpen(false) }}
                  role="menuitem"
                >
                  {f.k}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Offscreen measuring row — same chip widths as the visible row. */}
      <div className="cm-gallery__measure" ref={measureRef} aria-hidden="true">
        <span className="cm-gallery__chip" data-chip-idx="all">{allLabel}</span>
        {facets.map((f, i) => (
          <span key={f.k} className="cm-gallery__chip" data-chip-idx={i}>{f.k}</span>
        ))}
      </div>
    </div>
  );
}
