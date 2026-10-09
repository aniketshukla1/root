// The arithmetic behind the time-series panel, apart from React, the DOM and
// uPlot, so `scripts/check-plot-math.ts` can run it under plain Node.

/** Inclusive millisecond bounds of a chart's x-axis. */
export type Range = [number, number];

/**
 * The y range a panel draws: what is visible, every limit on it, and room
 * above the highest point so the line never runs along the frame.
 */
export function yRange(min: number | null, max: number | null, limits: number[]): Range {
  let lo = min !== null && Number.isFinite(min) ? min : Infinity;
  let hi = max !== null && Number.isFinite(max) ? max : -Infinity;
  for (const limit of limits) {
    if (limit < lo) lo = limit;
    if (limit > hi) hi = limit;
  }
  if (lo === Infinity) return [0, 1];
  // A flat line still needs a band, and should sit in the middle of it.
  if (lo === hi) return lo === 0 ? [0, 1] : [Math.min(lo * 0.95, lo * 1.05), Math.max(hi * 0.95, hi * 1.05)];
  // Anchor at zero when the data sits close to it, so a small wobble far
  // above the baseline is not drawn as a mountain range.
  if (lo > 0 && lo < (hi - lo) * 1.2) lo = 0;
  const pad = (hi - lo) * 0.08;
  return [lo === 0 ? 0 : lo - pad, hi + pad];
}

/** Tick steps for bytes: round in the 1024-based units the labels print. */
export const BYTE_INCRS: number[] = [0, 1, 2, 3, 4, 5].flatMap((power) =>
  [1, 2, 2.5, 5, 10, 20, 25, 50, 100, 200, 250, 500].map((m) => m * 1024 ** power),
);

/** Tick steps for durations in seconds: round in s, m, h and d. */
export const SECOND_INCRS: number[] = [
  0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5,
  1, 2, 5, 10, 15, 30,
  60, 120, 300, 600, 900, 1800,
  3600, 7200, 10800, 21600, 43200,
  86400, 172800, 432000, 864000, 2592000, 7776000, 31536000,
];

/** Index of the sample nearest `t` in an ascending array, or -1 when empty. */
export function nearestIndex(ts: ArrayLike<number>, t: number): number {
  let lo = 0;
  let hi = ts.length - 1;
  if (hi < 0) return -1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (ts[mid] < t) lo = mid + 1;
    else hi = mid;
  }
  return lo > 0 && t - ts[lo - 1] <= ts[lo] - t ? lo - 1 : lo;
}

export interface Stats {
  min: number;
  max: number;
  avg: number;
  last: number;
}

/** What the legend reports for a series: its samples inside the window. */
export function windowStats(
  ts: ArrayLike<number>,
  vs: ArrayLike<number>,
  [from, to]: Range,
): Stats | null {
  let min = Infinity;
  let max = -Infinity;
  let sum = 0;
  let n = 0;
  let last = 0;
  for (let i = 0; i < ts.length; i += 1) {
    if (ts[i] < from || ts[i] > to) continue;
    const v = vs[i];
    if (!Number.isFinite(v)) continue;
    if (v < min) min = v;
    if (v > max) max = v;
    sum += v;
    n += 1;
    last = v;
  }
  return n === 0 ? null : { min, max, avg: sum / n, last };
}

/**
 * The window after a drag of `dx` pixels across a plot `width` wide, kept
 * inside the data: panning past the newest sample only shows an empty plot.
 * A window that already holds all of it does not move.
 */
export function panRange([from, to]: Range, dx: number, width: number, [lo, hi]: Range): Range {
  const span = to - from;
  if (span >= hi - lo) return [from, to];
  const shift = (dx / Math.max(1, width)) * span;
  const start = Math.min(Math.max(from - shift, lo), hi - span);
  return [start, start + span];
}

/**
 * Which series a legend click leaves visible. A plain click isolates the
 * series, or brings everything back if it already stood alone; a modifier
 * click toggles just that one, as Grafana does. Hiding every series is
 * never the result.
 */
export function legendClick(keys: string[], hidden: Set<string>, key: string, toggle: boolean): Set<string> {
  if (toggle) {
    const next = new Set(hidden);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    return next.size >= keys.length ? new Set() : next;
  }
  const alone = !hidden.has(key) && hidden.size === keys.length - 1;
  return alone ? new Set() : new Set(keys.filter((k) => k !== key));
}

/** `#rgb` or `#rrggbb` at an opacity; anything else is mixed by the browser. */
export function withAlpha(color: string, alpha: number): string {
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(color.trim());
  if (!hex) return `color-mix(in srgb, ${color} ${Math.round(alpha * 100)}%, transparent)`;
  const digits = hex[1].length === 3 ? [...hex[1]].map((c) => c + c).join("") : hex[1];
  const n = parseInt(digits, 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}
