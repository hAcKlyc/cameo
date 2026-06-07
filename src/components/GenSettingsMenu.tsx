import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { Check, ChevronRight, Shield, Zap } from "lucide-react";
import { useT } from "../i18n/locale";
import type { MsgKey } from "../i18n/messages";
import { useBoardStore } from "../store/board";
import {
  useGenStore,
  effortsFor,
  tiersFor,
  type Effort,
} from "../store/genSettings";
import { usePermissionStore } from "../store/permissionSettings";
import type { ModelInfo, PermissionMode } from "../types";

/**
 * The composer's "model / 智能 / 速度" picker — mirrors the official Codex app's
 * input-box menu. A trigger pill shows `{model} {effort}`; clicking opens a
 * popover (opens upward, the composer sits at the bottom) with the effort list
 * inline and Model / Speed / Permission as left-flyout submenus.
 */

const EFFORT_KEY: Record<Effort, MsgKey> = {
  low: "composer.gen.effort.low",
  medium: "composer.gen.effort.medium",
  high: "composer.gen.effort.high",
  xhigh: "composer.gen.effort.xhigh",
};

/** "GPT-5.5" / "gpt-5.5" → "5.5" for the compact trigger label. */
function shortModel(label: string): string {
  return label.replace(/^gpt-/i, "");
}

function modelLabel(id: string, models: ModelInfo[]): string {
  return models.find((m) => m.id === id)?.displayName ?? id;
}

type Sub = "model" | "speed" | "permission" | null;
type FlyoutSide = "left" | "right";
type MenuPos = { right: number; bottom: number; flyoutSide: FlyoutSide };

const PERMISSIONS: PermissionMode[] = ["workspace", "fullAccess"];

function permissionLabel(mode: PermissionMode, t: (key: MsgKey) => string): string {
  return mode === "fullAccess"
    ? t("composer.gen.permission.fullAccess")
    : t("composer.gen.permission.workspace");
}

function permissionDesc(mode: PermissionMode, t: (key: MsgKey) => string): string {
  return mode === "fullAccess"
    ? t("composer.gen.permission.fullAccessDesc")
    : t("composer.gen.permission.workspaceDesc");
}

export function GenSettingsMenu() {
  const t = useT();
  const boardId = useBoardStore((s) => s.boardId);
  const model = useGenStore((s) => s.model);
  const effort = useGenStore((s) => s.effort);
  const serviceTier = useGenStore((s) => s.serviceTier);
  const models = useGenStore((s) => s.models);
  const permissionMode = usePermissionStore((s) => s.mode);

  const [open, setOpen] = useState(false);
  const [sub, setSub] = useState<Sub>(null);
  const [menuPos, setMenuPos] = useState<MenuPos | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (!open) return;
    const update = () => {
      const rect = rootRef.current?.getBoundingClientRect();
      if (!rect) return;
      const right = Math.max(8, window.innerWidth - rect.right);
      const menuWidth = menuRef.current?.offsetWidth ?? 196;
      const menuLeft = window.innerWidth - right - menuWidth;
      const flyoutSide: FlyoutSide = menuLeft >= 301 ? "left" : "right";
      setMenuPos({
        right,
        bottom: Math.max(8, window.innerHeight - rect.top + 6),
        flyoutSide,
      });
    };
    update();
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    window.visualViewport?.addEventListener("resize", update);
    return () => {
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
      window.visualViewport?.removeEventListener("resize", update);
    };
  }, [open]);

  // Dismiss on outside pointerdown / Escape (mirrors CanvasContextMenu).
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      const target = e.target as Node;
      if (!rootRef.current?.contains(target) && !menuRef.current?.contains(target)) {
        setOpen(false);
        setSub(null);
        setMenuPos(null);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        setSub(null);
        setMenuPos(null);
      }
    };
    window.addEventListener("pointerdown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const current = models.find((m) => m.id === model);
  const efforts = effortsFor(current);
  const tiers = tiersFor(current);
  const effortLabel = (e: string) =>
    (EXPOSED(e) ? t(EFFORT_KEY[e as Effort]) : e);

  const pick = (fn: () => void) => {
    fn();
    setOpen(false);
    setSub(null);
    setMenuPos(null);
  };
  const onSubKey = (next: Exclude<Sub, null>) => (e: ReactKeyboardEvent) => {
    if (e.key === "ArrowRight") {
      e.preventDefault();
      setSub(next);
    }
  };

  const triggerLabel = `${shortModel(modelLabel(model, models))} ${effortLabel(effort)}`;
  const tierLabel = serviceTier
    ? tiers.find((tr) => tr.id === serviceTier)?.name ?? serviceTier
    : t("composer.gen.speed.standard");
  const currentPermissionLabel = permissionLabel(permissionMode, t);

  return (
    <div className="cm-gen" ref={rootRef}>
      <button
        type="button"
        className="cm-gen__trigger"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={!boardId}
        title={t("composer.gen.menu")}
        onClick={() => {
          setMenuPos(null);
          setOpen((v) => !v);
          setSub(null);
        }}
      >
        <span className="cm-gen__triggerlabel">{triggerLabel}</span>
        <svg width="9" height="6" viewBox="0 0 9 6" aria-hidden className="cm-gen__caret">
          <path d="M1 1l3.5 3.5L8 1" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>

      {open && createPortal(
        <div
          className="cm-gen__menu"
          ref={menuRef}
          role="menu"
          data-flyout-side={menuPos?.flyoutSide ?? "left"}
          style={{
            right: menuPos?.right ?? 8,
            bottom: menuPos?.bottom ?? 8,
            visibility: menuPos ? "visible" : "hidden",
          }}
        >
          <div className="cm-gen__header">{t("composer.gen.intelligence")}</div>
          {efforts.map((e) => (
            <button
              key={e}
              type="button"
              className="cm-gen__item"
              role="menuitemradio"
              aria-checked={effort === e}
              onMouseEnter={() => setSub(null)}
              onClick={() => pick(() => useGenStore.getState().setEffort(e))}
            >
              <span className="cm-gen__label">{effortLabel(e)}</span>
              {effort === e && <Check size={15} className="cm-gen__check" />}
            </button>
          ))}

          <div className="cm-gen__sep" />

          {/* Model flyout */}
          <div className="cm-gen__row" onMouseEnter={() => setSub("model")}>
            <button
              type="button"
              className="cm-gen__item"
              role="menuitem"
              onFocus={() => setSub("model")}
              onClick={() => setSub("model")}
              onKeyDown={onSubKey("model")}
            >
              <span className="cm-gen__label">{modelLabel(model, models)}</span>
              <ChevronRight size={15} className="cm-gen__chev" />
            </button>
            {sub === "model" && models.length > 0 && (
              <div className="cm-gen__flyout" role="menu">
                <div className="cm-gen__header">{t("composer.gen.model")}</div>
                {models.map((m) => (
                  <button
                    key={m.id}
                    type="button"
                    className="cm-gen__item"
                    role="menuitemradio"
                    aria-checked={model === m.id}
                    onClick={() => pick(() => useGenStore.getState().setModel(m.id))}
                  >
                    <span className="cm-gen__label">{m.displayName}</span>
                    {model === m.id && <Check size={15} className="cm-gen__check" />}
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Speed flyout */}
          <div className="cm-gen__row" onMouseEnter={() => setSub("speed")}>
            <button
              type="button"
              className="cm-gen__item"
              role="menuitem"
              onFocus={() => setSub("speed")}
              onClick={() => setSub("speed")}
              onKeyDown={onSubKey("speed")}
            >
              <span className="cm-gen__label">{t("composer.gen.speed")}</span>
              <span className="cm-gen__value">{tierLabel}</span>
              <ChevronRight size={15} className="cm-gen__chev" />
            </button>
            {sub === "speed" && (
              <div className="cm-gen__flyout" role="menu">
                <div className="cm-gen__header">{t("composer.gen.speed")}</div>
                <button
                  type="button"
                  className="cm-gen__item cm-gen__item--rich"
                  role="menuitemradio"
                  aria-checked={serviceTier === null}
                  onClick={() => pick(() => useGenStore.getState().setServiceTier(null))}
                >
                  <span className="cm-gen__rich">
                    <span className="cm-gen__label">{t("composer.gen.speed.standard")}</span>
                    <span className="cm-gen__desc">{t("composer.gen.speed.standardDesc")}</span>
                  </span>
                  {serviceTier === null && <Check size={15} className="cm-gen__check" />}
                </button>
                {tiers.map((tr) => (
                  <button
                    key={tr.id}
                    type="button"
                    className="cm-gen__item cm-gen__item--rich"
                    role="menuitemradio"
                    aria-checked={serviceTier === tr.id}
                    onClick={() => pick(() => useGenStore.getState().setServiceTier(tr.id))}
                  >
                    <span className="cm-gen__rich">
                      <span className="cm-gen__label">
                        <Zap size={13} className="cm-gen__zap" /> {tr.name}
                      </span>
                      {tr.description && <span className="cm-gen__desc">{tr.description}</span>}
                    </span>
                    {serviceTier === tr.id && <Check size={15} className="cm-gen__check" />}
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Permission flyout */}
          <div className="cm-gen__sep" />
          <div className="cm-gen__row" onMouseEnter={() => setSub("permission")}>
            <button
              type="button"
              className="cm-gen__item"
              role="menuitem"
              onFocus={() => setSub("permission")}
              onClick={() => setSub("permission")}
              onKeyDown={onSubKey("permission")}
            >
              <span className="cm-gen__label">
                <Shield size={13} className="cm-gen__shield" /> {t("composer.gen.permission")}
              </span>
              <span className="cm-gen__value">{currentPermissionLabel}</span>
              <ChevronRight size={15} className="cm-gen__chev" />
            </button>
            {sub === "permission" && (
              <div className="cm-gen__flyout cm-gen__flyout--permission" role="menu">
                <div className="cm-gen__header">{t("composer.gen.permission")}</div>
                {PERMISSIONS.map((mode) => (
                  <button
                    key={mode}
                    type="button"
                    className="cm-gen__item cm-gen__item--rich"
                    role="menuitemradio"
                    aria-checked={permissionMode === mode}
                    onClick={() => pick(() => usePermissionStore.getState().setMode(mode))}
                  >
                    <span className="cm-gen__rich">
                      <span className="cm-gen__label">{permissionLabel(mode, t)}</span>
                      <span className="cm-gen__desc">{permissionDesc(mode, t)}</span>
                    </span>
                    {permissionMode === mode && <Check size={15} className="cm-gen__check" />}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>,
        document.body,
      )}
    </div>
  );
}

/** True for the four exposed effort levels (others fall back to the raw id). */
function EXPOSED(e: string): e is Effort {
  return e === "low" || e === "medium" || e === "high" || e === "xhigh";
}
