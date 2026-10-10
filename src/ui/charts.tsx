// Time series draw through uPlot; the small forms (sparklines, the log
// histogram, meters) stay hand-drawn SVG. Either way the constraints are the
// same: thin recessive marks, a hover layer on everything that plots, one
// y-axis per chart (two units means two charts, never two scales), and status
// colours that never double as series identity.
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
  useRef,
  useState,
  type ReactNode,
} from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";

import { formatMetricValue, formatTime } from "../format";
import type { LogLevel, MetricUnit, Point } from "../types";
import {
  BYTE_INCRS,
  legendClick,
  nearestIndex,
  panRange,
  SECOND_INCRS,
  windowStats,
  withAlpha,
  yRange,
} from "./plot-math";
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
          <stop offset="0%" style={{ stopColor: stroke, stopOpacity: 0.22 }} />
          <stop offset="100%" style={{ stopColor: stroke, stopOpacity: 0 }} />
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
      {/* The glow: the same line, wide and faint, under the line itself. */}
      <path
        d={line}
        fill="none"
        stroke={stroke}
        strokeWidth="4.5"
        className={breaching ? "chart-glow-strong" : "chart-glow"}
        strokeLinejoin="round"
        strokeLinecap="round"
      />
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
  /** Shown before the label in the legend, such as the service's mark. */
  icon?: ReactNode;
  /** A reference line rather than a reading — the same query a day earlier. */
  dashed?: boolean;
}

/** A series' samples as numbers, parsed once per points array however often
 *  the panel re-renders — a shared crosshair re-renders every panel on every
 *  mouse move, and a 100,000-point series must not be re-parsed each time. */
interface Column {
  ts: Float64Array;
  vs: Float64Array;
}

const parsed = new WeakMap<Point[], Column>();

function columnOf(points: Point[]): Column {
  let column = parsed.get(points);
  if (!column) {
    const ts = new Float64Array(points.length);
    const vs = new Float64Array(points.length);
    for (let i = 0; i < points.length; i += 1) {
      ts[i] = ms(points[i].t);
      vs[i] = points[i].v;
    }
    column = { ts, vs };
    parsed.set(points, column);
  }
  return column;
}

/** uPlot's data: one shared time column, then each series twice — its glow,
 *  then itself — from the same array. Series sampled at different moments
 *  are joined on the union of their timestamps and drawn across the gaps. */
function align(columns: Column[]): uPlot.AlignedData {
  const joined: uPlot.AlignedData =
    columns.length === 1
      ? [columns[0].ts, columns[0].vs]
      : uPlot.join(columns.map((c) => [c.ts, c.vs] as uPlot.AlignedData));
  const [x, ...ys] = joined;
  return [x, ...ys.flatMap((y) => [y, y])] as uPlot.AlignedData;
}

/** A CSS colour as canvas takes it: a custom property is read off `el`. */
function resolveColor(el: Element, color: string): string {
  const custom = /^var\((--[\w-]+)\)$/.exec(color.trim());
  if (!custom) return color;
  return getComputedStyle(el).getPropertyValue(custom[1]).trim() || color;
}

/** Bumps when the theme flips, so canvas colours are read again. */
function useThemeTick(): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const observer = new MutationObserver(() => setTick((t) => t + 1));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => observer.disconnect();
  }, []);
  return tick;
}

/** Past a limit the line turns: one gradient with a hard stop at the limit,
 *  so only the stretch that broke it changes colour. */
function limitStroke(u: uPlot, limit: number | undefined, below: string, above: string) {
  if (limit === undefined) return below;
  const { top, height } = u.bbox;
  const y = u.valToPos(limit, "y", true);
  if (!Number.isFinite(y) || y <= top) return below;
  if (y >= top + height) return above;
  const stop = (y - top) / height;
  const gradient = u.ctx.createLinearGradient(0, top, 0, top + height);
  gradient.addColorStop(0, above);
  gradient.addColorStop(stop, above);
  gradient.addColorStop(stop, below);
  gradient.addColorStop(1, below);
  return gradient;
}

const clockFormat = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
const secondsFormat = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
});
const dayFormat = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });

/** Time ticks: the clock, and the date wherever a tick crosses midnight. A
 *  label centred on a tick at the plot's right edge would be cut in half, so
 *  that one goes and its gridline stays. */
function timeTicks(u: uPlot, splits: number[], incr: number): (string | null)[] {
  const right = u.bbox.width / uPlot.pxRatio;
  return splits.map((t, i) => {
    if (u.valToPos(t, "x") > right - 16) return null;
    if (incr >= 86_400_000) return dayFormat.format(t);
    if (i > 0 && new Date(t).getDate() !== new Date(splits[i - 1]).getDate()) return dayFormat.format(t);
    return (incr < 60_000 ? secondsFormat : clockFormat).format(t);
  });
}

/**
 * Multi-series line chart on an explicit time domain, drawn by uPlot — the
 * engine inside Grafana's time-series panel — so round ticks, time ticks and a
 * 100,000-point series come from code built for them.
 *
 * Every series on one chart must share a unit — the caller facets by unit
 * before it gets here. A second y-scale is never an option: it lets any two
 * shapes be made to look correlated.
 *
 * `hoverTime` / `onHoverTime` are controlled, so a parent can drive one
 * crosshair across a whole page of panels; leave them out and the chart
 * hovers on its own. Drag to zoom: with `onBrush` the parent owns the window,
 * without it the chart zooms itself and a double-click zooms back out.
 * Shift-drag, or a sideways scroll, pans.
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
  legendExtra,
  axisWidth,
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
  /** One more legend column after the stats, for what only the caller knows. */
  legendExtra?: { label: string; render: (series: Series) => ReactNode };
  /** A fixed value-axis width, so charts stacked above each other share one time axis. */
  axisWidth?: number;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const host = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);
  const theme = useThemeTick();
  const [localHover, setLocalHover] = useState<number | null>(null);
  const [zoom, setZoom] = useState<Domain | null>(null);
  const [hidden, setHidden] = useState<Set<string>>(() => new Set());
  // Whether the pointer is over this panel, as opposed to a crosshair synced
  // from another one: only the panel being read shows the tooltip.
  const [pointer, setPointer] = useState(false);

  const controlled = onHoverTime !== undefined;
  const hover = controlled ? (hoverTime ?? null) : localHover;
  const view: Domain = onBrush ? domain : (zoom ?? domain);

  const plotted = series.filter((s) => s.points.length > 1);
  const columns = plotted.map((s) => columnOf(s.points));
  const single = plotted.length === 1;

  // New data only when a points array actually changed, not on every render.
  const aligned = useRef<{ columns: Column[]; data: uPlot.AlignedData } | null>(null);
  if (
    columns.length > 0 &&
    (!aligned.current ||
      aligned.current.columns.length !== columns.length ||
      aligned.current.columns.some((c, i) => c !== columns[i]))
  ) {
    aligned.current = { columns, data: align(columns) };
  }
  const data = aligned.current?.data;

  // What the drawing is made of, as opposed to the data under it.
  const shape = [
    height,
    unit,
    theme,
    plotted.map((s) => `${s.key}|${s.color}|${s.warnAbove ?? ""}|${s.dashed ? "d" : ""}`).join(","),
    (annotations?.length ?? 0) > 0,
    axisWidth ?? "",
  ].join("/");

  // Where there is data at all: panning stops at its edges.
  const bounds: Domain =
    columns.length > 0
      ? [
          Math.min(...columns.map((c) => c.ts[0])),
          Math.max(...columns.map((c) => c.ts[c.ts.length - 1])),
        ]
      : view;

  // Latest values for the callbacks uPlot holds on to.
  const live = useRef({
    view,
    bounds,
    width,
    data,
    annotations,
    syncing: false,
    hover: (t: number | null) => (controlled ? onHoverTime!(t) : setLocalHover(t)),
    setWindow: (range: Domain) => (onBrush ? onBrush(range) : setZoom(range)),
    resetZoom: () => setZoom(null),
    setPointer,
  });
  live.current = {
    ...live.current,
    view,
    bounds,
    width,
    data,
    annotations,
    hover: (t) => (controlled ? onHoverTime!(t) : setLocalHover(t)),
    setWindow: (range) => (onBrush ? onBrush(range) : setZoom(range)),
  };

  const ready = width > 0 && plotted.length > 0;

  useEffect(() => {
    const el = host.current;
    const initial = live.current.data;
    if (!ready || !el || !initial) return;

    const css = (color: string) => resolveColor(el, color);
    const ink = css("var(--color-ink-4)");
    const grid = css("var(--color-line)");
    const warn = css("var(--color-warn)");
    const crit = css("var(--color-crit)");
    const mono = getComputedStyle(el).getPropertyValue("--font-mono").trim() || "monospace";
    const font = `10px ${mono}`;
    const light = document.documentElement.dataset.theme === "light";
    const glow = light ? 0.06 : 0.14;
    const glowOver = light ? 0.14 : 0.28;
    const px = uPlot.pxRatio;
    const limits = [
      ...new Set(plotted.map((s) => s.warnAbove).filter((v): v is number => v !== undefined)),
    ];

    const lines: uPlot.Series[] = [{}];
    for (const s of plotted) {
      const color = css(s.color);
      // The glow: the same line, wide and faint, under it — and the area
      // wash rides on it so the fill never covers the glow.
      lines.push({
        scale: "y",
        spanGaps: true,
        points: { show: false },
        width: 6,
        stroke: (u) =>
          limitStroke(u, s.warnAbove, withAlpha(color, s.dashed ? 0 : glow), withAlpha(warn, s.dashed ? 0 : glowOver)),
        ...(single
          ? {
              fill: (u: uPlot) => {
                const { top, height: h } = u.bbox;
                const wash = u.ctx.createLinearGradient(0, top, 0, top + h);
                wash.addColorStop(0, withAlpha(color, 0.2));
                wash.addColorStop(1, withAlpha(color, 0));
                return wash;
              },
            }
          : {}),
      });
      lines.push({
        label: s.label,
        scale: "y",
        spanGaps: true,
        points: { show: false },
        width: 2,
        ...(s.dashed ? { dash: [5, 4] } : {}),
        stroke: (u) => limitStroke(u, s.warnAbove, s.dashed ? withAlpha(color, 0.6) : color, warn),
      });
    }

    // One window change a frame, and none when the data's edge leaves
    // nowhere to go.
    let frame = 0;
    let pending: Domain | null = null;
    const panTo = (u: uPlot, next: Domain) => {
      if (next[0] === u.scales.x.min && next[1] === u.scales.x.max) return;
      pending = next;
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        if (pending) live.current.setWindow(pending);
      });
    };

    const startPan = (u: uPlot, e: MouseEvent) => {
      const startX = e.clientX;
      const start: Domain = [u.scales.x.min ?? view[0], u.scales.x.max ?? view[1]];
      const move = (ev: MouseEvent) =>
        panTo(u, panRange(start, ev.clientX - startX, u.over.clientWidth, live.current.bounds));
      const up = () => {
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    };

    const opts: uPlot.Options = {
      width: live.current.width,
      height,
      ms: 1,
      padding: [(annotations?.length ?? 0) > 0 ? 12 : 8, 8, 0, 0],
      legend: { show: false },
      cursor: {
        x: true,
        y: false,
        points: { show: false },
        drag: { x: true, y: false, setScale: false, dist: 8 },
        bind: {
          mousedown: (u, _target, handler) => (e) => {
            if (e.shiftKey) {
              startPan(u, e);
              return null;
            }
            return handler(e);
          },
          dblclick: () => () => {
            live.current.resetZoom();
            return null;
          },
        },
      },
      scales: {
        x: { time: true },
        y: { range: (_u, min, max) => yRange(min, max, limits) },
      },
      axes: [
        {
          font,
          stroke: ink,
          grid: { stroke: grid, width: 1 },
          ticks: { show: false },
          size: 20,
          gap: 4,
          space: 72,
          values: (u, splits, _axis, _space, incr) => timeTicks(u, splits, incr),
        },
        {
          font,
          stroke: ink,
          grid: { stroke: grid, width: 1 },
          ticks: { show: false },
          gap: 6,
          // About four ticks whatever the panel's height: a fixed spacing
          // left a 104px card with a single label.
          space: (_u, _axis, _min, _max, dim) => Math.max(18, dim / 4),
          // Wide enough for the longest label at this size, and no wider.
          size:
            axisWidth ??
            ((_u, values) => (values ? Math.max(...values.map((v) => String(v).length)) * 6.2 + 14 : 40)),
          ...(unit === "bytes" || unit === "bytes_per_second"
            ? { incrs: BYTE_INCRS }
            : unit === "seconds"
              ? { incrs: SECOND_INCRS }
              : {}),
          values: (_u, splits) => splits.map((v) => formatMetricValue(v, unit)),
        },
      ],
      series: lines,
      hooks: {
        // Under the lines: the zone past each limit, the limit itself, and
        // a dashed line at each annotated moment.
        drawAxes: [
          (u) => {
            const { ctx } = u;
            const { left, top, width: w, height: h } = u.bbox;
            ctx.save();
            for (const limit of limits) {
              const y = Math.round(u.valToPos(limit, "y", true));
              if (y < top || y > top + h) continue;
              const zone = ctx.createLinearGradient(0, top, 0, y);
              zone.addColorStop(0, withAlpha(crit, 0.17));
              zone.addColorStop(1, withAlpha(crit, 0.02));
              ctx.fillStyle = zone;
              ctx.fillRect(left, top, w, y - top);
              ctx.strokeStyle = withAlpha(warn, 0.55);
              ctx.lineWidth = px;
              ctx.setLineDash([3 * px, 3 * px]);
              ctx.beginPath();
              ctx.moveTo(left, y + 0.5);
              ctx.lineTo(left + w, y + 0.5);
              ctx.stroke();
            }
            for (const note of live.current.annotations ?? []) {
              const x = Math.round(u.valToPos(note.t, "x", true));
              if (x < left || x > left + w) continue;
              ctx.strokeStyle = withAlpha(css(note.color), 0.75);
              ctx.lineWidth = px;
              ctx.setLineDash([2 * px, 3 * px]);
              ctx.beginPath();
              ctx.moveTo(x + 0.5, top);
              ctx.lineTo(x + 0.5, top + h);
              ctx.stroke();
            }
            ctx.restore();
          },
        ],
        // Annotation heads sit above the plot so they never cover a mark.
        draw: [
          (u) => {
            const { ctx } = u;
            const { left, top, width: w } = u.bbox;
            for (const note of live.current.annotations ?? []) {
              const x = u.valToPos(note.t, "x", true);
              if (x < left || x > left + w) continue;
              ctx.fillStyle = css(note.color);
              ctx.beginPath();
              ctx.moveTo(x, top - px);
              ctx.lineTo(x + 4 * px, top - 7 * px);
              ctx.lineTo(x - 4 * px, top - 7 * px);
              ctx.closePath();
              ctx.fill();
            }
          },
        ],
        setCursor: [
          (u) => {
            if (live.current.syncing) return;
            const left = u.cursor.left ?? -1;
            live.current.setPointer(left >= 0);
            live.current.hover(left < 0 ? null : u.posToVal(left, "x"));
          },
        ],
        setSelect: [
          (u) => {
            const { left, width: w } = u.select;
            if (w <= 0) return;
            const range: Domain = [u.posToVal(left, "x"), u.posToVal(left + w, "x")];
            u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
            live.current.setWindow(range);
          },
        ],
      },
    };

    const u = new uPlot(opts, initial, el);
    u.setScale("x", { min: live.current.view[0], max: live.current.view[1] });

    // A sideways scroll pans; an up-and-down one still scrolls the page.
    const onWheel = (e: WheelEvent) => {
      if (Math.abs(e.deltaX) <= Math.abs(e.deltaY)) return;
      e.preventDefault();
      const min = u.scales.x.min ?? live.current.view[0];
      const max = u.scales.x.max ?? live.current.view[1];
      panTo(u, panRange([min, max], -e.deltaX, u.over.clientWidth, live.current.bounds));
    };
    u.over.addEventListener("wheel", onWheel, { passive: false });

    plot.current = u;
    return () => {
      cancelAnimationFrame(frame);
      u.over.removeEventListener("wheel", onWheel);
      u.destroy();
      plot.current = null;
    };
    // `shape` stands for everything the drawing is built from.
  }, [shape, ready]);

  useEffect(() => {
    const u = plot.current;
    if (!u || !data) return;
    u.setData(data, false);
    u.setScale("x", { min: view[0], max: view[1] });
  }, [data]);

  useEffect(() => {
    plot.current?.setScale("x", { min: view[0], max: view[1] });
  }, [view[0], view[1]]);

  useEffect(() => {
    if (width > 0) plot.current?.setSize({ width, height });
  }, [width, height]);

  useEffect(() => {
    plot.current?.redraw(false, false);
  }, [annotations]);

  useEffect(() => {
    const u = plot.current;
    if (!u) return;
    plotted.forEach((s, i) => {
      const show = !hidden.has(s.key);
      u.setSeries(1 + i * 2, { show });
      u.setSeries(2 + i * 2, { show });
    });
  }, [hidden, shape]);

  // Another panel moved the shared crosshair: put this one's on the same moment.
  useEffect(() => {
    const u = plot.current;
    if (!u) return;
    const current = u.cursor.left ?? -10;
    if (hover === null) {
      if (current < 0) return;
    } else if (Math.abs(current - u.valToPos(hover, "x")) < 1) {
      return;
    }
    live.current.syncing = true;
    u.setCursor({
      left: hover === null ? -10 : u.valToPos(hover, "x"),
      top: hover === null ? -10 : u.over.clientHeight / 2,
    });
    live.current.syncing = false;
  }, [hover]);

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

  const u = plot.current;
  const xMin = u?.scales.x.min ?? view[0];
  const xMax = u?.scales.x.max ?? view[1];
  const showHover = u !== null && hover !== null && hover >= xMin && hover <= xMax;
  const offsetLeft = u ? u.bbox.left / uPlot.pxRatio : 0;
  const offsetTop = u ? u.bbox.top / uPlot.pxRatio : 0;

  // Nearest real sample per series, within a few pixels' worth of time.
  const readings =
    showHover && u
      ? plotted.flatMap((s, i) => {
          if (hidden.has(s.key)) return [];
          const { ts, vs } = columns[i];
          const k = nearestIndex(ts, hover);
          const tolerance = ((xMax - xMin) / Math.max(1, u.over.clientWidth)) * 6;
          if (k < 0 || Math.abs(ts[k] - hover) > tolerance) return [];
          return [
            {
              series: s,
              value: vs[k],
              x: offsetLeft + u.valToPos(ts[k], "x"),
              y: offsetTop + u.valToPos(vs[k], "y"),
            },
          ];
        })
      : [];
  const hoverX = showHover && u ? offsetLeft + u.valToPos(hover, "x") : 0;
  const notesNear =
    showHover && u
      ? (annotations ?? []).filter((a) => Math.abs(offsetLeft + u.valToPos(a.t, "x") - hoverX) < 6)
      : [];
  const keys = plotted.map((s) => s.key);

  return (
    <div ref={ref} className="relative">
      <div ref={host} className={cx("uplot-host", onBrush && "select-none")} style={{ height }} />

      {zoom && !onBrush && (
        <button
          type="button"
          onClick={() => setZoom(null)}
          className="absolute top-0 right-1 z-10 rounded border border-line bg-panel px-1.5 py-0.5 text-2xs text-ink-3 hover:text-ink"
        >
          Zoom out
        </button>
      )}

      {readings.map((r) => (
        <span
          key={r.series.key}
          className="pointer-events-none absolute h-2 w-2 -translate-x-1/2 -translate-y-1/2 rounded-full"
          // A 2px ring in the surface colour keeps a marker legible where it
          // crosses another line.
          style={{
            left: r.x,
            top: r.y,
            background: r.series.color,
            boxShadow: "0 0 0 2px var(--color-panel)",
          }}
        />
      ))}

      {showHover && (pointer || !controlled) && (
        <Tooltip x={hoverX} containerWidth={width}>
          <div className="mb-1 text-2xs text-ink-4">{formatTime(new Date(hover).toISOString())}</div>
          {readings.map((r) => (
            <div key={r.series.key} className="flex items-center gap-2 whitespace-nowrap">
              <span className="h-[2px] w-2.5 shrink-0 rounded-full" style={{ background: r.series.color }} />
              <span className="num text-ink">{formatMetricValue(r.value, unit)}</span>
              {!single && <span className="max-w-40 truncate text-ink-3">{r.series.label}</span>}
            </div>
          ))}
          {notesNear.map((a) => (
            <div
              key={a.id}
              className="mt-1 flex items-center gap-2 border-t border-line pt-1 whitespace-nowrap"
            >
              <span className="h-1.5 w-1.5 shrink-0 rotate-45" style={{ background: a.color }} />
              <span className="max-w-44 truncate text-ink-2">{a.label}</span>
            </div>
          ))}
        </Tooltip>
      )}

      {showLegend && plotted.length > 1 && (
        <table className="mt-2 w-full table-fixed text-2xs">
          <thead>
            <tr className="text-ink-4">
              <th className="text-left font-normal">
                <span className="sr-only">Series</span>
              </th>
              {["Min", "Max", "Avg", "Last"].map((h) => (
                <th key={h} className="w-16 text-right font-normal">
                  {h}
                </th>
              ))}
              {legendExtra && <th className="w-[72px] text-right font-normal">{legendExtra.label}</th>}
            </tr>
          </thead>
          <tbody>
            {plotted.map((s, i) => {
              const stats = windowStats(columns[i].ts, columns[i].vs, [xMin, xMax]);
              const off = hidden.has(s.key);
              const over = stats !== null && s.warnAbove !== undefined && stats.last > s.warnAbove;
              return (
                <tr
                  key={s.key}
                  onClick={(e) => setHidden((h) => legendClick(keys, h, s.key, e.metaKey || e.ctrlKey))}
                  className={cx("cursor-pointer hover:bg-raised", off && "opacity-40")}
                  title="Click to show only this series; ⌘-click to toggle it"
                >
                  <td className="py-0.5 pr-2">
                    <button
                      type="button"
                      aria-pressed={!off}
                      className="flex max-w-full items-center gap-1.5 text-ink-2"
                    >
                      <span className="h-[2px] w-3 shrink-0 rounded-full" style={{ background: s.color }} />
                      {s.icon}
                      <span className="truncate">{s.label}</span>
                    </button>
                  </td>
                  {(["min", "max", "avg", "last"] as const).map((k) => (
                    <td
                      key={k}
                      className={cx("num text-right", k === "last" && over ? "text-warn" : "text-ink-3")}
                    >
                      {stats ? formatMetricValue(stats[k], unit) : "—"}
                    </td>
                  ))}
                  {legendExtra && <td className="num text-right">{legendExtra.render(s)}</td>}
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

/**
 * Why a series is not worth a chart in this window, if it is not: it never
 * moved, or it is a clock — an uptime grows exactly as fast as time passes.
 * Either says one number, and a flat line or a perfect diagonal spends a
 * whole panel saying it.
 */
export function staticKind(series: {
  unit: MetricUnit;
  points: { t: string; v: number }[];
}): "steady" | "clock" | null {
  const { points } = series;
  if (points.length < 2) return null;
  const first = points[0];
  const last = points[points.length - 1];
  if (points.every((p) => p.v === first.v)) return "steady";
  if (series.unit === "seconds") {
    const elapsed = (Date.parse(last.t) - Date.parse(first.t)) / 1000;
    if (elapsed > 0 && Math.abs(last.v - first.v - elapsed) <= elapsed * 0.02) return "clock";
  }
  return null;
}

/** The series `staticKind` takes out of the chart grid, as one line of values. */
export function SteadyStats({
  series,
}: {
  series: { key: string; label: string; unit: MetricUnit; latest: number }[];
}) {
  if (series.length === 0) return null;
  return (
    <div className="flex flex-wrap items-baseline gap-x-5 gap-y-1.5 rounded-md border border-line bg-page px-3 py-2">
      <span className="text-2xs text-ink-4">Steady in this window</span>
      {series.map((s) => (
        <span key={s.key} className="flex items-baseline gap-1.5">
          <span className="text-xs text-ink-3">{s.label}</span>
          <span className="num text-sm text-ink">{formatMetricValue(s.latest, s.unit)}</span>
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

/**
 * The same ordering, for text rather than for a mark.
 *
 * `trace` and `debug` are deliberately the quietest thing on the screen, and
 * as a swatch — a filled square beside a label — `--color-line-strong` is
 * exactly right. As *text* it measures 1.57:1 against the panel, which is not
 * quiet but unreadable: the level is the first thing anyone scans a log line
 * for, and a level nobody can read is a column of blanks.
 *
 * The same distinction `incident.tsx` makes about the cell tokens. A colour
 * that works as a ground does not automatically work as a mark, and the
 * failure is silent in both directions.
 */
export function levelTextColor(level: LogLevel): string {
  return level === "trace" || level === "debug"
    ? "var(--color-ink-4)"
    : LEVEL_COLOR[level];
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

  /**
   * The stretch of the window the buffer never covered.
   *
   * The log ring holds what it holds, so a one-hour view of a twenty-minute
   * buffer is two thirds empty — and empty, on a chart, reads as "nothing
   * happened here" rather than "nothing was recorded here". Those are opposite
   * findings, so the gap is shaded and named instead of left blank.
   */
  const firstAt = domain ? Math.min(...buckets.map((b) => ms(b.t))) : undefined;
  const blankTo =
    domain && firstAt !== undefined && firstAt - bucketMs > domain[0]
      ? padL + ((firstAt - bucketMs - domain[0]) / domainSpan) * innerW
      : undefined;

  return (
    <div ref={ref} className="relative" style={{ height }}>
      {width > 0 && (
        <svg width={width} height={height} className="block" onMouseLeave={() => setHover(null)}>
          {blankTo !== undefined && blankTo > padL + 24 && (
            <g>
              <rect
                x={padL}
                y={0}
                width={blankTo - padL}
                height={height}
                fill="var(--color-cell-todo)"
                opacity="0.6"
              />
              <line
                x1={blankTo}
                x2={blankTo}
                y1={0}
                y2={height}
                stroke="var(--color-line-strong)"
                strokeWidth="1"
              />
              <text
                x={padL + 6}
                y={height / 2}
                dominantBaseline="middle"
                className="num"
                fontSize="10.5"
                fill="var(--color-ink-4)"
              >
                no lines buffered before {formatTime(new Date(firstAt!).toISOString())}
              </text>
            </g>
          )}

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
                  fill={active ? "var(--color-wash)" : "transparent"}
                />
                {order.map((level) => {
                  const count = bucket[level];
                  if (count <= 0) return null;
                  const h = (count / max) * (height - 2);
                  if (h < 0.5) return null;
                  cursor -= h;
                  // Errors and warnings glow — the hue the rest of root gives
                  // a failing thing — as a soft halo behind the segment.
                  const halo =
                    level === "error" || level === "fatal" ? 0.3 : level === "warn" ? 0.16 : 0;
                  return (
                    <g key={level} opacity={active || hoverIndex === null ? 1 : 0.55}>
                      {halo > 0 && (
                        <rect
                          x={bx - 1.5}
                          y={cursor - 1}
                          width={barW + 3}
                          height={Math.max(1, h - 2) + 2}
                          rx={2.5}
                          fill={LEVEL_COLOR[level]}
                          opacity={halo}
                        />
                      )}
                      <rect
                        x={bx}
                        y={cursor}
                        width={barW}
                        height={Math.max(1, h - 2)}
                        rx={1}
                        fill={LEVEL_COLOR[level]}
                      />
                    </g>
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
      className="pointer-events-none absolute top-1 z-20 min-w-32 rounded-md border border-line-strong bg-overlay px-2 py-1.5 text-xs overlay-plate"
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
