import { useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { formatDateTime, formatMetricValue } from "../format";
import type { View } from "../nav";
import type { ChangeEvent, Correlation } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { levelColor } from "../ui/charts";
import { Button, cx, Empty, Panel, Segmented, ServiceMark, Tag } from "../ui/primitives";
import { ChevronLeft } from "../ui/icons";

const WINDOWS = [
  { value: "30", label: "±30s" },
  { value: "120", label: "±2m" },
  { value: "300", label: "±5m" },
  { value: "900", label: "±15m" },
];

/**
 * One moment, every system. Lanes are ordered by how likely each connection is
 * to be relevant — errors first, then volume — so the thing that moved at the
 * same time is near the top rather than buried in alphabetical order.
 */
export function Correlate({
  at,
  catalog,
  onNavigate,
}: {
  at: string;
  catalog: Catalog;
  onNavigate: (view: View) => void;
}) {
  const [windowSecs, setWindowSecs] = useState("120");
  const [data, setData] = useState<Correlation | null>(null);
  const [changes, setChanges] = useState<ChangeEvent[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .correlate(at, Number(windowSecs))
      .then((result) => {
        if (!cancelled) setData(result);
      })
      .catch((e) => {
        if (!cancelled) setError(errorMessage(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [at, windowSecs]);

  // Deploys are fetched separately and clipped to the correlation window. "What
  // shipped just before this?" is usually the first question a timeline is
  // opened to answer.
  useEffect(() => {
    let cancelled = false;
    const half = Number(windowSecs) * 1000;
    const centre = new Date(at).getTime();
    api
      .getChangeEvents(undefined, 1440)
      .then((events) => {
        if (cancelled) return;
        setChanges(
          events.filter((c) => {
            const t = new Date(c.at).getTime();
            return t >= centre - half && t <= centre + half;
          }),
        );
      })
      .catch(() => {
        // A missing change store must not blank the correlation itself.
        if (!cancelled) setChanges([]);
      });
    return () => {
      cancelled = true;
    };
  }, [at, windowSecs]);

  const lanes = data?.lanes ?? [];

  return (
    <>
      <ScreenHeader
        title={
          <span className="flex items-center gap-2">
            <button
              onClick={() => onNavigate({ name: "logs" })}
              title="Back to logs"
              className="text-ink-4 transition-colors hover:text-ink"
            >
              <ChevronLeft size={16} />
            </button>
            Correlation
          </span>
        }
        meta={<span className="num">{formatDateTime(at)}</span>}
      >
        <Segmented value={windowSecs} onChange={setWindowSecs} options={WINDOWS} />
      </ScreenHeader>

      <Screen className="flex flex-col gap-3">
        {error && (
          <div
            className="rounded-lg border px-3 py-2 text-xs text-crit"
            style={{ borderColor: "color-mix(in oklab, var(--color-crit) 40%, transparent)" }}
          >
            {error}
          </div>
        )}

        {changes.length > 0 && (
          <Panel title="Deploys in this window" subtitle={`${changes.length}`} flush>
            <ul className="divide-y divide-line">
              {changes.map((change) => (
                <li key={change.id} className="flex items-center gap-2 px-3 py-1.5">
                  <span
                    className="h-1.5 w-1.5 shrink-0 rotate-45"
                    style={{ background: "var(--color-accent)" }}
                    aria-hidden
                  />
                  <ServiceMark descriptor={catalog.get(change.kind)} size={14} />
                  <span className="shrink-0 text-xs text-ink">{change.component}</span>
                  {change.from ? (
                    <span className="flex min-w-0 items-baseline gap-1 text-2xs">
                      <span className="num truncate text-ink-4">{change.from}</span>
                      <span className="shrink-0 text-ink-4">→</span>
                      <span className="num truncate text-ink-2">{change.to}</span>
                    </span>
                  ) : (
                    <span className="num truncate text-2xs text-ink-3">{change.to}</span>
                  )}
                  <span className="num ml-auto shrink-0 text-2xs text-ink-4">
                    {new Date(change.at).toLocaleTimeString([], { hour12: false })}
                  </span>
                  <button
                    onClick={() => onNavigate({ name: "service", profileId: change.profileId })}
                    className="shrink-0 text-2xs text-ink-4 transition-colors hover:text-accent"
                  >
                    {change.profileName}
                  </button>
                </li>
              ))}
            </ul>
          </Panel>
        )}

        {data && data.alerts.length > 0 && (
          <Panel title="Alerts in this window" flush>
            <ul className="divide-y divide-line">
              {data.alerts.map((alert) => (
                <li key={alert.id} className="flex items-center gap-2 px-3 py-1.5">
                  <span
                    className="h-1.5 w-1.5 shrink-0 rounded-full"
                    style={{
                      background:
                        alert.severity === "critical"
                          ? "var(--color-crit)"
                          : alert.severity === "warning"
                            ? "var(--color-warn)"
                            : "var(--color-ink-3)",
                    }}
                    aria-hidden
                  />
                  <span className="text-xs text-ink">{alert.title}</span>
                  <span className="truncate text-2xs text-ink-4">{alert.detail}</span>
                  <button
                    onClick={() => onNavigate({ name: "service", profileId: alert.profileId })}
                    className="num ml-auto shrink-0 text-2xs text-ink-4 transition-colors hover:text-accent"
                  >
                    {alert.profileName}
                  </button>
                </li>
              ))}
            </ul>
          </Panel>
        )}

        {loading && !data ? (
          <Panel>
            <Empty title="Correlating…" />
          </Panel>
        ) : lanes.length === 0 ? (
          <Panel>
            <Empty
              title="Nothing recorded in this window"
              hint="Widen the window, or pick a moment closer to when something happened."
            />
          </Panel>
        ) : (
          <div className="flex flex-col gap-3">
            {lanes.map((lane) => (
              <Panel
                key={lane.profileId}
                title={
                  <span className="flex items-center gap-2">
                    <ServiceMark descriptor={catalog.get(lane.kind)} size={16} />
                    {lane.profileName}
                  </span>
                }
                subtitle={`${lane.total} line${lane.total === 1 ? "" : "s"}`}
                actions={
                  <>
                    {lane.errorCount > 0 && (
                      <Tag color="var(--color-crit)">{lane.errorCount} errors</Tag>
                    )}
                    <Button
                      variant="ghost"
                      onClick={() => onNavigate({ name: "service", profileId: lane.profileId })}
                    >
                      Open
                    </Button>
                  </>
                }
                flush
              >
                {lane.movements.length > 0 && (
                  <div className="flex flex-wrap gap-x-5 gap-y-1.5 border-b border-line px-3 py-2">
                    {lane.movements.map((movement) => {
                      const rising = movement.changePct > 0;
                      const notable = Math.abs(movement.changePct) >= 10;
                      return (
                        <div key={movement.key} className="min-w-0">
                          <div className="flex items-baseline gap-1.5">
                            <span className="num text-xs text-ink-4">
                              {formatMetricValue(movement.from, movement.unit)}
                            </span>
                            <span className="text-2xs text-ink-4">→</span>
                            <span className="num text-xs text-ink">
                              {formatMetricValue(movement.to, movement.unit)}
                            </span>
                            <span
                              className={cx("num text-2xs", notable ? "text-warn" : "text-ink-4")}
                            >
                              {rising ? "+" : ""}
                              {movement.changePct.toFixed(1)}%
                            </span>
                          </div>
                          <div className="label mt-0.5 truncate">{movement.label}</div>
                        </div>
                      );
                    })}
                  </div>
                )}

                {lane.events.length === 0 ? (
                  <p className="px-3 py-2 text-2xs text-ink-4">
                    No log lines — the metric movement above is the only signal here.
                  </p>
                ) : (
                  <table className="w-full border-collapse">
                    <tbody>
                      {lane.events.slice(0, 40).map((event) => (
                        <tr key={event.id} className="border-b border-line/40 last:border-0 align-baseline">
                          <td className="num w-px px-3 py-1 text-2xs whitespace-nowrap text-ink-4">
                            {new Date(event.timestamp).toLocaleTimeString([], { hour12: false })}
                          </td>
                          <td className="w-px px-1 py-1">
                            <span
                              className="num text-2xs"
                              style={{ color: levelColor(event.level) }}
                            >
                              {event.level.toUpperCase()}
                            </span>
                          </td>
                          <td className="w-px max-w-32 px-2 py-1">
                            <span
                              className="num block truncate text-2xs text-ink-4"
                              title={event.source}
                            >
                              {event.source}
                            </span>
                          </td>
                          <td className="num px-2 py-1 break-all text-ink-2">{event.message}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
              </Panel>
            ))}
          </div>
        )}
      </Screen>
    </>
  );
}
