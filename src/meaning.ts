// What a chart says besides its line: the range that is normal for this
// metric and the readings outside it, where its trend is heading and when it
// reaches the limit, and when the connection was in trouble. Apart from React
// so scripts/check-meaning.ts can run it under plain Node.

import type { HealthRun, Point, Trend } from "./types";

/** Readings the normal range is learned from, trailing each point. */
const WINDOW = 30;
/** How many readings it needs before it says anything. */
const MIN_HISTORY = 12;
/** How far outside, in robust standard deviations, counts as unusual. */
const K = 3.5;

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * The normal range, learned from the readings just before each point: their
 * median, give or take K median absolute deviations (scaled to read like a
 * standard deviation). Median and MAD rather than mean and SD, so one spike
 * does not widen the range enough to excuse the next. A reading outside the
 * range it was judged against is an anomaly.
 */
export function normalRange(points: Point[]): { upper: Point[]; lower: Point[]; anomalies: Point[] } {
  const upper: Point[] = [];
  const lower: Point[] = [];
  const anomalies: Point[] = [];
  for (let i = MIN_HISTORY; i < points.length; i += 1) {
    const before = points.slice(Math.max(0, i - WINDOW), i).map((p) => p.v);
    const mid = median(before);
    // A flat series has no spread at all; a floor keeps every wobble from
    // reading as an anomaly against a range of zero width.
    const spread = Math.max(1.4826 * median(before.map((v) => Math.abs(v - mid))), Math.abs(mid) * 0.02, 1e-9);
    const hi = mid + K * spread;
    const lo = mid - K * spread;
    upper.push({ t: points[i].t, v: hi });
    lower.push({ t: points[i].t, v: lo });
    if (points[i].v > hi || points[i].v < lo) anomalies.push(points[i]);
  }
  return { upper, lower, anomalies };
}

/**
 * Where the trend goes from the last reading: along its slope until it meets
 * the limit, or `horizonMs` ahead if that comes first, with a cone that
 * widens the worse a straight line fits. Nothing when it is not heading for
 * the limit, or the fit is too poor to draw a direction from.
 */
export function forecastCone(
  points: Point[],
  trend: Trend | undefined,
  limit: number | undefined,
  horizonMs: number,
): { center: Point[]; upper: Point[]; lower: Point[]; hitsAt: number } | null {
  if (!trend || trend.secondsToLimit === undefined || limit === undefined || points.length < 2 || trend.fit < 0.3) return null;
  const last = points[points.length - 1];
  const t0 = Date.parse(last.t);
  const hitsAt = t0 + trend.secondsToLimit * 1000;
  const end = Math.min(hitsAt, t0 + horizonMs);
  if (end <= t0) return null;
  const center: Point[] = [];
  const upper: Point[] = [];
  const lower: Point[] = [];
  const steps = 16;
  for (let s = 0; s <= steps; s += 1) {
    const t = t0 + ((end - t0) * s) / steps;
    const elapsed = (t - t0) / 1000;
    const v = last.v + trend.perSecond * elapsed;
    const half = (1 - trend.fit) * Math.abs(trend.perSecond) * elapsed * 1.5;
    const at = new Date(t).toISOString();
    center.push({ t: at, v });
    upper.push({ t: at, v: v + half });
    lower.push({ t: at, v: v - half });
  }
  return { center, upper, lower, hitsAt };
}

export interface Region {
  id: string;
  from: number;
  to: number;
  color: string;
  label: string;
}

/** The stretches a connection was down or degraded, clipped to the window. */
export function troubleRegions(runs: HealthRun[], [from, to]: [number, number]): Region[] {
  return runs
    .filter((r) => r.health === "down" || r.health === "degraded")
    .map((r) => ({ run: r, start: Math.max(from, Date.parse(r.from)), end: Math.min(to, Date.parse(r.to)) }))
    .filter(({ start, end }) => end > start)
    .map(({ run, start, end }) => ({
      id: `${run.health}:${run.from}`,
      from: start,
      to: end,
      color: run.health === "down" ? "var(--color-crit)" : "var(--color-warn)",
      label: run.health === "down" ? "Down" : "Degraded",
    }));
}
