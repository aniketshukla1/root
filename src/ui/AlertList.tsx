// Compact alert list, shared by the Overview panel and a connection's Alerts
// tab. The Alerts screen keeps its own table — it carries more columns and its
// own filters — but everywhere an alert appears in a *panel*, it appears here,
// so severity, acknowledgement, and age read identically across the app.

import type { Catalog } from "../catalog";
import { formatDateTime, formatRelative } from "../format";
import type { Alert, AlertSeverity } from "../types";
import { Button, Empty, Tag } from "./primitives";

export const SEVERITY: Record<AlertSeverity, { color: string; label: string; rank: number }> = {
  critical: { color: "var(--color-crit)", label: "Critical", rank: 0 },
  warning: { color: "var(--color-warn)", label: "Warning", rank: 1 },
  info: { color: "var(--color-ink-3)", label: "Info", rank: 2 },
};

/** Unacknowledged first, then severity, then oldest — the response order. */
export function compareAlerts(a: Alert, b: Alert): number {
  return (
    Number(a.acknowledged) - Number(b.acknowledged) ||
    SEVERITY[a.severity].rank - SEVERITY[b.severity].rank ||
    new Date(a.firstSeen).getTime() - new Date(b.firstSeen).getTime()
  );
}

export function AlertList({
  alerts,
  catalog,
  onAcknowledge,
  onOpenService,
  showConnection = true,
  emptyTitle = "No active alerts",
  emptyHint = "Every collector is reporting within threshold.",
  limit,
}: {
  alerts: Alert[];
  catalog: Catalog;
  onAcknowledge: (alertId: string) => void;
  onOpenService?: (profileId: string) => void;
  showConnection?: boolean;
  emptyTitle?: string;
  emptyHint?: string;
  limit?: number;
}) {
  if (alerts.length === 0) {
    return <Empty title={emptyTitle} hint={emptyHint} />;
  }

  const ordered = [...alerts].sort(compareAlerts);
  const shown = limit ? ordered.slice(0, limit) : ordered;

  return (
    <ul className="divide-y divide-line">
      {shown.map((alert) => {
        const severity = SEVERITY[alert.severity];
        return (
          <li key={alert.id} className="row-hover group flex items-start gap-2.5 px-3 py-2">
            <span
              className="mt-1 h-1.5 w-1.5 shrink-0 rounded-full"
              style={{ background: severity.color }}
              aria-hidden
            />
            <div className="min-w-0 flex-1">
              <div className="flex min-w-0 items-center gap-1.5">
                <span className="truncate text-[13px] text-ink">{alert.title}</span>
                {alert.acknowledged && alert.active && <Tag>Acked</Tag>}
                {!alert.active && <Tag color="var(--color-ok)">Resolved</Tag>}
              </div>
              <p className="line-clamp-1 text-xs text-ink-3" title={alert.detail}>
                {alert.detail}
              </p>
              <div className="mt-0.5 flex items-center gap-1.5 text-2xs text-ink-4">
                <span style={{ color: severity.color }}>{severity.label}</span>
                {showConnection && (
                  <>
                    <span className="text-ink-4/50">·</span>
                    {onOpenService ? (
                      <button
                        onClick={() => onOpenService(alert.profileId)}
                        className="truncate transition-colors hover:text-accent"
                      >
                        {alert.profileName}
                      </button>
                    ) : (
                      <span className="truncate">{alert.profileName}</span>
                    )}
                    <span className="text-ink-4/50">·</span>
                    <span className="shrink-0">{catalog.label(alert.kind)}</span>
                  </>
                )}
                {alert.component && (
                  <>
                    <span className="text-ink-4/50">·</span>
                    <span className="num truncate">{alert.component}</span>
                  </>
                )}
                <span className="text-ink-4/50">·</span>
                <span className="num shrink-0" title={formatDateTime(alert.firstSeen)}>
                  {formatRelative(alert.firstSeen)}
                </span>
              </div>
            </div>
            {alert.active && !alert.acknowledged && (
              <Button
                variant="ghost"
                onClick={() => onAcknowledge(alert.id)}
                className="opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
                title="Acknowledging silences the notification. It stays listed until the collector reports recovery."
              >
                Ack
              </Button>
            )}
          </li>
        );
      })}
    </ul>
  );
}
