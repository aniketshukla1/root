// Prometheus histograms as people read them: a quantile line from bucket
// counts, computed the way `histogram_quantile` computes one, and per-bucket
// counts for a heatmap. Apart from React so scripts/check-histogram.ts can
// run it under plain Node.

import type { Point } from "./types";

/** One `_bucket` series: cumulative counts of observations at or under `le`. */
export interface Bucket {
  le: number;
  points: Point[];
}

/** The `le` of a rendered label set: `{le="0.25",…}` → 0.25, `+Inf` → Infinity. */
export function leOf(labels: string): number | null {
  const found = /\ble="([^"]+)"/.exec(labels);
  if (!found) return null;
  if (found[1] === "+Inf") return Infinity;
  const value = Number(found[1]);
  return Number.isFinite(value) ? value : null;
}

/** The bucket series in a query's answer, by bound; anything without `le` is left out. */
export function bucketsFrom(series: { label: string; points: Point[] }[]): Bucket[] {
  return series
    .map((s) => ({ le: leOf(s.label), points: s.points }))
    .filter((b): b is Bucket => b.le !== null)
    .sort((a, b) => a.le - b.le);
}

/** Cumulative counts per timestamp, every bucket present, oldest first. */
function rows(buckets: Bucket[]): [string, number[]][] {
  const byTime = new Map<string, number[]>();
  buckets.forEach((bucket, i) => {
    for (const p of bucket.points) {
      let counts = byTime.get(p.t);
      if (!counts) byTime.set(p.t, (counts = new Array<number>(buckets.length).fill(Number.NaN)));
      counts[i] = p.v;
    }
  });
  return [...byTime]
    .filter(([, counts]) => counts.every((c) => !Number.isNaN(c)))
    .sort((a, b) => Date.parse(a[0]) - Date.parse(b[0]));
}

/**
 * One quantile over time. The rank `q × total` is found in the first bucket
 * whose cumulative count reaches it, and placed linearly inside that bucket;
 * a rank that only `+Inf` reaches answers the highest finite bound, as
 * Prometheus does. The lowest bucket starts at zero.
 */
export function quantile(q: number, buckets: Bucket[]): Point[] {
  if (buckets.length < 2) return [];
  const out: Point[] = [];
  for (const [t, counts] of rows(buckets)) {
    const total = counts[counts.length - 1];
    if (!(total > 0)) continue;
    const rank = q * total;
    const i = counts.findIndex((c) => c >= rank);
    if (i < 0) continue;
    const upper = buckets[i].le;
    if (!Number.isFinite(upper)) {
      out.push({ t, v: buckets[i - 1]?.le ?? 0 });
      continue;
    }
    const lower = i === 0 ? 0 : buckets[i - 1].le;
    const below = i === 0 ? 0 : counts[i - 1];
    const inside = counts[i] - below;
    out.push({ t, v: inside > 0 ? lower + (upper - lower) * ((rank - below) / inside) : upper });
  }
  return out;
}

/** Observations per bucket — not cumulative — at each timestamp, for a heatmap. */
export function cells(buckets: Bucket[]): { times: number[]; bounds: number[]; counts: number[][] } {
  const table = rows(buckets);
  return {
    times: table.map(([t]) => Date.parse(t)),
    bounds: buckets.map((b) => b.le),
    // A counter reset can make a higher bucket read lower for one sample;
    // a negative count of requests is not a thing to colour.
    counts: table.map(([, cumulative]) => cumulative.map((c, i) => Math.max(0, i === 0 ? c : c - cumulative[i - 1]))),
  };
}
