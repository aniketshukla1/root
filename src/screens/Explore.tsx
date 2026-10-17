// Explore: ask any connection anything, on one time axis. Each row is a
// PromQL query, a LogQL query or a trace search against its own connection;
// every row shares the window, the crosshair and the zoom, so a spike in one
// lines up with the burst of log lines and the slow traces under it. A drag
// across any chart narrows them all.

import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { DATA_INK, seriesColor } from "../catalog";
import { newId, shortLabels } from "../dashboards";
import {
  accept,
  completionAt,
  DAY_MS,
  fromLocalInput,
  LOGQL_WORDS,
  PROMQL_WORDS,
  rank,
  resolve,
  toLocalInput,
  traceIdIn,
  volume,
  zoomOut,
  type Completion,
  type Window,
} from "../explore";
import { formatDateTime, formatDuration, formatTime } from "../format";
import type { EstateModel, ServiceView } from "../model";
import type { View } from "../nav";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import type { ExploreLanguage, ExploreResult, LogEvent, MetricSeries, TraceSummary } from "../types";
import { levelTextColor, TimeSeries, useWidth, type Domain, type Series } from "../ui/charts";
import { Button, cx, Empty, Input, Segmented, Select } from "../ui/primitives";

/** Every row's value axis is this wide, so their time axes line up. */
const AXIS_W = 56;
/** uPlot's right padding, which the strips under the charts match. */
const PAD_R = 8;

const PRESETS = [
  { minutes: 15, label: "15m" },
  { minutes: 60, label: "1h" },
  { minutes: 360, label: "6h" },
  { minutes: 1440, label: "24h" },
  { minutes: 10080, label: "7d" },
];

const LANGUAGE_LABEL: Record<ExploreLanguage, string> = { promql: "PromQL", logql: "LogQL", traces: "Traces" };

const PLACEHOLDER: Record<ExploreLanguage, string> = {
  promql: "sum by (instance) (rate(http_requests_total[5m]))",
  logql: '{app="orders-api"} |= "error"',
  traces: "",
};

/** What Explore opens on when another screen sends a query over. */
export interface ExploreSeed {
  profileId: string;
  language: ExploreLanguage;
  query: string;
}

interface Row {
  id: string;
  profileId: string;
  language: ExploreLanguage;
  /** What is in the editor. */
  draft: string;
  /** What last ran: the row asks again when this, the window or the connection changes. */
  query: string;
  /** Bumped by Run, so running the same query again asks again. */
  runs: number;
}

function blankRow(sources: ServiceView[], seed?: ExploreSeed): Row {
  if (seed) return { id: newId(), ...seed, draft: seed.query, runs: 0 };
  return { id: newId(), profileId: sources[0]?.snapshot.profileId ?? "", language: speaks(sources[0])[0] ?? "promql", draft: "", query: "", runs: 0 };
}

function speaks(service: ServiceView | undefined): ExploreLanguage[] {
  if (service?.snapshot.kind === "gigapipe") return ["promql", "logql", "traces"];
  // An OTLP receiver keeps the traces sent to it; its metrics and lines are root's own.
  if (service?.snapshot.kind === "otlp") return ["traces"];
  return service?.snapshot.kind === "prometheus" ? ["promql"] : [];
}

export function Explore({
  model,
  catalog,
  seed,
  onNavigate,
  onAsk,
}: {
  model: EstateModel;
  catalog: Catalog;
  seed?: ExploreSeed;
  onNavigate: (view: View) => void;
  onAsk: () => void;
}) {
  const sources = model.services.filter((s) => speaks(s).length > 0);
  const [window, setWindow] = useState<Window>({ minutes: 60 });
  const [now, setNow] = useState(() => Date.now());
  const [compare, setCompare] = useState(false);
  const [hoverTime, setHoverTime] = useState<number | null>(null);
  const [rows, setRows] = useState<Row[]>(() => [blankRow(sources, seed)]);
  const [custom, setCustom] = useState<{ from: string; to: string } | null>(null);
  const range = useMemo(() => resolve(window, now), [window, now]);

  // A row whose connection has gone, or that was opened before the estate
  // loaded, takes the first connection that can answer it.
  const firstSource = sources[0]?.snapshot.profileId ?? "";
  useEffect(() => {
    if (!firstSource) return;
    setRows((all) =>
      all.some((r) => !sources.some((s) => s.snapshot.profileId === r.profileId))
        ? all.map((r) => (sources.some((s) => s.snapshot.profileId === r.profileId) ? r : { ...r, profileId: firstSource }))
        : all,
    );
    // `sources` is rebuilt every render; its first id is what matters.
  }, [firstSource]); // eslint-disable-line react-hooks/exhaustive-deps

  const meta =
    "minutes" in window
      ? `last ${PRESETS.find((p) => p.minutes === window.minutes)?.label ?? `${window.minutes / 60}h`}`
      : `${formatDateTime(new Date(window.from).toISOString())} → ${formatDateTime(new Date(window.to).toISOString())}`;

  const applyCustom = () => {
    if (!custom) return;
    const from = fromLocalInput(custom.from);
    const to = fromLocalInput(custom.to);
    if (from === null || to === null || to <= from) return;
    setWindow({ from, to });
    setCustom(null);
  };
  const customValid = (() => {
    if (!custom) return false;
    const from = fromLocalInput(custom.from);
    const to = fromLocalInput(custom.to);
    return from !== null && to !== null && to > from;
  })();

  if (sources.length === 0) {
    return (
      <>
        <ScreenHeader title="Explore" onAsk={onAsk} />
        <Screen>
          <Empty
            title="Nothing to explore yet"
            hint="Explore runs PromQL on a Prometheus connection, and PromQL, LogQL and trace search on a Gigapipe one. Add either in Settings."
          />
          <div className="mt-3">
            <Button onClick={() => onNavigate({ name: "settings" })}>Open Settings</Button>
          </div>
        </Screen>
      </>
    );
  }

  return (
    <>
      <ScreenHeader title="Explore" meta={meta} onRefresh={() => setNow(Date.now())} onAsk={onAsk} />
      <Screen className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <Segmented
            label="Time range"
            value={"minutes" in window ? String(window.minutes) : "custom"}
            onChange={(v) => {
              if (v === "custom") {
                setCustom({ from: toLocalInput(range[0]), to: toLocalInput(range[1]) });
                return;
              }
              setCustom(null);
              setWindow({ minutes: Number(v) });
              setNow(Date.now());
            }}
            options={[
              ...PRESETS.map((p) => ({ value: String(p.minutes), label: p.label })),
              { value: "custom", label: "Custom" },
            ]}
          />
          {custom && (
            <span className="flex flex-wrap items-center gap-1.5">
              <Input
                type="datetime-local"
                size="sm"
                value={custom.from}
                onChange={(e) => setCustom({ ...custom, from: e.target.value })}
                aria-label="From"
              />
              <span className="text-ink-4">→</span>
              <Input
                type="datetime-local"
                size="sm"
                value={custom.to}
                onChange={(e) => setCustom({ ...custom, to: e.target.value })}
                aria-label="To"
              />
              <Button size="sm" variant="primary" disabled={!customValid} onClick={applyCustom}>
                Apply
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setCustom(null)}>
                Cancel
              </Button>
            </span>
          )}
          <Button
            size="sm"
            variant="ghost"
            title="Twice the window, around the same middle"
            onClick={() => {
              const at = Date.now();
              setWindow(zoomOut(window, at));
              setNow(at);
            }}
          >
            Zoom out
          </Button>
          <label className="flex cursor-pointer items-center gap-1.5 text-xs text-ink-3 select-none">
            <input type="checkbox" checked={compare} onChange={(e) => setCompare(e.target.checked)} className="accent-accent" />
            Compare with yesterday
          </label>
        </div>

        {rows.map((row, index) => (
          <QueryRow
            key={row.id}
            row={row}
            letter={String.fromCharCode(65 + (index % 26))}
            sources={sources}
            catalog={catalog}
            range={range}
            compare={compare}
            hoverTime={hoverTime}
            onHoverTime={setHoverTime}
            onBrush={([from, to]) => setWindow({ from, to })}
            onChange={(next) => setRows((all) => all.map((r) => (r.id === row.id ? next : r)))}
            onRemove={rows.length > 1 ? () => setRows((all) => all.filter((r) => r.id !== row.id)) : undefined}
            onNavigate={onNavigate}
          />
        ))}
        <div>
          <Button onClick={() => setRows((all) => [...all, blankRow(sources)])}>Add a query</Button>
        </div>
      </Screen>
    </>
  );
}

// ---------------------------------------------------------------- one row

interface Answer {
  result?: ExploreResult;
  /** The same query a day earlier, moved forward a day to sit under today's. */
  yesterday?: MetricSeries[];
  error?: string;
  loading: boolean;
}

function QueryRow({
  row,
  letter,
  sources,
  catalog,
  range,
  compare,
  hoverTime,
  onHoverTime,
  onBrush,
  onChange,
  onRemove,
  onNavigate,
}: {
  row: Row;
  letter: string;
  sources: ServiceView[];
  catalog: Catalog;
  range: [number, number];
  compare: boolean;
  hoverTime: number | null;
  onHoverTime: (t: number | null) => void;
  onBrush: (range: Domain) => void;
  onChange: (row: Row) => void;
  onRemove?: () => void;
  onNavigate: (view: View) => void;
}) {
  const source = sources.find((s) => s.snapshot.profileId === row.profileId);
  const languages = speaks(source);
  const [answer, setAnswer] = useState<Answer>({ loading: false });
  const [from, to] = range;
  const ready = Boolean(row.profileId) && (row.language === "traces" || row.query.trim() !== "");

  useEffect(() => {
    if (!ready) {
      setAnswer({ loading: false });
      return;
    }
    let live = true;
    setAnswer((a) => ({ ...a, loading: true, error: undefined }));
    (async () => {
      try {
        const result = await api.explore(row.profileId, row.language, row.query, from, to);
        // Yesterday is a reference, not the answer: if it fails, today stands.
        const yesterday =
          compare && result.series.length > 0
            ? await api
                .explore(row.profileId, row.language, row.query, from - DAY_MS, to - DAY_MS)
                .then((r) =>
                  r.series.map((s) => ({
                    ...s,
                    points: s.points.map((p) => ({ ...p, t: new Date(Date.parse(p.t) + DAY_MS).toISOString() })),
                  })),
                )
                .catch(() => undefined)
            : undefined;
        if (live) setAnswer({ result, yesterday, loading: false });
      } catch (e) {
        if (live) setAnswer({ loading: false, error: errorMessage(e) });
      }
    })();
    return () => {
      live = false;
    };
  }, [row.profileId, row.language, row.query, row.runs, from, to, compare, ready]);

  const run = () => onChange({ ...row, query: row.draft, runs: row.runs + 1 });

  return (
    <section className="panel flex flex-col gap-2 p-3" aria-label={`Query ${letter}`}>
      <div className="flex flex-wrap items-center gap-2">
        <span className="num w-4 text-2xs text-ink-4">{letter}</span>
        <Select
          size="sm"
          value={row.profileId}
          aria-label="Connection"
          onChange={(e) => {
            const next = sources.find((s) => s.snapshot.profileId === e.target.value);
            const language = speaks(next).includes(row.language) ? row.language : (speaks(next)[0] ?? "promql");
            onChange({ ...row, profileId: e.target.value, language, query: language === row.language ? row.query : "" });
          }}
        >
          {sources.map((s) => (
            <option key={s.snapshot.profileId} value={s.snapshot.profileId}>
              {s.snapshot.profileName} · {catalog.label(s.snapshot.kind)}
            </option>
          ))}
        </Select>
        {languages.length > 1 && (
          <Segmented
            label="Language"
            value={row.language}
            onChange={(language) => onChange({ ...row, language, draft: "", query: "" })}
            options={languages.map((l) => ({ value: l, label: LANGUAGE_LABEL[l] }))}
          />
        )}
        <span className="ml-auto flex items-center gap-2">
          {answer.loading && <span className="text-2xs text-ink-4">Running…</span>}
          {row.language !== "traces" && (
            <Button size="sm" variant="primary" disabled={!row.draft.trim()} onClick={run} title="Run (⌘↵)">
              Run
            </Button>
          )}
          {onRemove && (
            <Button size="sm" variant="ghost" onClick={onRemove} aria-label={`Remove query ${letter}`}>
              ×
            </Button>
          )}
        </span>
      </div>

      {row.language !== "traces" && (
        <QueryEditor
          value={row.draft}
          onChange={(draft) => onChange({ ...row, draft })}
          onRun={run}
          language={row.language}
          profileId={row.profileId}
        />
      )}

      {answer.error ? (
        <p className="text-xs text-crit wrap-anywhere">{answer.error}</p>
      ) : !ready ? (
        <p className="text-2xs text-ink-4">
          {row.language === "promql"
            ? "A metric name, then {labels} — the list offers what this store has. ⌘↵ or Shift↵ runs it."
            : 'A stream selector such as {app="orders-api"}, then |= "error" to filter. ⌘↵ or Shift↵ runs it.'}
        </p>
      ) : answer.result ? (
        <AnswerView
          result={answer.result}
          yesterday={answer.yesterday}
          range={range}
          hoverTime={hoverTime}
          onHoverTime={onHoverTime}
          onBrush={onBrush}
          onTrace={(traceId) => onNavigate({ name: "trace", profileId: row.profileId, traceId })}
        />
      ) : null}
    </section>
  );
}

function AnswerView({
  result,
  yesterday,
  range,
  hoverTime,
  onHoverTime,
  onBrush,
  onTrace,
}: {
  result: ExploreResult;
  yesterday?: MetricSeries[];
  range: [number, number];
  hoverTime: number | null;
  onHoverTime: (t: number | null) => void;
  onBrush: (range: Domain) => void;
  onTrace: (traceId: string) => void;
}) {
  if (result.series.length > 0) {
    const names = shortLabels(result.series.map((s) => s.label));
    const series: Series[] = result.series.map((s, i) => ({
      key: s.key,
      label: names[i],
      color: result.series.length === 1 ? DATA_INK : seriesColor(i),
      points: s.points,
    }));
    const byKey = new Map(series.map((s) => [s.key, s]));
    for (const s of yesterday ?? []) {
      const today = byKey.get(s.key);
      series.push({
        key: `${s.key}#yesterday`,
        label: `${today?.label ?? s.label} · yesterday`,
        color: today?.color ?? DATA_INK,
        points: s.points,
        dashed: true,
      });
    }
    return (
      <div className="flex flex-col gap-1">
        <TimeSeries
          series={series}
          unit={result.series[0].unit}
          domain={range}
          height={220}
          showLegend={series.length > 1}
          hoverTime={hoverTime}
          onHoverTime={onHoverTime}
          onBrush={onBrush}
          axisWidth={AXIS_W}
        />
        {result.matched > result.series.length && (
          <p className="text-2xs text-ink-4">
            Showing {result.series.length} of {result.matched} series — add a label or a{" "}
            <code className="font-mono">by (…)</code> to narrow it.
          </p>
        )}
      </div>
    );
  }
  if (result.lines.length > 0) {
    return <LinesView lines={result.lines} range={range} hoverTime={hoverTime} onHoverTime={onHoverTime} onTrace={onTrace} />;
  }
  if (result.traces.length > 0) {
    return <TracesView traces={result.traces} range={range} hoverTime={hoverTime} onHoverTime={onHoverTime} onTrace={onTrace} />;
  }
  return <p className="text-2xs text-ink-4">Nothing matched in this window.</p>;
}

// ----------------------------------------------------------------- strips

/**
 * A strip on the charts' time axis: the same left and right edges as a
 * chart's plot, so log volume and traces sit exactly under the spike they
 * explain, with the shared crosshair drawn through them.
 */
function Strip({
  range,
  height,
  hoverTime,
  onHoverTime,
  children,
}: {
  range: [number, number];
  height: number;
  hoverTime: number | null;
  onHoverTime: (t: number | null) => void;
  children: (x: (t: number) => number, plotWidth: number) => ReactNode;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const plotWidth = Math.max(0, width - AXIS_W - PAD_R);
  const span = Math.max(1, range[1] - range[0]);
  const x = (t: number) => AXIS_W + ((t - range[0]) / span) * plotWidth;
  return (
    <div ref={ref} style={{ height }}>
      {width > 0 && (
        <svg
          width={width}
          height={height}
          className="block"
          onMouseMove={(e) => {
            const px = e.clientX - e.currentTarget.getBoundingClientRect().left - AXIS_W;
            onHoverTime(px < 0 || px > plotWidth ? null : range[0] + (px / plotWidth) * span);
          }}
          onMouseLeave={() => onHoverTime(null)}
        >
          <line x1={AXIS_W} x2={AXIS_W + plotWidth} y1={height - 0.5} y2={height - 0.5} className="stroke-line" />
          {children(x, plotWidth)}
          {hoverTime !== null && hoverTime >= range[0] && hoverTime <= range[1] && (
            <line x1={x(hoverTime)} x2={x(hoverTime)} y1={0} y2={height} className="stroke-ink-4" strokeDasharray="2 2" />
          )}
        </svg>
      )}
    </div>
  );
}

const BUCKETS = 80;
/** The backend's cap on one LogQL answer, so a full page can say so. */
const LINE_CAP = 1000;

function LinesView({
  lines,
  range,
  hoverTime,
  onHoverTime,
  onTrace,
}: {
  lines: LogEvent[];
  range: [number, number];
  hoverTime: number | null;
  onHoverTime: (t: number | null) => void;
  onTrace: (traceId: string) => void;
}) {
  const all = useMemo(() => volume(lines.map((l) => Date.parse(l.timestamp)), range, BUCKETS), [lines, range]);
  const bad = useMemo(
    () => volume(lines.filter((l) => l.level === "error" || l.level === "fatal").map((l) => Date.parse(l.timestamp)), range, BUCKETS),
    [lines, range],
  );
  const peak = Math.max(1, ...all);
  const long = range[1] - range[0] > DAY_MS;
  const errors = bad.reduce((sum, n) => sum + n, 0);
  return (
    <div className="flex flex-col gap-2">
      <p className="num text-2xs text-ink-4">
        {lines.length === LINE_CAP ? `the newest ${LINE_CAP} lines` : `${lines.length} line${lines.length === 1 ? "" : "s"}`}
        {errors > 0 && <span style={{ color: levelTextColor("error") }}> · {errors} error{errors === 1 ? "" : "s"}</span>}
      </p>
      <Strip range={range} height={56} hoverTime={hoverTime} onHoverTime={onHoverTime}>
        {(x, plotWidth) => {
          const w = plotWidth / BUCKETS;
          const h = 56 - 16;
          return all.map((count, i) => {
            if (count === 0) return null;
            const left = x(range[0]) + i * w;
            const total = (count / peak) * h;
            const errors = (bad[i] / peak) * h;
            return (
              <g key={i}>
                <rect x={left + 0.5} y={56 - total} width={Math.max(1, w - 1)} height={total} className="fill-ink-4" opacity={0.55} />
                {errors > 0 && (
                  <rect x={left + 0.5} y={56 - errors} width={Math.max(1, w - 1)} height={errors} fill={levelTextColor("error")} />
                )}
              </g>
            );
          });
        }}
      </Strip>
      <div className="max-h-[420px] overflow-auto rounded-md border border-line">
        <table className="w-full text-xs">
          <tbody>
            {lines.map((line) => {
              const traceId = traceIdIn(line.message);
              return (
                <tr key={line.id} className="border-b border-line/60 align-top last:border-0">
                  <td className="num w-px px-2 py-1 text-2xs whitespace-nowrap text-ink-4">
                    {long ? formatDateTime(line.timestamp) : formatTime(line.timestamp)}
                  </td>
                  <td className="num w-px px-1 py-1 text-2xs" style={{ color: levelTextColor(line.level) }}>
                    {line.level.toUpperCase()}
                  </td>
                  <td className="w-px px-2 py-1">
                    <span className="num block w-24 truncate text-2xs text-ink-4" title={line.source}>
                      {line.source}
                    </span>
                  </td>
                  <td className="px-2 py-1">
                    <span className="num block text-ink-2 wrap-anywhere">{line.message}</span>
                  </td>
                  <td className="w-px px-2 py-1">
                    {traceId && (
                      <Button size="sm" variant="ghost" onClick={() => onTrace(traceId)} title={`Open trace ${traceId}`}>
                        Trace
                      </Button>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function TracesView({
  traces,
  range,
  hoverTime,
  onHoverTime,
  onTrace,
}: {
  traces: TraceSummary[];
  range: [number, number];
  hoverTime: number | null;
  onHoverTime: (t: number | null) => void;
  onTrace: (traceId: string) => void;
}) {
  const [filter, setFilter] = useState("");
  const needle = filter.trim().toLowerCase();
  const shown = traces.filter((t) => !needle || `${t.rootService} ${t.rootName}`.toLowerCase().includes(needle));
  const slowest = Math.max(1, ...shown.map((t) => t.durationMs));
  // Durations spread over orders of magnitude; a log scale keeps the fast
  // majority from collapsing onto the floor under one slow outlier.
  const y = (ms: number) => 60 - (Math.log1p(ms) / Math.log1p(slowest)) * 48;
  return (
    <div className="flex flex-col gap-2">
      <p className="num text-2xs text-ink-4">
        {shown.length} trace{shown.length === 1 ? "" : "s"} · higher is slower · slowest {formatDuration(slowest)}
      </p>
      <Strip range={range} height={64} hoverTime={hoverTime} onHoverTime={onHoverTime}>
        {(x) =>
          shown.map((t) => (
            <circle
              key={t.traceId}
              cx={x(Date.parse(t.startedAt))}
              cy={y(t.durationMs)}
              r={3.5}
              fill={t.errorCount ? levelTextColor("error") : "var(--color-accent)"}
              opacity={0.85}
              className="cursor-pointer"
              onClick={() => onTrace(t.traceId)}
            >
              <title>
                {t.rootService} · {t.rootName} · {formatDuration(t.durationMs)}
                {t.errorCount ? ` · ${t.errorCount} error${t.errorCount === 1 ? "" : "s"}` : ""}
              </title>
            </circle>
          ))
        }
      </Strip>
      <Input
        size="sm"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        placeholder="Filter by service or operation"
        aria-label="Filter traces"
        className="max-w-72"
      />
      <div className="max-h-[420px] overflow-auto rounded-md border border-line">
        <table className="w-full text-xs">
          <tbody>
            {shown.map((t) => (
              <tr
                key={t.traceId}
                onClick={() => onTrace(t.traceId)}
                className="row-hover cursor-pointer border-b border-line/60 last:border-0"
              >
                <td className="num w-px px-2 py-1 text-2xs whitespace-nowrap text-ink-4">{formatTime(t.startedAt)}</td>
                <td className="px-2 py-1">
                  <span className="text-ink-2">{t.rootService}</span>{" "}
                  <span className="num text-2xs text-ink-4">{t.rootName}</span>
                </td>
                <td className="w-40 px-2 py-1">
                  <span className="flex items-center gap-2">
                    <span className="h-1.5 flex-1 overflow-hidden rounded-full bg-line">
                      <span
                        className="block h-full rounded-full"
                        style={{
                          width: `${(t.durationMs / slowest) * 100}%`,
                          background: t.errorCount ? levelTextColor("error") : "var(--color-accent)",
                        }}
                      />
                    </span>
                    <span className="num w-14 text-right text-2xs text-ink-3">{formatDuration(t.durationMs)}</span>
                  </span>
                </td>
                <td className="num w-px px-2 py-1 text-2xs whitespace-nowrap" style={{ color: t.errorCount ? levelTextColor("error") : undefined }}>
                  {t.errorCount ? `${t.errorCount} error${t.errorCount === 1 ? "" : "s"}` : ""}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// ----------------------------------------------------------------- editor

/** Names this store has, fetched once per connection, language and label. */
function useNames(profileId: string, language: ExploreLanguage, completion: Completion | null) {
  const [cache, setCache] = useState<Record<string, string[]>>({});
  const label =
    completion?.kind === "value"
      ? (completion.label ?? null)
      : completion?.kind === "label"
        ? ""
        : completion?.kind === "name" && language === "promql"
          ? "__name__"
          : null;
  const key = label === null ? null : `${profileId}/${language}/${label}`;

  useEffect(() => {
    if (key === null || label === null || cache[key] !== undefined || !profileId) return;
    let live = true;
    api
      .exploreSuggest(profileId, language, label || undefined)
      .then((names) => live && setCache((c) => ({ ...c, [key]: names })))
      .catch(() => live && setCache((c) => ({ ...c, [key]: [] })));
    return () => {
      live = false;
    };
  }, [key, label, profileId, language, cache]);

  if (!completion) return [];
  const fetched = key === null ? [] : (cache[key] ?? []);
  if (completion.kind === "value") return fetched;
  if (completion.kind === "label") return fetched.filter((n) => n !== "__name__");
  return language === "promql" ? [...fetched, ...PROMQL_WORDS] : LOGQL_WORDS;
}

function QueryEditor({
  value,
  onChange,
  onRun,
  language,
  profileId,
}: {
  value: string;
  onChange: (value: string) => void;
  onRun: () => void;
  language: ExploreLanguage;
  profileId: string;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [cursor, setCursor] = useState(0);
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [active, setActive] = useState(0);
  const completion = focused ? completionAt(value, cursor, language) : null;
  const candidates = useNames(profileId, language, completion);
  const options = completion ? rank(candidates, completion.prefix) : [];
  const open = !dismissed && options.length > 0;
  const chosen = Math.min(active, Math.max(0, options.length - 1));
  const words = language === "promql" ? PROMQL_WORDS : LOGQL_WORDS;

  // React writing a new value parks the caret at the end. Where a choice
  // means it to be goes back in the same commit, before another key lands.
  const caret = useRef<number | null>(null);
  useLayoutEffect(() => {
    if (caret.current === null) return;
    ref.current?.setSelectionRange(caret.current, caret.current);
    caret.current = null;
  }, [value]);

  const choose = (choice: string) => {
    if (!completion) return;
    const next = accept(value, cursor, completion, choice);
    caret.current = next.cursor;
    onChange(next.text);
    setCursor(next.cursor);
    setActive(0);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey || e.shiftKey)) {
      e.preventDefault();
      setDismissed(true);
      onRun();
      return;
    }
    if (!open) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      setActive((chosen + step + options.length) % options.length);
    } else if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      choose(options[chosen]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      setDismissed(true);
    }
  };

  return (
    <div className="relative">
      <textarea
        ref={ref}
        value={value}
        rows={Math.min(6, Math.max(2, value.split("\n").length))}
        onChange={(e) => {
          onChange(e.target.value);
          setCursor(e.target.selectionStart);
          setDismissed(false);
          setActive(0);
        }}
        onSelect={(e) => setCursor(e.currentTarget.selectionStart)}
        onKeyDown={onKeyDown}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        spellCheck={false}
        placeholder={PLACEHOLDER[language]}
        aria-label={`${LANGUAGE_LABEL[language]} query`}
        aria-autocomplete="list"
        aria-expanded={open}
        className="w-full resize-y rounded-md border border-line-strong bg-sunk px-2.5 py-2 font-mono text-xs leading-relaxed text-ink placeholder:text-ink-4 focus:border-ink/45 focus:outline-none"
      />
      {open && completion && (
        <ul
          role="listbox"
          aria-label="Suggestions"
          className="absolute top-full left-0 z-20 mt-1 max-h-64 min-w-64 overflow-auto rounded-md border border-line-strong bg-overlay p-1 shadow-lg"
        >
          {options.map((option, i) => (
            <li key={option} role="option" aria-selected={i === chosen}>
              <button
                type="button"
                onMouseDown={(e) => e.preventDefault()}
                onMouseEnter={() => setActive(i)}
                onClick={() => choose(option)}
                className={cx(
                  "flex w-full items-baseline justify-between gap-4 rounded-sm px-2 py-1 text-left font-mono text-xs",
                  i === chosen ? "bg-raised text-ink" : "text-ink-3",
                )}
              >
                {option}
                <span className="text-2xs text-ink-4">
                  {completion.kind === "value"
                    ? completion.label
                    : completion.kind === "label"
                      ? "label"
                      : words.includes(option)
                        ? "function"
                        : "metric"}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
