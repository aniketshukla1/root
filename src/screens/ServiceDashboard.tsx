// The service depth page: one canvas, one time domain.
//
// Everything here reads from a single `domain`, so the metric charts, the log
// rate, and the crosshair all describe the same instant — you can read straight
// down the page at a fixed moment, which is the whole point of a dashboard and
// the thing a stack of independently-scaled cards cannot do.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "../events";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { healthColor, isProblem, markColor } from "../catalog";
import { formatDateTime, formatMetricValue, formatRelative, isBreaching } from "../format";
import type { Entity, ServiceView } from "../model";
import type { View } from "../nav";
import { clampMinutes, describeSpan, rangesFor, spanOf } from "../ranges";
import type {
  Alert,
  ChangeEvent,
  HistogramBucket,
  LogEvent,
  MetricSeries,
} from "../types";
import { GLOBAL_STREAMS } from "../types";
import { AlertList } from "../ui/AlertList";
import {
  LogHistogram,
  levelColor,
  Sparkline,
  TimeSeries,
  type Annotation,
  type Domain,
} from "../ui/charts";
import { Horizon, Rate } from "../ui/capacity";
import { Bullet } from "../ui/plots";
import {
  Button,
  cx,
  Dot,
  Empty,
  Segmented,
  Tag,
} from "../ui/primitives";
import { ChevronDown, ChevronRight, RefreshIcon, TimelineIcon } from "../ui/icons";

const HISTOGRAM_BUCKETS = 72;
const AUTO_REFRESH_MS = 15000;

type SectionKey = "signals" | "series" | "logs" | "changes" | "components";

const DEFAULT_OPEN: Record<SectionKey, boolean> = {
  signals: true,
  series: true,
  logs: true,
  changes: true,
  components: true,
};

export function ServiceDashboard({
  service,
  catalog,
  logs,
  onNavigate,
  onAcknowledge,
}: {
  service: ServiceView;
  catalog: Catalog;
  logs: LogEvent[];
  onNavigate: (view: View) => void;
  onAcknowledge: (alertId: string) => void;
}) {
  const profileId = service.snapshot.profileId;
  const pollSecs = service.profile?.pollIntervalSecs ?? 15;

  const ranges = useMemo(() => rangesFor(pollSecs), [pollSecs]);
  const [minutes, setMinutes] = useState(() => clampMinutes(60, pollSecs));

  const [history, setHistory] = useState<MetricSeries[]>([]);
  const [buckets, setBuckets] = useState<HistogramBucket[]>([]);
  const [changes, setChanges] = useState<ChangeEvent[]>([]);
  const [showDeploys, setShowDeploys] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [auto, setAuto] = useState(true);

  // Fixed at fetch time rather than derived from `Date.now()` on every render,
  // so the axis does not creep out from under the cursor mid-hover.
  const [domain, setDomain] = useState<Domain>(() => {
    const now = Date.now();
    return [now - minutes * 60000, now];
  });
  const [zoom, setZoom] = useState<Domain | null>(null);
  const [hoverTime, setHoverTime] = useState<number | null>(null);

  const [open, setOpen] = useState<Record<SectionKey, boolean>>(() => {
    try {
      const stored = localStorage.getItem("root-dashboard-sections");
      return stored ? { ...DEFAULT_OPEN, ...JSON.parse(stored) } : DEFAULT_OPEN;
    } catch {
      return DEFAULT_OPEN;
    }
  });

  const toggleSection = (key: SectionKey) =>
    setOpen((current) => {
      const next = { ...current, [key]: !current[key] };
      try {
        localStorage.setItem("root-dashboard-sections", JSON.stringify(next));
      } catch {
        // A session with no storage still gets working sections.
      }
      return next;
    });

  const load = useCallback(async () => {
    try {
      const [series, histogram, changeEvents] = await Promise.all([
        api.getMetricHistory(profileId, minutes),
        api.logHistogram(
          { profileIds: [profileId], streams: GLOBAL_STREAMS },
          minutes,
          HISTOGRAM_BUCKETS,
        ),
        api.getChangeEvents(profileId, minutes),
      ]);
      setHistory(series);
      setBuckets(histogram);
      setChanges(changeEvents);
      const now = Date.now();
      setDomain([now - minutes * 60000, now]);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, [profileId, minutes]);

  useEffect(() => {
    setLoading(true);
    setZoom(null);
    load();
  }, [load]);

  const savedLoad = useRef(load);
  savedLoad.current = load;
  useEffect(() => {
    if (!auto) return;
    const timer = setInterval(() => savedLoad.current(), AUTO_REFRESH_MS);
    return () => clearInterval(timer);
  }, [auto]);

  // A deploy is worth showing the moment it is detected rather than on the next
  // refresh tick — it is the marker you are watching for when you ship.
  useEffect(() => {
    const subscription = listen<ChangeEvent[]>("monitor://changes", (event) => {
      const mine = event.payload.filter((c) => c.profileId === profileId);
      if (mine.length === 0) return;
      setChanges((current) => {
        const seen = new Set(current.map((c) => c.id));
        return [...mine.filter((c) => !seen.has(c.id)), ...current];
      });
    });
    return () => {
      subscription.then((unlisten) => unlisten());
    };
  }, [profileId]);

  const view: Domain = zoom ?? domain;
  const span = useMemo(() => spanOf(history), [history]);

  /**
   * Markers on the time axis. Two sources, deliberately different colours:
   * alerts wear their status hue because they mean something is wrong, while
   * deploys wear the interaction accent because they are neutral facts. Reading
   * "the error rate rose right after that deploy" is the entire point.
   */
  const annotations = useMemo<Annotation[]>(() => {
    const fromAlerts = service.alerts.map((alert: Alert) => ({
      id: `alert:${alert.id}`,
      t: new Date(alert.firstSeen).getTime(),
      color:
        alert.severity === "critical"
          ? "var(--color-crit)"
          : alert.severity === "warning"
            ? "var(--color-warn)"
            : "var(--color-ink-3)",
      label: alert.title,
      detail: alert.detail,
    }));

    if (!showDeploys) return fromAlerts;

    const fromChanges = changes.map((change) => ({
      id: `change:${change.id}`,
      t: new Date(change.at).getTime(),
      color: "var(--color-accent)",
      label: change.change === "deployed" ? `Deployed ${change.component}` : `New ${change.component}`,
      detail: change.summary,
    }));

    return [...fromAlerts, ...fromChanges];
  }, [service.alerts, changes, showDeploys]);

  const deploysInView = useMemo(
    () =>
      changes
        .filter((change) => {
          const t = new Date(change.at).getTime();
          return t >= view[0] && t <= view[1];
        })
        // Newest first. The store returns them ordered, but live events are
        // prepended and ingested ones can arrive with a backdated `at`, so the
        // order has to be re-established rather than assumed.
        .sort((a, b) => new Date(b.at).getTime() - new Date(a.at).getTime()),
    [changes, view],
  );

  const plotted = useMemo(() => history.filter((s) => s.points.length > 1), [history]);
  const accent = markColor(catalog.get(service.snapshot.kind).accent);

  const errorLines = useMemo(
    () =>
      logs
        .filter((event) => {
          if (event.profileId !== profileId) return false;
          if (event.level !== "error" && event.level !== "fatal") return false;
          const t = new Date(event.timestamp).getTime();
          return t >= view[0] && t <= view[1];
        })
        .slice(0, 40),
    [logs, profileId, view],
  );

  const failing = useMemo(
    () =>
      service.entities
        .filter((e) => isProblem(e.health))
        .sort((a, b) => (a.health === "down" ? -1 : 1) - (b.health === "down" ? -1 : 1)),
    [service.entities],
  );

  return (
    <div className="flex flex-col gap-3">
      {/* Sticky so the range control stays reachable however far you scroll. */}
      <div className="sticky top-0 z-[2] -mx-3 -mt-3 flex flex-wrap items-center gap-2 border-b border-line bg-panel/95 px-3 py-2 backdrop-blur">
        <Segmented
          value={String(minutes)}
          onChange={(v) => setMinutes(Number(v))}
          options={ranges.map((r) => ({ value: String(r.minutes), label: r.label }))}
        />

        {/* The ring holds a fixed sample count, not a duration — so state what
            is actually on screen rather than implying the window was filled. */}
        <span className="text-2xs text-ink-4" title={`${span.points} samples per metric`}>
          {span.points > 0 ? (
            <>
              <span className="num">{span.points}</span> samples ·{" "}
              <span className="num">{describeSpan(span)}</span> of history
              {span.saturated && " · ring full"}
            </>
          ) : (
            "no history yet"
          )}
        </span>

        {zoom && (
          <Button variant="ghost" onClick={() => setZoom(null)}>
            Reset zoom
          </Button>
        )}

        <div className="ml-auto flex items-center gap-2">
          {hoverTime !== null && (
            <button
              onClick={() =>
                onNavigate({ name: "correlate", at: new Date(hoverTime).toISOString() })
              }
              className="num flex items-center gap-1 text-2xs text-ink-3 transition-colors hover:text-accent"
              title="Correlate this moment across every connection"
            >
              <TimelineIcon size={11} />
              {formatDateTime(new Date(hoverTime).toISOString())}
            </button>
          )}
          <label
            className="flex cursor-pointer items-center gap-1.5 text-xs text-ink-3"
            title="Draw a marker on every chart where a component's version changed"
          >
            <input
              type="checkbox"
              checked={showDeploys}
              onChange={(e) => setShowDeploys(e.target.checked)}
              className="accent-accent"
            />
            <span className="flex items-center gap-1">
              <span
                className="inline-block h-1.5 w-1.5 rotate-45"
                style={{ background: "var(--color-accent)" }}
                aria-hidden
              />
              Deploys
              {deploysInView.length > 0 && (
                <span className="num text-ink-4">{deploysInView.length}</span>
              )}
            </span>
          </label>
          <label className="flex cursor-pointer items-center gap-1.5 text-xs text-ink-3">
            <input
              type="checkbox"
              checked={auto}
              onChange={(e) => setAuto(e.target.checked)}
              className="accent-accent"
            />
            Auto
          </label>
          <Button variant="ghost" onClick={load} title="Refresh now" className="px-1.5">
            <RefreshIcon size={14} />
          </Button>
        </div>
      </div>

      {error && (
        <div
          className="rounded-lg border px-3 py-2 text-xs"
          style={{ borderColor: "color-mix(in oklab, var(--color-crit) 40%, transparent)" }}
        >
          <span className="font-medium text-crit">Could not load history.</span>{" "}
          <span className="num text-ink-2">{error}</span>
        </div>
      )}

      <Section
        title="Signals"
        subtitle={`${service.snapshot.metrics.length} reported`}
        open={open.signals}
        onToggle={() => toggleSection("signals")}
      >
        {service.snapshot.metrics.length === 0 ? (
          <Empty
            title="No service-level metrics"
            hint="This collector reports inventory only."
          />
        ) : (
          <div className="grid gap-2 [grid-template-columns:repeat(auto-fill,minmax(196px,1fr))]">
            {service.snapshot.metrics.map((metric) => {
              const series = history.find((s) => s.key === metric.key);
              const over = isBreaching(metric);
              return (
                <div
                  key={metric.key}
                  className={cx(
                    "rounded-md border bg-page p-2.5",
                    over ? "border-warn/40" : "border-line",
                  )}
                >
                  <div className="label truncate" title={metric.label}>
                    {metric.label}
                  </div>
                  <div className="mt-1 flex items-end gap-2">
                    <span
                      className={cx("num text-[21px] leading-none", over ? "text-warn" : "text-ink")}
                    >
                      {formatMetricValue(metric.value, metric.unit)}
                    </span>
                    {series?.trend && (
                      <span className="num flex gap-1.5 pb-0.5 text-2xs">
                        <Rate trend={series.trend} unit={metric.unit} />
                        {metric.warnAbove !== undefined && (
                          <Horizon trend={series.trend} unit={metric.unit} />
                        )}
                      </span>
                    )}
                    <span className="ml-auto">
                      {series && (
                        <Sparkline
                          points={series.points}
                          width={62}
                          height={20}
                          color={accent}
                          warnAbove={series.warnAbove}
                        />
                      )}
                    </span>
                  </div>
                  {metric.warnAbove !== undefined && (
                    <div className="mt-2">
                      {/* The threshold is drawn, not just stated: a number
                          without its ceiling is a rumour. A percentage has a
                          real ceiling at 100; everything else only has the
                          threshold, so the track is scaled to put that tick at
                          three quarters and a breach visibly runs past it. */}
                      <Bullet
                        value={metric.value}
                        max={
                          metric.unit === "percent"
                            ? 100
                            : Math.max(metric.value, metric.warnAbove / 0.75)
                        }
                        warnAbove={metric.warnAbove}
                        height={5}
                      />
                      <div className="mt-1.5 flex items-baseline gap-1 text-2xs text-ink-4">
                        <span>limit</span>
                        <span className={cx("num", over && "text-warn")}>
                          {formatMetricValue(metric.warnAbove, metric.unit)}
                        </span>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </Section>

      <Section
        title="Time series"
        subtitle={[
          `${plotted.length} metrics`,
          service.alerts.length > 0 && `${service.alerts.length} alerts`,
          showDeploys && deploysInView.length > 0 && `${deploysInView.length} deploys`,
        ]
          .filter(Boolean)
          .join(" · ")}
        open={open.series}
        onToggle={() => toggleSection("series")}
        hint="Drag across a chart to zoom. Hover moves the crosshair on every panel."
      >
        {loading && plotted.length === 0 ? (
          <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(340px,1fr))]">
            {Array.from({ length: 2 }).map((_, i) => (
              <div key={i} className="h-40 animate-pulse rounded-md border border-line opacity-40" />
            ))}
          </div>
        ) : plotted.length === 0 ? (
          <Empty
            title="No history yet"
            hint="Series build as this connection polls. Give it a couple of collection cycles."
          />
        ) : (
          <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(340px,1fr))]">
            {plotted.map((series) => {
              const over = series.warnAbove !== undefined && series.latest > series.warnAbove;
              return (
                <div key={series.key} className="rounded-md border border-line bg-page p-2.5">
                  <div className="flex items-baseline gap-2">
                    <span className="min-w-0 truncate text-xs text-ink-3" title={series.label}>
                      {series.label}
                    </span>
                    <span className="num ml-auto shrink-0 text-2xs text-ink-4">
                      {formatMetricValue(series.min, series.unit)} –{" "}
                      {formatMetricValue(series.max, series.unit)}
                    </span>
                    <span
                      className={cx("num shrink-0 text-sm", over ? "text-warn" : "text-ink")}
                    >
                      {formatMetricValue(series.latest, series.unit)}
                    </span>
                  </div>

                  {/* One series per chart, so no legend box — the label names it. */}
                  <TimeSeries
                    series={[
                      {
                        key: series.key,
                        label: series.label,
                        color: accent,
                        points: series.points,
                        warnAbove: series.warnAbove,
                      },
                    ]}
                    unit={series.unit}
                    domain={view}
                    height={132}
                    showLegend={false}
                    annotations={annotations}
                    hoverTime={hoverTime}
                    onHoverTime={setHoverTime}
                    onBrush={setZoom}
                  />
                </div>
              );
            })}
          </div>
        )}
      </Section>

      <Section
        title="Logs"
        subtitle={`${errorLines.length} at error or above in view`}
        open={open.logs}
        onToggle={() => toggleSection("logs")}
        action={
          <Button
            variant="ghost"
            onClick={() => onNavigate({ name: "service", profileId, tab: "logs" })}
          >
            Full stream
          </Button>
        }
      >
        <div className="rounded-md border border-line bg-page p-2.5">
          <div className="label mb-1">Rate</div>
          {/* Same domain as the charts above, so a spike here sits directly
              under the metric that moved. */}
          <LogHistogram
            buckets={buckets}
            height={72}
            domain={view}
            hoverTime={hoverTime}
            onHoverTime={setHoverTime}
            annotations={annotations}
          />
          <div className="mt-1.5 flex flex-wrap gap-x-3 gap-y-1">
            {(["fatal", "error", "warn", "info", "debug", "trace"] as const).map((level) => (
              <span
                key={level}
                className="flex items-center gap-1 text-2xs text-ink-4 capitalize"
              >
                <span
                  className="h-1.5 w-1.5 shrink-0 rounded-full"
                  style={{ background: levelColor(level) }}
                />
                {level}
              </span>
            ))}
          </div>
        </div>

        <div className="mt-2 overflow-hidden rounded-md border border-line">
          {errorLines.length === 0 ? (
            <Empty
              title="No errors in this window"
              hint="Nothing at error or fatal from this connection in the selected range."
            />
          ) : (
            <table className="w-full border-collapse">
              <tbody>
                {errorLines.map((event) => (
                  <tr
                    key={event.id}
                    className="group border-b border-line/40 align-baseline last:border-0 hover:bg-white/[0.035]"
                  >
                    <td className="w-px px-2.5 py-1 whitespace-nowrap">
                      <button
                        onClick={() => onNavigate({ name: "correlate", at: event.timestamp })}
                        title={`${formatDateTime(event.timestamp)} — correlate across every connection`}
                        className="num flex items-center gap-1 text-2xs text-ink-4 transition-colors hover:text-accent"
                      >
                        {new Date(event.timestamp).toLocaleTimeString([], { hour12: false })}
                        <TimelineIcon
                          size={11}
                          className="opacity-0 transition-opacity group-hover:opacity-100"
                        />
                      </button>
                    </td>
                    <td className="w-px px-1 py-1">
                      <span className="num text-2xs" style={{ color: levelColor(event.level) }}>
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
        </div>
      </Section>

      <Section
        title="Deploys"
        subtitle={
          deploysInView.length > 0
            ? `${deploysInView.length} in this window`
            : "nothing shipped in this window"
        }
        open={open.changes}
        onToggle={() => toggleSection("changes")}
        hint="Derived by diffing each component's version between polls."
      >
        {deploysInView.length === 0 ? (
          <Empty
            title="No changes in this window"
            hint={
              changes.length > 0
                ? `${changes.length} recorded outside the selected range — widen it to see them.`
                : "A marker appears here when a container image, build number, or other version identifier moves."
            }
          />
        ) : (
          <div className="overflow-hidden rounded-md border border-line">
            <table className="w-full border-collapse">
              <thead>
                <tr className="border-b border-line">
                  <th className="label px-2.5 py-1.5 text-left font-semibold">When</th>
                  <th className="label px-2.5 py-1.5 text-left font-semibold">Component</th>
                  <th className="label px-2.5 py-1.5 text-left font-semibold">Change</th>
                  <th className="label hidden px-2.5 py-1.5 text-left font-semibold lg:table-cell">
                    Section
                  </th>
                </tr>
              </thead>
              <tbody>
                {deploysInView.map((change) => (
                  <tr
                    key={change.id}
                    className="group border-b border-line/40 last:border-0 hover:bg-white/[0.035]"
                    style={{ height: "var(--row-h)" }}
                  >
                    <td className="w-px px-2.5 whitespace-nowrap">
                      <button
                        onClick={() => onNavigate({ name: "correlate", at: change.at })}
                        title={`${formatDateTime(change.at)} — correlate across every connection`}
                        className="num flex items-center gap-1 text-2xs text-ink-4 transition-colors hover:text-accent"
                      >
                        {formatRelative(change.at)}
                        <TimelineIcon
                          size={11}
                          className="opacity-0 transition-opacity group-hover:opacity-100"
                        />
                      </button>
                    </td>
                    <td className="max-w-52 px-2.5">
                      <span className="flex min-w-0 items-center gap-1.5">
                        <span
                          className="h-1.5 w-1.5 shrink-0 rotate-45"
                          style={{ background: "var(--color-accent)" }}
                          aria-hidden
                        />
                        <span className="truncate text-xs font-medium text-ink">
                          {change.component}
                        </span>
                        {/* An observed change is one root watched happen; a
                            reported one is an external assertion, which can be
                            wrong. Saying so is cheap and the distinction
                            matters when you are deciding what to trust. */}
                        {change.source === "reported" && (
                          <Tag title="Posted to the local ingest endpoint rather than observed by a collector">
                            reported
                          </Tag>
                        )}
                      </span>
                    </td>
                    <td className="px-2.5">
                      {change.from ? (
                        <span className="flex min-w-0 items-baseline gap-1.5 text-2xs">
                          <span className="num truncate text-ink-4">{change.from}</span>
                          <span className="shrink-0 text-ink-4">→</span>
                          <span className="num truncate text-ink-2">{change.to}</span>
                        </span>
                      ) : (
                        <span className="text-2xs text-ink-3">
                          appeared at <span className="num">{change.to}</span>
                        </span>
                      )}
                    </td>
                    <td className="hidden px-2.5 lg:table-cell">
                      <Tag>{change.section}</Tag>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Section>

      <Section
        title="Components"
        subtitle={`${failing.length} of ${service.entities.length} need attention`}
        open={open.components}
        onToggle={() => toggleSection("components")}
        action={
          <Button
            variant="ghost"
            onClick={() => onNavigate({ name: "service", profileId, tab: "components" })}
          >
            Full inventory
          </Button>
        }
      >
        <div className="grid gap-3 lg:grid-cols-2">
          <div className="overflow-hidden rounded-md border border-line">
            <div className="label border-b border-line px-2.5 py-1.5">Needs attention</div>
            {failing.length === 0 ? (
              <Empty title="All components healthy" />
            ) : (
              <ul className="divide-y divide-line/60">
                {failing.slice(0, 10).map((entity: Entity) => {
                  // `Entity.id` is "profile:section:row"; the drill-in needs
                  // the two halves back.
                  const [, entitySection, ...rest] = entity.id.split(":");
                  const entityRow = rest.join(":");
                  return (
                    <li
                      key={entity.id}
                      className="row-hover flex items-baseline gap-2 px-2.5 py-1.5"
                    >
                      <span className="translate-y-px" title={entity.health}>
                        <Dot health={entity.health} size={7} />
                      </span>
                      <button
                        onClick={() =>
                          onNavigate({
                            name: "component",
                            profileId,
                            sectionId: entitySection,
                            rowId: entityRow,
                          })
                        }
                        disabled={!entityRow}
                        title={entityRow ? `Open ${entity.name}` : undefined}
                        className="shrink-0 text-xs font-medium text-ink transition-colors hover:text-accent disabled:hover:text-ink"
                      >
                        {entity.name}
                      </button>
                      <Tag>{entity.section}</Tag>
                      <span
                        className="min-w-0 truncate text-2xs text-ink-4"
                        title={entity.summary}
                      >
                        {entity.summary}
                      </span>
                    </li>
                  );
                })}
                {failing.length > 10 && (
                  <li className="px-2.5 py-1.5 text-2xs text-ink-4">
                    and {failing.length - 10} more.
                  </li>
                )}
              </ul>
            )}
          </div>

          <div className="overflow-hidden rounded-md border border-line">
            <div className="label border-b border-line px-2.5 py-1.5">Active alerts</div>
            <AlertList
              alerts={service.alerts}
              catalog={catalog}
              onAcknowledge={onAcknowledge}
              showConnection={false}
              emptyTitle="No active alerts"
              emptyHint="No threshold on this connection is currently breached."
              limit={6}
            />
          </div>
        </div>
      </Section>
    </div>
  );
}

// ------------------------------------------------------------------ section

function Section({
  title,
  subtitle,
  hint,
  action,
  open,
  onToggle,
  children,
}: {
  title: string;
  subtitle?: string;
  hint?: string;
  action?: React.ReactNode;
  open: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}) {
  return (
    <section className="panel overflow-hidden">
      <header className="flex h-9 items-center gap-2 border-b border-line px-3">
        <button
          onClick={onToggle}
          className="flex min-w-0 items-center gap-1.5 text-ink transition-colors hover:text-accent"
          aria-expanded={open}
        >
          {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
          <h2 className="text-[13px] font-semibold">{title}</h2>
        </button>
        {subtitle && <span className="truncate text-xs text-ink-4">{subtitle}</span>}
        {hint && open && (
          <span className="hidden truncate text-2xs text-ink-4/70 xl:inline">{hint}</span>
        )}
        {action && <div className="ml-auto">{action}</div>}
      </header>
      {open && <div className="p-3">{children}</div>}
    </section>
  );
}

export { healthColor, formatRelative };
