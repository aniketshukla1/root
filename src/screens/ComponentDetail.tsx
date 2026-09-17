// The third level: one component inside one service.
//
// Two kinds of component reach this page and they are answered differently.
//
//   Tracked sections (containers, cluster nodes, jobs) have server-side
//   history, so this draws real charts over a chosen window.
//
//   Untracked sections (Elasticsearch indices, Kubernetes pods, database
//   active queries) are too many or too short-lived for the backend to retain
//   a series. They still get everything else — every cell, logs, alerts,
//   actions — plus a sparkline accumulated in this browser session while the
//   page is open, which is the only honest way to show a trend for something
//   that may not exist in ten seconds.
//
// The distinction is stated on screen rather than left for the reader to infer
// from a missing chart.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { DATA_INK, healthColor, isProblem } from "../catalog";
import { formatDateTime, formatMetricValue, formatRelative } from "../format";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import { clampMinutes, describeSpan, rangesFor, spanOf } from "../ranges";
import type {
  CellValue,
  ColumnSpec,
  LogEvent,
  MetricSeries,
  MetricUnit,
  Point,
  Row,
  RowAction,
  Section,
} from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { AlertList } from "../ui/AlertList";
import { levelColor, Meter, Sparkline, TimeSeries, type Domain } from "../ui/charts";
import { Horizon, Rate } from "../ui/capacity";
import {
  Button,
  cx,
  Dot,
  Empty,
  HealthBadge,
  Panel,
  Segmented,
  ServiceMark,
  Tag,
} from "../ui/primitives";
import { ChevronLeft, ChevronRight, TimelineIcon } from "../ui/icons";

/** Samples the session buffer keeps for an untracked component. */
const SESSION_POINTS = 180;

export function ComponentDetail({
  profileId,
  sectionId,
  rowId,
  model,
  catalog,
  logs,
  onNavigate,
  onAcknowledge,
  onRefresh,
}: {
  profileId: string;
  sectionId: string;
  rowId: string;
  model: EstateModel;
  catalog: Catalog;
  logs: LogEvent[];
  onNavigate: (view: View) => void;
  onAcknowledge: (alertId: string) => void;
  onRefresh: () => void;
}) {
  const service = model.services.find((s) => s.snapshot.profileId === profileId);
  const section = service?.snapshot.sections.find((s) => s.id === sectionId);
  const row = section?.rows.find((r) => r.id === rowId);

  const pollSecs = service?.profile?.pollIntervalSecs ?? 15;
  const ranges = useMemo(() => rangesFor(pollSecs), [pollSecs]);
  const [minutes, setMinutes] = useState(() => clampMinutes(60, pollSecs));

  const [history, setHistory] = useState<MetricSeries[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [domain, setDomain] = useState<Domain>(() => {
    const now = Date.now();
    return [now - minutes * 60000, now];
  });
  const [hoverTime, setHoverTime] = useState<number | null>(null);
  const [pending, setPending] = useState<RowAction | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);

  /**
   * Session-local trend for untracked components. Keyed by column so a row
   * that vanishes and returns does not splice two different lifetimes
   * together — it is cleared whenever the component identity changes.
   */
  const sessionRef = useRef<Record<string, Point[]>>({});
  const [sessionTick, setSessionTick] = useState(0);

  const tracked = section?.tracked ?? false;

  const load = useCallback(async () => {
    if (!tracked) {
      setLoading(false);
      return;
    }
    try {
      setHistory(await api.getComponentHistory(profileId, sectionId, rowId, minutes));
      const now = Date.now();
      setDomain([now - minutes * 60000, now]);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, [tracked, profileId, sectionId, rowId, minutes]);

  useEffect(() => {
    setLoading(true);
    load();
  }, [load]);

  // Reset the session buffer when the component being viewed changes.
  useEffect(() => {
    sessionRef.current = {};
    setSessionTick(0);
  }, [profileId, sectionId, rowId]);

  const numericColumns = useMemo(
    () => (section?.columns ?? []).filter((c) => unitOf(c) !== null),
    [section],
  );

  // Accumulate live values for untracked components on every snapshot the
  // parent hands down.
  const checkedAt = service?.snapshot.checkedAt;
  useEffect(() => {
    if (tracked || !row || !checkedAt) return;
    let changed = false;
    for (const column of numericColumns) {
      const value = numericValue(row.cells[column.key]);
      if (value === null) continue;
      const series = sessionRef.current[column.key] ?? [];
      if (series[series.length - 1]?.t === checkedAt) continue;
      series.push({ t: checkedAt, v: value });
      if (series.length > SESSION_POINTS) series.shift();
      sessionRef.current[column.key] = series;
      changed = true;
    }
    if (changed) setSessionTick((t) => t + 1);
  }, [tracked, row, checkedAt, numericColumns]);

  const componentLogs = useMemo(() => {
    if (!row) return [];
    const needle = row.name.toLowerCase();
    return logs
      .filter(
        (event) =>
          event.profileId === profileId &&
          (event.source.toLowerCase() === needle ||
            event.source.toLowerCase().includes(needle) ||
            event.message.toLowerCase().includes(needle)),
      )
      .slice(0, 200);
  }, [logs, profileId, row]);

  const componentAlerts = useMemo(() => {
    if (!row || !service) return [];
    const needle = row.name.toLowerCase();
    return service.alerts.filter(
      (a) => a.component?.toLowerCase() === needle || a.title.toLowerCase().includes(needle),
    );
  }, [service, row]);

  if (!service || !section || !row) {
    return (
      <>
        <ScreenHeader title="Component" />
        <Screen>
          <Panel>
            <Empty
              title="Component not found"
              hint="It may have been removed, or the connection has not reported it in its latest snapshot."
            />
            <div className="flex justify-center pb-4">
              <Button onClick={() => onNavigate({ name: "service", profileId })}>
                Back to the connection
              </Button>
            </div>
          </Panel>
        </Screen>
      </>
    );
  }

  const descriptor = catalog.get(service.snapshot.kind);
  const line = DATA_INK;
  const span = spanOf(history);
  const plotted = history.filter((s) => s.points.length > 1);

  const runAction = async () => {
    if (!pending) return;
    setBusy(true);
    try {
      const text = await api.runRowAction(profileId, pending.tool, pending.args);
      setResult({ ok: true, text });
      onRefresh();
    } catch (e) {
      setResult({ ok: false, text: errorMessage(e) });
    } finally {
      setBusy(false);
      setPending(null);
    }
  };

  return (
    <>
      <ScreenHeader
        title={
          <span className="flex min-w-0 items-center gap-1.5">
            <button
              onClick={() => onNavigate({ name: "service", profileId, tab: "components" })}
              title="Back to the connection"
              className="shrink-0 text-ink-4 transition-colors hover:text-ink"
            >
              <ChevronLeft size={16} />
            </button>
            <button
              onClick={() => onNavigate({ name: "services" })}
              className="shrink-0 text-base font-normal text-ink-4 transition-colors hover:text-accent"
            >
              Services
            </button>
            <ChevronRight size={12} className="shrink-0 text-ink-4/60" />
            <button
              onClick={() => onNavigate({ name: "service", profileId })}
              className="flex shrink-0 items-center gap-1.5 text-base font-normal text-ink-4 transition-colors hover:text-accent"
            >
              <ServiceMark descriptor={descriptor} size={15} />
              {service.snapshot.profileName}
            </button>
            <ChevronRight size={12} className="shrink-0 text-ink-4/60" />
            <span className="truncate">{row.name}</span>
          </span>
        }
        meta={
          <>
            {section.label}
            <span className="text-ink-4/50">·</span>
            <span className="num">{formatRelative(service.snapshot.checkedAt)}</span>
          </>
        }
        onRefresh={() => {
          load();
          onRefresh();
        }}
      >
        <HealthBadge health={row.health} />
      </ScreenHeader>

      <Screen className="flex flex-col gap-3">
        <div
          className="panel border-l-2 px-4 py-3"
          style={{
            borderLeftColor: isProblem(row.health) ? healthColor(row.health) : "transparent",
          }}
        >
          <p className="text-base text-ink-2">{row.summary || row.name}</p>

          {row.actions.length > 0 && (
            <div className="mt-2 flex flex-wrap gap-1.5">
              {row.actions.map((action) => (
                <Button
                  key={action.tool}
                  variant={action.destructive ? "danger" : "default"}
                  onClick={() => setPending(action)}
                  title={
                    action.destructive
                      ? `${action.label} — you will be asked to confirm`
                      : action.label
                  }
                >
                  {action.label}
                </Button>
              ))}
            </div>
          )}

          {result && (
            <div className="mt-2 flex items-center gap-2 rounded-md border border-line bg-page px-2.5 py-1.5 text-xs">
              <span className={result.ok ? "text-ok" : "text-crit"}>
                {result.ok ? "Action completed." : "Action failed."}
              </span>
              <span className="num min-w-0 flex-1 truncate text-ink-2" title={result.text}>
                {result.text}
              </span>
              <Button variant="ghost" onClick={() => setResult(null)}>
                Dismiss
              </Button>
            </div>
          )}
        </div>

        {/* Every cell the collector reported, whether chartable or not. */}
        <Panel title="Current" subtitle={`${section.columns.length} reported fields`}>
          <div className="grid gap-2 [grid-template-columns:repeat(auto-fill,minmax(180px,1fr))]">
            {section.columns.map((column) => (
              <div key={column.key} className="rounded-md border border-line bg-page p-2.5">
                <div className="label truncate" title={column.label}>
                  {column.label}
                </div>
                <div className="mt-1 text-base text-ink">
                  <CellReading column={column} value={row.cells[column.key]} health={row.health} />
                </div>
              </div>
            ))}
          </div>
        </Panel>

        {tracked ? (
          <Panel
            title="History"
            subtitle={
              span.points > 0
                ? `${span.points} samples · ${describeSpan(span)}`
                : "building…"
            }
            actions={
              <Segmented
                label="Time range"
                value={String(minutes)}
                onChange={(v) => setMinutes(Number(v))}
                options={ranges.map((r) => ({ value: String(r.minutes), label: r.label }))}
              />
            }
          >
            {error ? (
              <p className="text-xs text-crit">{error}</p>
            ) : loading && plotted.length === 0 ? (
              <div className="h-32 animate-pulse rounded-md border border-line opacity-40" />
            ) : plotted.length === 0 ? (
              <Empty
                title="No history yet"
                hint="This component was seen recently but has not been sampled enough times to plot. Give it a couple of collection cycles."
              />
            ) : (
              <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(320px,1fr))]">
                {plotted.map((series) => (
                  <ChartCard
                    key={series.key}
                    series={series}
                    color={line}
                    domain={domain}
                    hoverTime={hoverTime}
                    onHoverTime={setHoverTime}
                  />
                ))}
              </div>
            )}
          </Panel>
        ) : (
          <Panel
            title="Live"
            subtitle="this session only"
            actions={
              <Tag title="The backend does not retain a series for this kind of component">
                not retained
              </Tag>
            }
          >
            <p className="mb-3 text-xs leading-relaxed text-ink-3">
              {section.label} are not kept as history — there are too many of them, or they do
              not live long enough for a stored series to mean anything. These values update on
              every poll, and the trend below is accumulated in this window while the page stays
              open. It resets if you navigate away.
            </p>

            {numericColumns.length === 0 ? (
              <Empty
                title="No numeric fields"
                hint="This component reports no values that can be trended."
              />
            ) : (
              <div className="grid gap-2 [grid-template-columns:repeat(auto-fill,minmax(220px,1fr))]">
                {numericColumns.map((column) => {
                  const points = sessionRef.current[column.key] ?? [];
                  const value = numericValue(row.cells[column.key]);
                  const unit = unitOf(column) ?? "count";
                  return (
                    <div
                      key={`${column.key}-${sessionTick}`}
                      className="rounded-md border border-line bg-page p-2.5"
                    >
                      <div className="label truncate">{column.label}</div>
                      <div className="mt-1 flex items-end gap-2">
                        <span className="num text-xl leading-none text-ink">
                          {value === null ? "—" : formatMetricValue(value, unit)}
                        </span>
                        <span className="ml-auto">
                          <Sparkline points={points} width={78} height={22} color={line} />
                        </span>
                      </div>
                      <div className="num mt-1 text-2xs text-ink-4">
                        {points.length < 2
                          ? "waiting for a second sample"
                          : `${points.length} samples this session`}
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </Panel>
        )}

        <div className="grid gap-3 xl:grid-cols-[3fr_2fr]">
          <Panel
            title="Logs"
            subtitle={`${componentLogs.length} lines mentioning ${row.name}`}
            actions={
              <Button
                variant="ghost"
                onClick={() => onNavigate({ name: "service", profileId, tab: "logs" })}
              >
                Full stream
              </Button>
            }
            flush
            bodyClassName="max-h-96 overflow-y-auto"
          >
            {componentLogs.length === 0 ? (
              <Empty
                title="No lines for this component"
                hint="Nothing in the buffer names it. Its connection may not tail logs at all."
              />
            ) : (
              <table className="w-full border-collapse">
                <tbody>
                  {componentLogs.map((event) => (
                    <tr
                      key={event.id}
                      className="group border-b border-line/40 align-baseline last:border-0 row-hover"
                    >
                      <td className="w-px px-3 py-1 whitespace-nowrap">
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
                      <td className="num px-2 py-1 break-all text-ink-2">{event.message}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Panel>

          <Panel title="Alerts" subtitle={`${componentAlerts.length} naming this component`} flush>
            <AlertList
              alerts={componentAlerts}
              catalog={catalog}
              onAcknowledge={onAcknowledge}
              showConnection={false}
              emptyTitle="No alerts for this component"
              emptyHint="Nothing currently firing names it."
            />
          </Panel>
        </div>

        <SiblingStrip
          section={section}
          current={row}
          onOpen={(next) =>
            onNavigate({ name: "component", profileId, sectionId, rowId: next.id })
          }
        />
      </Screen>

      {pending && (
        <ConfirmRowAction
          action={pending}
          component={row.name}
          target={service.snapshot.profileName}
          busy={busy}
          onCancel={() => setPending(null)}
          onConfirm={runAction}
        />
      )}
    </>
  );
}

// -------------------------------------------------------------------- pieces

function ChartCard({
  series,
  color,
  domain,
  hoverTime,
  onHoverTime,
}: {
  series: MetricSeries;
  color: string;
  domain: Domain;
  hoverTime: number | null;
  onHoverTime: (t: number | null) => void;
}) {
  const over = series.warnAbove !== undefined && series.latest > series.warnAbove;
  return (
    <div className="rounded-md border border-line bg-page p-2.5">
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 truncate text-xs text-ink-3" title={series.label}>
          {series.label}
        </span>
        <span className="num ml-auto shrink-0 text-2xs text-ink-4">
          {formatMetricValue(series.min, series.unit)} –{" "}
          {formatMetricValue(series.max, series.unit)}
        </span>
        <span className={cx("num shrink-0 text-sm", over ? "text-warn" : "text-ink")}>
          {formatMetricValue(series.latest, series.unit)}
        </span>
      </div>
      {/* One series per chart, so no legend box — the label names it. */}
      <TimeSeries
        series={[
          {
            key: series.key,
            label: series.label,
            color,
            points: series.points,
            warnAbove: series.warnAbove,
          },
        ]}
        unit={series.unit}
        domain={domain}
        height={124}
        showLegend={false}
        hoverTime={hoverTime}
        onHoverTime={onHoverTime}
      />

      {/* Per-component is where the capacity answer is actually actionable:
          it is one export, one container, one node that fills — the
          connection-level roll-up cannot tell you which. */}
      {series.trend && (
        <div className="num mt-1 flex items-baseline gap-3 text-2xs">
          <Rate trend={series.trend} unit={series.unit} />
          {series.warnAbove !== undefined && (
            <span className="ml-auto text-ink-4">
              limit{" "}
              <span className="num text-warn">
                {formatMetricValue(series.warnAbove, series.unit)}
              </span>
              {" · "}
              <Horizon trend={series.trend} unit={series.unit} />
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/** Move between components without going back up a level. */
function SiblingStrip({
  section,
  current,
  onOpen,
}: {
  section: Section;
  current: Row;
  onOpen: (row: Row) => void;
}) {
  if (section.rows.length < 2) return null;

  return (
    <Panel title={section.label} subtitle={`${section.rows.length} in this connection`}>
      <div className="flex flex-wrap gap-1.5">
        {section.rows.map((row) => (
          <button
            key={row.id}
            onClick={() => onOpen(row)}
            className={cx(
              "flex items-center gap-1.5 rounded-md border px-2 py-1 text-xs transition-colors",
              row.id === current.id
                ? "border-accent/50 bg-accent/8 text-ink"
                : "border-line bg-page text-ink-3 hover:border-line-strong hover:text-ink",
            )}
          >
            <Dot health={row.health} size={7} />
            <span className="max-w-40 truncate">{row.name}</span>
          </button>
        ))}
      </div>
    </Panel>
  );
}

function ConfirmRowAction({
  action,
  component,
  target,
  busy,
  onCancel,
  onConfirm,
}: {
  action: RowAction;
  component: string;
  target: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/60 p-4 backdrop-blur-sm"
      onClick={onCancel}
      role="dialog"
      aria-modal
    >
      <div
        className="panel w-full max-w-md bg-overlay p-4 overlay-plate"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-sm font-semibold text-ink">{action.label}</h2>
        <p className="mt-1.5 text-xs text-ink-2">
          This runs against <span className="font-medium text-ink">{component}</span> on{" "}
          <span className="font-medium text-ink">{target}</span>.
        </p>
        {action.destructive && (
          <p className="mt-2.5 text-2xs text-warn">
            This changes the state of running infrastructure.
          </p>
        )}
        <div className="mt-4 flex justify-end gap-2">
          <Button onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant={action.destructive ? "danger" : "primary"}
            onClick={onConfirm}
            disabled={busy}
          >
            {busy ? "Running…" : action.label}
          </Button>
        </div>
      </div>
    </div>
  );
}

// --------------------------------------------------------------------- cells

function unitOf(column: ColumnSpec): MetricUnit | null {
  if (column.kind.type === "metric" || column.kind.type === "gauge") return column.kind.unit;
  return null;
}

function numericValue(value: CellValue | undefined): number | null {
  if (!value) return null;
  if (value.type === "number" || value.type === "gauge") {
    return Number.isFinite(value.value) ? value.value : null;
  }
  return null;
}

/** One cell rendered for reading rather than for a table row. */
function CellReading({
  column,
  value,
  health,
}: {
  column: ColumnSpec;
  value: CellValue | undefined;
  health: Row["health"];
}) {
  if (!value || value.type === "null") return <span className="text-ink-4/60">—</span>;

  switch (value.type) {
    case "status":
      return <HealthBadge health={value.value} />;
    case "tags":
      return value.value.filter(Boolean).length === 0 ? (
        <span className="text-ink-4/60">—</span>
      ) : (
        <span className="flex flex-wrap gap-1">
          {value.value.filter(Boolean).map((tag, i) => (
            <Tag key={`${tag}-${i}`}>{tag}</Tag>
          ))}
        </span>
      );
    case "duration":
      return <span className="num">{formatMetricValue(value.value, "seconds")}</span>;
    case "timestamp":
      return (
        <span className="num" title={formatDateTime(value.value)}>
          {formatRelative(value.value)}
        </span>
      );
    case "gauge":
      return (
        <Meter
          value={value.value}
          max={value.max}
          warnAbove={value.warnAbove}
          unit={unitOf(column) ?? "count"}
          width={64}
        />
      );
    case "number":
      return <span className="num">{formatMetricValue(value.value, unitOf(column) ?? "count")}</span>;
    default: {
      const text = (value as { value: string }).value;
      return column.kind.type === "code" ? (
        <code className="block break-words whitespace-pre-wrap text-ink-2">{text}</code>
      ) : (
        <span className="flex items-center gap-1.5">
          {column.primary && <Dot health={health} />}
          <span className="min-w-0 break-words">{text}</span>
        </span>
      );
    }
  }
}
