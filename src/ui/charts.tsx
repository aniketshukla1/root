// Charts are hand-drawn SVG rather than a charting dependency, because every
// form here is narrow and the constraints are strict: thin recessive marks, a
// hover layer on everything that plots, one y-axis per chart (two units means
// two charts, never two scales), and status colours that never double as
// series identity.
//
// TIME DOMAIN. `TimeSeries` and `LogHistogram` position marks by *timestamp*
// against an explicit `[from, to]` domain, not by array index. That is what
// lets a stack of panels on the service dashboard be read straight down at a
// fixed instant — two series with different sample counts land on the same x
// for the same moment. Index-based positioning silently misaligns them, which
// is invisible in an isolated card and wrong on a dashboard.

import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { formatMetricValue, formatTime } from "../format";
import type { LogLevel, MetricUnit, Point } from "../types";
import { cx } from "./primitives";

/** Inclusive millisecond bounds of a chart's x-axis. */
export type Domain = [number, number];

/** A moment worth marking on the time axis — an alert, later a deploy. */
export interface Annotation {
  id: string;
  /** Milliseconds. */
  t: number;
  color: string;
  label: string;
  detail?: string;
}

/** Width tracking, so strokes stay 2px instead of being scaled by a viewBox. */
export function useWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);

  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) => {
      setWidth(Math.round(entry.contentRect.width));
    });
    observer.observe(node);
    setWidth(Math.round(node.getBoundingClientRect().width));
    return () => observer.disconnect();
  }, []);

  return [ref, width] as const;
}

function extent(values: number[]): [number, number] {
  let min = Infinity;
  let max = -Infinity;
  for (const v of values) {
    if (!Number.isFinite(v)) continue;
    if (v < min) min = v;
    if (v > max) max = v;
  }
  if (min === Infinity) return [0, 1];
  // A perfectly flat series still needs a band to draw in, and it should sit in
  // the middle of it rather than along the floor.
  if (min === max) return min === 0 ? [0, 1] : [min * 0.95, max * 1.05];
  return [min, max];
}

function ms(iso: string): number {
  return new Date(iso).getTime();
}

// ---------------------------------------------------------------- sparkline

/**
 * Inline trend, one series. No axes, no legend — whatever labels it names it.
 * The final value gets a marker so "where it is now" is readable without hover.
 * Deliberately index-positioned: a sparkline is a shape, not a reading, and it
 * is never stacked with other panels.
 */
export function Sparkline({
  points,
  width = 96,
  height = 24,
  color = "var(--color-ink-3)",
  warnAbove,
}: {
  points: Point[];
  width?: number;
  height?: number;
  color?: string;
  warnAbove?: number;
}) {
  if (points.length < 2) {
    return (
      <div
        style={{ width, height }}
        className="flex items-center justify-center text-2xs text-ink-4/60"
        title="Not enough history yet"
      >
        —
      </div>
    );
  }

  const values = points.map((p) => p.v);
  const [lo, hi] = extent(warnAbove !== undefined ? [...values, warnAbove] : values);
  const pad = 3;
  const x = (i: number) => (i / (points.length - 1)) * (width - 2) + 1;
  const y = (v: number) => height - pad - ((v - lo) / (hi - lo)) * (height - pad * 2);

  const line = points
    .map((p, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(1)},${y(p.v).toFixed(1)}`)
    .join(" ");
  const area = `${line} L${x(points.length - 1).toFixed(1)},${height} L${x(0).toFixed(1)},${height} Z`;
  const last = points[points.length - 1];
  const breaching = warnAbove !== undefined && last.v > warnAbove;
  const stroke = breaching ? "var(--color-warn)" : color;
  const id = `spark-${Math.abs(points.length * 31 + Math.round(last.v * 100))}`;

  return (
    <svg width={width} height={height} className="overflow-visible" aria-hidden>
      <defs>
        <linearGradient id={id} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={stroke} stopOpacity="0.22" />
          <stop offset="100%" stopColor={stroke} stopOpacity="0" />
        </linearGradient>
      </defs>
      {warnAbove !== undefined && warnAbove <= hi && warnAbove >= lo && (
        <line
          x1={1}
          x2={width - 1}
          y1={y(warnAbove)}
          y2={y(warnAbove)}
          stroke="var(--color-warn)"
          strokeWidth="1"
          strokeDasharray="2 2"
          opacity="0.5"
        />
      )}
      <path d={area} fill={`url(#${id})`} />
      <path
        d={line}
        fill="none"
        stroke={stroke}
        strokeWidth="1.5"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
      <circle cx={x(points.length - 1)} cy={y(last.v)} r="2" fill={stroke} />
    </svg>
  );
}

// -------------------------------------------------------------- time series

export interface Series {
  key: string;
  label: string;
  color: string;
  points: Point[];
  warnAbove?: number;
}

/**
 * Multi-series line chart on an explicit time domain.
 *
 * Every series on one chart must share a unit — the caller facets by unit
 * before it gets here. A second y-scale is never an option: it lets any two
 * shapes be made to look correlated.
 *
 * `hoverTime` / `onHoverTime` are controlled, so a parent can drive one
 * crosshair across a whole page of panels. Leave them out and the chart hovers
 * on its own.
 */
export function TimeSeries({
  series,
  unit,
  domain,
  height = 168,
  showLegend = true,
  annotations,
  hoverTime,
  onHoverTime,
  onBrush,
}: {
  series: Series[];
  unit: MetricUnit;
  domain: Domain;
  height?: number;
  showLegend?: boolean;
  annotations?: Annotation[];
  hoverTime?: number | null;
  onHoverTime?: (t: number | null) => void;
  /** Drag across the plot to pick a narrower window. */
  onBrush?: (range: Domain) => void;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [localHover, setLocalHover] = useState<number | null>(null);
  const [brush, setBrush] = useState<{ from: number; to: number } | null>(null);

  const controlled = onHoverTime !== undefined;
  const hover = controlled ? (hoverTime ?? null) : localHover;
  const setHover = (t: number | null) => (controlled ? onHoverTime!(t) : setLocalHover(t));

  const plotted = useMemo(() => series.filter((s) => s.points.length > 1), [series]);

  // Wide enough for a formatted byte value ("284.1 GB") at the tick size.
  const padL = 54;
  const padR = 8;
  const padT = annotations && annotations.length > 0 ? 12 : 8;
  const padB = 18;
  const innerW = Math.max(0, width - padL - padR);
  const innerH = height - padT - padB;

  const [from, to] = domain;
  const spanMs = Math.max(1, to - from);

  const scale = useMemo(() => {
    const all = plotted.flatMap((s) =>
      s.points.filter((p) => {
        const t = ms(p.t);
        return t >= from && t <= to;
      }).map((p) => p.v),
    );
    const thresholds = plotted
      .map((s) => s.warnAbove)
      .filter((v): v is number => v !== undefined);
    let [lo, hi] = extent([...all, ...thresholds]);
    // Anchor to zero when the data sits close to it, so a small wobble high
    // above the baseline is not drawn as a mountain range.
    if (lo > 0 && lo < (hi - lo) * 1.2) lo = 0;
    return { lo, hi };
  }, [plotted, from, to]);

  if (plotted.length === 0) {
    return (
      <div
        ref={ref}
        style={{ height }}
        className="flex items-center justify-center text-xs text-ink-4"
      >
        Not enough history yet
      </div>
    );
  }

  const x = (t: number) => padL + ((t - from) / spanMs) * innerW;
  const y = (v: number) =>
    padT + innerH - ((v - scale.lo) / (scale.hi - scale.lo || 1)) * innerH;
  const timeAt = (px: number) => from + ((px - padL) / (innerW || 1)) * spanMs;

  const ticks = [scale.lo, scale.lo + (scale.hi - scale.lo) / 2, scale.hi];
  const clipId = `plot-${Math.round(from)}-${Math.round(height)}`;

  const onMove = (event: React.MouseEvent<SVGSVGElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const px = event.clientX - rect.left;
    if (px < padL || px > padL + innerW) {
      setHover(null);
      return;
    }
    const t = timeAt(px);
    setHover(t);
    if (brush) setBrush({ ...brush, to: t });
  };

  const onDown = (event: React.MouseEvent<SVGSVGElement>) => {
    if (!onBrush) return;
    const rect = event.currentTarget.getBoundingClientRect();
    const px = event.clientX - rect.left;
    if (px < padL || px > padL + innerW) return;
    const t = timeAt(px);
    setBrush({ from: t, to: t });
  };

  const onUp = () => {
    if (!brush || !onBrush) return setBrush(null);
    const lo = Math.min(brush.from, brush.to);
    const hi = Math.max(brush.from, brush.to);
    // Ignore a click that happened to land inside the plot.
    if (x(hi) - x(lo) > 8) onBrush([lo, hi]);
    setBrush(null);
  };

  /** Nearest sample to the crosshair, so the tooltip reads real values. */
  const nearest = (s: Series): Point | undefined => {
    if (hover === null) return undefined;
    let best: Point | undefined;
    let bestGap = Infinity;
    for (const p of s.points) {
      const gap = Math.abs(ms(p.t) - hover);
      if (gap < bestGap) {
        bestGap = gap;
        best = p;
      }
    }
    // Beyond a couple of pixels' worth of time there is no sample to report.
    return bestGap <= (spanMs / Math.max(1, innerW)) * 6 ? best : undefined;
  };

  const inView = (annotations ?? []).filter((a) => a.t >= from && a.t <= to);

  return (
    <div ref={ref} className="relative">
      {width > 0 && (
        <svg
          width={width}
          height={height}
          onMouseMove={onMove}
          onMouseLeave={() => {
            setHover(null);
            setBrush(null);
          }}
          onMouseDown={onDown}
          onMouseUp={onUp}
          className={cx("block", onBrush && "cursor-crosshair select-none")}
        >
          <defs>
            <clipPath id={clipId}>
              <rect x={padL} y={padT} width={innerW} height={innerH} />
            </clipPath>
          </defs>

          {/* Recessive grid — present enough to read a value against, quiet
              enough that the data is what you see. */}
          {ticks.map((tick) => (
            <g key={tick}>
              <line
                x1={padL}
                x2={padL + innerW}
                y1={y(tick)}
                y2={y(tick)}
                stroke="var(--color-line)"
                strokeWidth="1"
              />
              {/* `tnum` rather than `num`: the latter also sets a font-size, and
                  a CSS size beats the SVG attribute, which overflows the gutter
                  and clips the leading digit. */}
              <text
                x={padL - 6}
                y={y(tick) + 3}
                textAnchor="end"
                className="tnum"
                fontSize="9"
                fill="var(--color-ink-4)"
              >
                {formatMetricValue(tick, unit)}
              </text>
            </g>
          ))}

          {plotted.map((s) =>
            s.warnAbove !== undefined &&
            s.warnAbove >= scale.lo &&
            s.warnAbove <= scale.hi ? (
              <line
                key={`limit-${s.key}`}
                x1={padL}
                x2={padL + innerW}
                y1={y(s.warnAbove)}
                y2={y(s.warnAbove)}
                stroke="var(--color-warn)"
                strokeWidth="1"
                strokeDasharray="3 3"
                opacity="0.55"
              />
            ) : null,
          )}

          <g clipPath={`url(#${clipId})`}>
            {inView.map((annotation) => (
              <line
                key={annotation.id}
                x1={x(annotation.t)}
                x2={x(annotation.t)}
                y1={padT}
                y2={padT + innerH}
                stroke={annotation.color}
                strokeWidth="1"
                strokeDasharray="2 3"
                opacity="0.75"
              />
            ))}

            {plotted.map((s) => {
              const path = s.points
                .map((p, i) => `${i === 0 ? "M" : "L"}${x(ms(p.t)).toFixed(1)},${y(p.v).toFixed(1)}`)
                .join(" ");
              return (
                <path
                  key={s.key}
                  d={path}
                  fill="none"
                  stroke={s.color}
                  strokeWidth="2"
                  strokeLinejoin="round"
                  strokeLinecap="round"
                />
              );
            })}

            {brush && (
              <rect
                x={Math.min(x(brush.from), x(brush.to))}
                y={padT}
                width={Math.abs(x(brush.to) - x(brush.from))}
                height={innerH}
                fill="var(--color-accent)"
                opacity="0.18"
              />
            )}
          </g>

          {/* Annotation heads sit above the plot so they stay clickable and do
              not collide with the marks. */}
          {inView.map((annotation) => (
            <g key={`head-${annotation.id}`}>
              <title>{`${annotation.label}${annotation.detail ? ` — ${annotation.detail}` : ""}`}</title>
              <path
                d={`M${x(annotation.t)},${padT - 1} l4,-6 l-8,0 Z`}
                fill={annotation.color}
              />
            </g>
          ))}

          {hover !== null && hover >= from && hover <= to && (
            <>
              <line
                x1={x(hover)}
                x2={x(hover)}
                y1={padT}
                y2={padT + innerH}
                stroke="var(--color-line-strong)"
                strokeWidth="1"
              />
              {plotted.map((s) => {
                const point = nearest(s);
                if (!point) return null;
                return (
                  <circle
                    key={s.key}
                    cx={x(ms(point.t))}
                    cy={y(point.v)}
                    r="3.5"
                    fill={s.color}
                    // A 2px ring in the surface colour keeps overlapping
                    // markers from merging into one blob.
                    stroke="var(--color-panel)"
                    strokeWidth="2"
                  />
                );
              })}
            </>
          )}

          <text x={padL} y={height - 5} fontSize="9" fill="var(--color-ink-4)" className="tnum">
            {formatTime(new Date(from).toISOString())}
          </text>
          <text
            x={padL + innerW}
            y={height - 5}
            textAnchor="end"
            fontSize="9"
            fill="var(--color-ink-4)"
            className="tnum"
          >
            {formatTime(new Date(to).toISOString())}
          </text>
        </svg>
      )}

      {hover !== null && hover >= from && hover <= to && (
        <Tooltip x={x(hover)} containerWidth={width}>
          <div className="mb-1 text-2xs text-ink-4">
            {formatTime(new Date(hover).toISOString())}
          </div>
          {plotted.map((s) => {
            const point = nearest(s);
            if (!point) return null;
            return (
              <div key={s.key} className="flex items-center gap-2 whitespace-nowrap">
                <span
                  className="h-1.5 w-1.5 shrink-0 rounded-full"
                  style={{ background: s.color }}
                />
                <span className="max-w-40 truncate text-ink-2">{s.label}</span>
                <span className="num ml-auto text-ink">{formatMetricValue(point.v, unit)}</span>
              </div>
            );
          })}
          {inView
            .filter((a) => Math.abs(x(a.t) - x(hover)) < 6)
            .map((a) => (
              <div
                key={a.id}
                className="mt-1 flex items-center gap-2 border-t border-line pt-1 whitespace-nowrap"
              >
                <span
                  className="h-1.5 w-1.5 shrink-0 rotate-45"
                  style={{ background: a.color }}
                />
                <span className="max-w-44 truncate text-ink-2">{a.label}</span>
              </div>
            ))}
        </Tooltip>
      )}

      {showLegend && plotted.length > 1 && <Legend series={plotted} />}
    </div>
  );
}

export function Legend({ series }: { series: { key: string; label: string; color: string }[] }) {
  return (
    <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
      {series.map((s) => (
        <span key={s.key} className="flex items-center gap-1.5 text-2xs text-ink-3">
          <span className="h-[3px] w-3 shrink-0 rounded-full" style={{ background: s.color }} />
          {s.label}
        </span>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------- histogram

const LEVEL_COLOR: Record<LogLevel, string> = {
  // Ordinal by alarm: the three quiet levels stay neutral so the bulk of a
  // healthy stream recedes, and only warn upward spends a status hue.
  trace: "var(--color-line-strong)",
  debug: "var(--color-lvl-debug)",
  info: "var(--color-lvl-info)",
  warn: "var(--color-warn)",
  error: "var(--color-serious)",
  fatal: "var(--color-crit)",
};

export function levelColor(level: LogLevel): string {
  return LEVEL_COLOR[level];
}

export interface Bucket {
  t: string;
  trace: number;
  debug: number;
  info: number;
  warn: number;
  error: number;
  fatal: number;
  total: number;
}

/**
 * Severity-stacked activity over the buffer. Segments are separated by a 2px
 * gap in the surface colour rather than a stroke, so adjacent counts stay
 * countable without a border darkening the fill.
 *
 * Pass `domain` to place bars by timestamp — that is what lines this up under
 * the metric charts on the service dashboard.
 */
export function LogHistogram({
  buckets,
  height = 96,
  selected,
  onSelect,
  domain,
  hoverTime,
  onHoverTime,
  annotations,
}: {
  buckets: Bucket[];
  height?: number;
  selected?: string | null;
  onSelect?: (bucket: Bucket) => void;
  domain?: Domain;
  hoverTime?: number | null;
  onHoverTime?: (t: number | null) => void;
  annotations?: Annotation[];
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [localHover, setLocalHover] = useState<number | null>(null);

  const controlled = onHoverTime !== undefined;
  const setHover = (t: number | null) => (controlled ? onHoverTime!(t) : setLocalHover(t));

  // Matches the metric charts' gutter so the two line up column for column.
  const padL = domain ? 54 : 0;
  const padR = domain ? 8 : 0;
  const innerW = Math.max(0, width - padL - padR);

  if (buckets.length === 0) {
    return (
      <div
        ref={ref}
        style={{ height }}
        className="flex items-center justify-center text-xs text-ink-4"
      >
        No activity in this window
      </div>
    );
  }

  const max = Math.max(...buckets.map((b) => b.total), 1);
  const order: LogLevel[] = ["fatal", "error", "warn", "info", "debug", "trace"];

  // A bar is as wide as the time it covers. Sizing it by bucket *count*
  // instead looks right at the default window and wrong the moment the domain
  // is narrower than the data — bars stay hairlines with gaps between them
  // rather than widening as you zoom in.
  const domainSpan = domain ? Math.max(1, domain[1] - domain[0]) : 1;
  const bucketMs =
    buckets.length > 1 ? Math.abs(ms(buckets[1].t) - ms(buckets[0].t)) : 0;
  const step = domain
    ? bucketMs > 0
      ? (bucketMs / domainSpan) * innerW
      : innerW / buckets.length
    : width / buckets.length;
  const barW = Math.max(1, step - 2);
  const xOf = (bucket: Bucket, index: number) =>
    domain ? padL + ((ms(bucket.t) - domain[0]) / domainSpan) * innerW : index * step;

  /** Bars outside the visible window are not drawn at all. */
  const visible = domain
    ? buckets.filter((b) => {
        const t = ms(b.t);
        return t >= domain[0] - bucketMs && t <= domain[1] + bucketMs;
      })
    : buckets;

  const hoverIndex = (() => {
    const t = controlled ? hoverTime ?? null : localHover;
    if (t === null) return null;
    if (!domain) return typeof t === "number" && Number.isInteger(t) ? t : null;
    let best: number | null = null;
    let gap = Infinity;
    buckets.forEach((bucket, i) => {
      const d = Math.abs(ms(bucket.t) - t);
      if (d < gap) {
        gap = d;
        best = i;
      }
    });
    // Only report a bucket the crosshair is actually over, so a shared
    // crosshair parked outside this chart's data does not light one up.
    return gap <= Math.max(bucketMs, domainSpan / 40) ? best : null;
  })();

  const inView = (annotations ?? []).filter(
    (a) => !domain || (a.t >= domain[0] && a.t <= domain[1]),
  );

  return (
    <div ref={ref} className="relative" style={{ height }}>
      {width > 0 && (
        <svg width={width} height={height} className="block" onMouseLeave={() => setHover(null)}>
          {inView.map((annotation) => {
            const ax = domain
              ? padL + ((annotation.t - domain[0]) / Math.max(1, domain[1] - domain[0])) * innerW
              : 0;
            return (
              <line
                key={annotation.id}
                x1={ax}
                x2={ax}
                y1={0}
                y2={height}
                stroke={annotation.color}
                strokeWidth="1"
                strokeDasharray="2 3"
                opacity="0.7"
              />
            );
          })}

          {visible.map((bucket, i) => {
            const bx = xOf(bucket, i);
            let cursor = height;
            const active = hoverIndex === i || selected === bucket.t;

            return (
              <g
                key={bucket.t}
                onMouseEnter={() => setHover(domain ? ms(bucket.t) : i)}
                onClick={() => onSelect?.(bucket)}
                className={onSelect ? "cursor-pointer" : undefined}
              >
                {/* Full-height hit target — a 3px bar is not clickable. */}
                <rect
                  x={bx - (step - barW) / 2}
                  y={0}
                  width={step}
                  height={height}
                  fill={active ? "#ffffff0a" : "transparent"}
                />
                {order.map((level) => {
                  const count = bucket[level];
                  if (count <= 0) return null;
                  const h = (count / max) * (height - 2);
                  if (h < 0.5) return null;
                  cursor -= h;
                  return (
                    <rect
                      key={level}
                      x={bx}
                      y={cursor}
                      width={barW}
                      height={Math.max(1, h - 2)}
                      rx={1}
                      fill={LEVEL_COLOR[level]}
                      opacity={active || hoverIndex === null ? 1 : 0.55}
                    />
                  );
                })}
              </g>
            );
          })}
        </svg>
      )}

      {hoverIndex !== null && buckets[hoverIndex] && (
        <Tooltip x={xOf(buckets[hoverIndex], hoverIndex) + step / 2} containerWidth={width}>
          <div className="mb-1 text-2xs text-ink-4">{formatTime(buckets[hoverIndex].t)}</div>
          {order
            .filter((level) => buckets[hoverIndex!][level] > 0)
            .map((level) => (
              <div key={level} className="flex items-center gap-2 whitespace-nowrap">
                <span
                  className="h-1.5 w-1.5 shrink-0 rounded-full"
                  style={{ background: LEVEL_COLOR[level] }}
                />
                <span className="text-ink-2 capitalize">{level}</span>
                <span className="num ml-auto text-ink">{buckets[hoverIndex!][level]}</span>
              </div>
            ))}
          <div className="mt-1 flex gap-2 border-t border-line pt-1 text-2xs">
            <span className="text-ink-4">Total</span>
            <span className="num ml-auto text-ink">{buckets[hoverIndex].total}</span>
          </div>
        </Tooltip>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ tooltip

function Tooltip({
  x,
  containerWidth,
  children,
}: {
  x: number;
  containerWidth: number;
  children: ReactNode;
}) {
  // Flip before the tooltip can leave the panel rather than clipping it.
  const flip = x > containerWidth - 150;
  return (
    <div
      className="pointer-events-none absolute top-1 z-20 min-w-32 rounded-md border border-line-strong bg-overlay px-2 py-1.5 text-xs shadow-lg shadow-black/40"
      style={flip ? { right: containerWidth - x + 8 } : { left: x + 8 }}
    >
      {children}
    </div>
  );
}

// -------------------------------------------------------------------- gauge

/** Proportional bar for a bounded value. Unbounded values render as text only. */
export function Meter({
  value,
  max,
  unit,
  warnAbove,
  width = 44,
}: {
  value: number;
  max?: number;
  unit: MetricUnit;
  warnAbove?: number;
  width?: number;
}) {
  const text = formatMetricValue(value, unit);
  if (max === undefined || max <= 0) {
    return <span className="num">{text}</span>;
  }
  const pct = Math.max(0, Math.min(100, (value / max) * 100));
  const breaching = warnAbove !== undefined && value > warnAbove;

  return (
    <span
      className="inline-flex items-center justify-end gap-2"
      title={`${text} of ${formatMetricValue(max, unit)}`}
    >
      <span className="relative h-1 shrink-0 overflow-hidden rounded-full bg-line" style={{ width }}>
        <span
          className="absolute inset-y-0 left-0 rounded-full"
          style={{
            width: `${pct}%`,
            background: breaching ? "var(--color-warn)" : "var(--color-ink-3)",
          }}
        />
      </span>
      <span className={cx("num", breaching && "text-warn")}>{text}</span>
    </span>
  );
}

/** Relative-time text that stays current without the parent re-rendering. */
export function useTick(ms = 30000): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => setTick((t) => t + 1), ms);
    return () => clearInterval(timer);
  }, [ms]);
  return tick;
}
