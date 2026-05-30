import { AlertCircle, CheckCircle2, Info, X } from "lucide-react";
import { useToastStore, type ToastKind } from "../store/toast";

const ICON: Record<ToastKind, typeof Info> = {
  info: Info,
  success: CheckCircle2,
  error: AlertCircle,
};

/** Renders the in-app toast stack. Mounted once at the app root; reads from the
 *  toast store. See store/toast.ts. */
export function ToastHost() {
  const toasts = useToastStore((s) => s.toasts);
  const dismiss = useToastStore((s) => s.dismiss);

  if (toasts.length === 0) return null;

  return (
    <div className="cm-toasts" role="status" aria-live="polite">
      {toasts.map((toast) => {
        const Icon = ICON[toast.kind];
        return (
          <div key={toast.id} className={`cm-toast cm-toast--${toast.kind}`}>
            <Icon size={15} className="cm-toast__icon" />
            <span className="cm-toast__msg">{toast.message}</span>
            <button
              type="button"
              className="cm-toast__close"
              onClick={() => dismiss(toast.id)}
              aria-label="Dismiss"
            >
              <X size={13} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
