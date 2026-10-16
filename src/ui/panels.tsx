// Panels beyond the line chart: a bar gauge, a top-N table, a latency
// heatmap and a state timeline. Each draws the same series a line chart
// would, so a dashboard panel changes what it draws without a new query.

import { useEffect, useMemo, useRef, useState } from "react";

import { DATA_INK } from "../catalog";
import { formatDateTime, formatMetricValue } from "../format";
import { cells, type Bucket } from "../histogram";
import type { Health, MetricUnit, Point } from "../types";
import { Sparkline, useWidth, type Domain } from "./charts";
import { cx } from "./primitives";

export interface ValueRow {
  key: string;
  label: string;
  value: number;
  warnAbove?: number;
  points: Point[];
}

/** Biggest first: the bar gauge and the table both answer "which ones". */
function ranked(rows: ValueRow[]): ValueRow[] {
  return [...rows].sort((a, b) => b.value - a.value);
}

export function BarGauge({ rows, unit, height }: { rows: ValueRow[]; unit: MetricUnit; height: number }) {
  const ordered = ranked(rows);
  const fits = Math.max(1, Math.floor(height / 24));
  const shown = ordered.slice(0, ordered.length > fits ? fits - 1 : fits);
  // Bars share one scale, reaching to the largest value or the limit, so a
  // limit drawn on one bar means the same on every other.
  const top = Math.max(...ordered.map((r) => Math.max(r.value, r.warnAbove ?? 0)), Number.MIN_VALUE);
  return (
    <div className="flex h-full flex-col justify-center gap-1.5">
      {shown.map((row) => {
        const over = row.warnAbove !== undefined && row.value > row.warnAbove;
        return (
          <div key={row.key} className="grid grid-cols-[minmax(0,9rem)_1fr_auto] items-center gap-2">
            <span className="truncate text-2xs text-ink-3" title={row.label}>
              {row.label}
            </span>
            <span className="relative h-3 overflow-hidden rounded-sm bg-line/60">
              <span
                className="absolute inset-y-0 left-0 rounded-sm"
                style={{ width: `${Math.max(1, (row.value / top) * 100)}%`, background: over ? "var(--color-warn)" : DATA_INK }}
              />
              {row.warnAbove !== undefined && (
                <span className="absolute inset-y-0 w-px bg-warn" style={{ left: `${(row.warnAbove / top) * 100}%` }} />
              )}
            </span>
            <span className={cx("num w-16 text-right text-xs", over ? "text-warn" : "text-ink")}>
              {formatMetricValue(row.value, unit)}
            </span>
          </div>
        );
      })}
      {shown.length < ordered.length && (
        <span className="text-2xs text-ink-4">and {ordered.length - shown.length} more, smaller</span>
      )}
    </div>
  );
}

export function TopTable({ rows, unit }: { rows: ValueRow[]; unit: MetricUnit }) {
  const ordered = ranked(rows);
  return (
    <div className="h-full overflow-auto">
      <table className="w-full text-xs">
        <thead className="sticky top-0 bg-panel text-2xs text-ink-4">
          <tr>
            <th className="w-6 py-1 text-left font-normal">#</th>
            <th className="py-1 text-left font-normal">Series</th>
            <th className="w-24 py-1 font-normal" />
            <th className="py-1 text-right font-normal">Now</th>
            <th className="py-1 pl-3 text-right font-normal">Peak</th>
          </tr>
        </thead>
        <tbody>
          {ordered.map((row, i) => {
            const peak = Math.max(...row.points.map((p) => p.v));
            const over = row.warnAbove !== undefined && row.value > row.warnAbove;
            return (
              <tr key={row.key} className="border-t border-line/60">
                <td className="num py-1 text-ink-4">{i + 1}</td>
                <td className="max-w-0 py-1 pr-2">
                  <span className="block truncate text-ink-2" title={row.label}>
                    {row.label}
                  </span>
                </td>
                <td className="py-1">
                  <Sparkline points={row.points} width={88} height={18} color={DATA_INK} warnAbove={row.warnAbove} />
                </td>
                <td className={cx("num py-1 text-right", over ? "text-warn" : "text-ink")}>{formatMetricValue(row.value, unit)}</td>
                <td className="num py-1 pl-3 text-right text-ink-3">{formatMetricValue(peak, unit)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

/** The value axis is this wide, as on the line charts beside it. */
const AXIS_W = 56;
const TIME_H = 18;

/**
 * Latency as a heatmap: time across, the histogram's buckets up, and each
 * cell shaded by how many requests landed there. A log scale on the shade,
 * because the fast bucket holds most requests and would wash out the slow
 * tail that is the reason to look.
 */
export function Heatmap({ buckets, unit, domain, height }: { buckets: Bucket[]; unit: MetricUnit; domain: Domain; height: number }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const canvas = useRef<HTMLCanvasElement>(null);
  const grid = useMemo(() => cells(buckets), [buckets]);
  const [hover, setHover] = useState<{ x: number; y: number; text: string } | null>(null);
  const plotW = Math.max(0, width - AXIS_W - 8);
  const plotH = Math.max(0, height - TIME_H);
  const rowsN = grid.bounds.length;
  const rowH = rowsN > 0 ? plotH / rowsN : 0;
  const span = Math.max(1, domain[1] - domain[0]);
  const x = (t: number) => AXIS_W + ((t - domain[0]) / span) * plotW;
  const peak = Math.max(1, ...grid.counts.flat());

  useEffect(() => {
    const el = canvas.current;
    if (!el || width === 0) return;
    const ratio = window.devicePixelRatio || 1;
    el.width = width * ratio;
    el.height = height * ratio;
    const ctx = el.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);
    const ink = getComputedStyle(el).getPropertyValue("--color-data").trim() || "#4ea1ff";
    const step = grid.times.length > 1 ? grid.times[1] - grid.times[0] : span / 60;
    grid.counts.forEach((column, ti) => {
      const left = x(grid.times[ti] - step);
      const right = x(grid.times[ti]);
      if (right < AXIS_W || left > AXIS_W + plotW) return;
      // A gap between columns only where they are wide enough to keep one;
      // at a pixel or two each it reads as stripes, not as cells.
      const from = Math.max(AXIS_W, left);
      const to = Math.min(right, AXIS_W + plotW);
      const gap = to - from > 4 ? 1 : 0;
      column.forEach((count, bi) => {
        if (count <= 0) return;
        ctx.globalAlpha = 0.12 + 0.88 * (Math.log1p(count) / Math.log1p(peak));
        ctx.fillStyle = ink;
        const top = plotH - (bi + 1) * rowH;
        ctx.fillRect(from, top + 0.5, Math.max(1, to - from - gap), rowH - 1);
      });
    });
    ctx.globalAlpha = 1;
  });

  // Sub-second bounds in milliseconds: "0.0s" for 5ms, 10ms and 25ms alike is no axis.
  const bound = (v: number) =>
    unit === "seconds" && v < 1 ? `${Number((v * 1000).toFixed(v < 0.01 ? 1 : 0))}ms` : formatMetricValue(v, unit);
  const bucketLabel = (i: number) =>
    Number.isFinite(grid.bounds[i]) ? `≤ ${bound(grid.bounds[i])}` : `> ${bound(grid.bounds[i - 1] ?? 0)}`;
  // Every row labelled only where they fit; the slow end always is.
  const every = Math.max(1, Math.ceil(14 / Math.max(1, rowH)));

  return (
    <div
      ref={ref}
      className="relative"
      style={{ height }}
      onMouseMove={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        const px = e.clientX - rect.left;
        const py = e.clientY - rect.top;
        if (px < AXIS_W || px > AXIS_W + plotW || py > plotH || rowH === 0) return setHover(null);
        const t = domain[0] + ((px - AXIS_W) / plotW) * span;
        const ti = grid.times.findIndex((at) => at >= t);
        const bi = rowsN - 1 - Math.floor(py / rowH);
        if (ti < 0 || bi < 0) return setHover(null);
        setHover({ x: px, y: py, text: `${grid.counts[ti][bi].toFixed(1)} in ${bucketLabel(bi)} · ${formatDateTime(new Date(grid.times[ti]).toISOString())}` });
      }}
      onMouseLeave={() => setHover(null)}
    >
      <canvas ref={canvas} style={{ width, height }} className="absolute inset-0" />
      {grid.bounds.map((_, i) =>
        i % every === 0 || i === rowsN - 1 ? (
          <span
            key={i}
            className="num absolute right-[calc(100%-52px)] text-[10px] leading-none whitespace-nowrap text-ink-4"
            style={{ top: plotH - (i + 0.5) * rowH - 5 }}
          >
            {bucketLabel(i)}
          </span>
        ) : null,
      )}
      {[0.25, 0.5, 0.75].map((f) => (
        <span key={f} className="num absolute bottom-0 -translate-x-1/2 text-[10px] text-ink-4" style={{ left: AXIS_W + f * plotW }}>
          {new Date(domain[0] + f * span).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })}
        </span>
      ))}
      {hover && (
        <span
          className="num pointer-events-none absolute z-10 rounded-sm border border-line-strong bg-overlay px-2 py-1 text-2xs whitespace-nowrap text-ink"
          style={{ left: Math.min(hover.x + 10, width - 220), top: Math.max(0, hover.y - 28) }}
        >
          {hover.text}
        </span>
      )}
    </div>
  );
}

export interface StateRow {
  key: string;
  label: string;
  segments: { from: number; to: number; state: Health }[];
}

const STATE_COLOR: Record<Health, string> = {
  healthy: "var(--color-ok)",
  degraded: "var(--color-warn)",
  down: "var(--color-crit)",
  unknown: "var(--color-ink-4)",
};

/** Up for a value above zero, down at zero: what an `up`-style series says. */
export function statesOf(points: Point[], until: number): StateRow["segments"] {
  const segments: StateRow["segments"] = [];
  for (const [i, p] of points.entries()) {
    const state: Health = p.v > 0 ? "healthy" : "down";
    const from = Date.parse(p.t);
    const to = i + 1 < points.length ? Date.parse(points[i + 1].t) : until;
    const last = segments[segments.length - 1];
    if (last && last.state === state && last.to === from) last.to = to;
    else segments.push({ from, to, state });
  }
  return segments;
}

/** One row per thing, coloured by its state over time: when each went down, and for how long. */
export function StateTimeline({ rows, domain, height }: { rows: StateRow[]; domain: Domain; height: number }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const labelW = Math.min(140, Math.max(80, width * 0.22));
  const plotW = Math.max(0, width - labelW - 8);
  const rowH = Math.max(10, Math.min(22, (height - TIME_H) / Math.max(1, rows.length) - 4));
  const span = Math.max(1, domain[1] - domain[0]);
  const x = (t: number) => labelW + ((Math.min(domain[1], Math.max(domain[0], t)) - domain[0]) / span) * plotW;
  return (
    <div ref={ref} className="h-full overflow-y-auto">
      {width > 0 && (
        <svg width={width} height={rows.length * (rowH + 4)} className="block">
          {rows.map((row, i) => (
            <g key={row.key} transform={`translate(0 ${i * (rowH + 4)})`}>
              <text x={labelW - 8} y={rowH / 2 + 4} textAnchor="end" className="fill-ink-3 font-mono text-[10px]">
                {row.label.length > 22 ? `${row.label.slice(0, 21)}…` : row.label}
              </text>
              <rect x={labelW} y={0} width={plotW} height={rowH} rx={2} className="fill-line" opacity={0.35} />
              {row.segments
                .filter((s) => s.to > domain[0] && s.from < domain[1])
                .map((s) => (
                  <rect key={s.from} x={x(s.from)} y={0} width={Math.max(1, x(s.to) - x(s.from))} height={rowH} rx={2} fill={STATE_COLOR[s.state]} opacity={s.state === "healthy" ? 0.55 : 0.9}>
                    <title>
                      {s.state} · {formatDateTime(new Date(s.from).toISOString())} → {formatDateTime(new Date(s.to).toISOString())}
                    </title>
                  </rect>
                ))}
            </g>
          ))}
        </svg>
      )}
    </div>
  );
}
