import { useEffect } from "react";

import type { Alert } from "../types";
import { cx } from "../ui/primitives";
import { CloseIcon } from "../ui/icons";

export interface Toast {
  id: string;
  severity: Alert["severity"];
  title: string;
  detail: string;
  onOpen?: () => void;
}

export function toastFromAlert(alert: Alert, onOpen: () => void): Toast {
  return {
    id: alert.id,
    severity: alert.severity,
    title: alert.title,
    detail: `${alert.profileName} — ${alert.detail}`,
    onOpen,
  };
}

const TONE: Record<Alert["severity"], string> = {
  critical: "var(--color-crit)",
  warning: "var(--color-warn)",
  info: "var(--color-ink-3)",
};

export function Toasts({
  toasts,
  onDismiss,
}: {
  toasts: Toast[];
  onDismiss: (id: string) => void;
}) {
  if (toasts.length === 0) return null;

  return (
    <div className="pointer-events-none fixed right-4 bottom-4 z-40 flex w-80 flex-col gap-2">
      {toasts.slice(-4).map((toast) => (
        <ToastCard key={toast.id} toast={toast} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

function ToastCard({ toast, onDismiss }: { toast: Toast; onDismiss: (id: string) => void }) {
  // Critical alerts stay until dismissed; anything lower clears itself so a
  // burst of warnings cannot bury the screen.
  useEffect(() => {
    if (toast.severity === "critical") return;
    const timer = setTimeout(() => onDismiss(toast.id), 8000);
    return () => clearTimeout(timer);
  }, [toast.id, toast.severity, onDismiss]);

  const tone = TONE[toast.severity];

  return (
    <div
      className="panel pointer-events-auto flex gap-2.5 border-l-2 bg-overlay p-2.5 shadow-lg shadow-black/40"
      style={{ borderLeftColor: tone }}
      role="status"
    >
      <div className="min-w-0 flex-1">
        <button
          onClick={toast.onOpen}
          disabled={!toast.onOpen}
          className={cx(
            "block w-full truncate text-left text-xs font-medium text-ink",
            toast.onOpen && "transition-colors hover:text-accent",
          )}
        >
          {toast.title}
        </button>
        <p className="mt-0.5 line-clamp-2 text-2xs text-ink-3">{toast.detail}</p>
      </div>
      <button
        onClick={() => onDismiss(toast.id)}
        className="shrink-0 self-start text-ink-4 transition-colors hover:text-ink"
        title="Dismiss"
      >
        <CloseIcon size={13} />
      </button>
    </div>
  );
}
