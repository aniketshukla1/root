// Run: node --experimental-strip-types scripts/check-histogram.ts
import assert from "node:assert/strict";

import { bucketsFrom, cells, leOf, quantile } from "../src/histogram.ts";

assert.equal(leOf('{le="0.25",job="api"}'), 0.25);
assert.equal(leOf('{job="api",le="+Inf"}'), Infinity);
assert.equal(leOf('{job="api"}'), null);

// 100 requests: 50 under 0.1s, 40 more under 0.5s, 9 more under 1s, 1 slower.
const t = "2026-10-16T12:00:00Z";
const later = "2026-10-16T12:01:00Z";
const series = (le: string, a: number, b: number) => ({ label: `{le="${le}"}`, points: [{ t, v: a }, { t: later, v: b }] });
const buckets = bucketsFrom([series("+Inf", 100, 10), series("0.5", 90, 10), series("0.1", 50, 10), series("1", 99, 10), { label: "{}", points: [] }]);
assert.deepEqual(buckets.map((b) => b.le), [0.1, 0.5, 1, Infinity], "sorted by bound, the label-less series left out");

const [p50] = quantile(0.5, buckets);
assert.equal(p50.v, 0.1, "rank 50 lands exactly on the first bucket's edge");
const [p90] = quantile(0.9, buckets);
assert.equal(p90.v, 0.5);
const [p95] = quantile(0.95, buckets);
assert.ok(Math.abs(p95.v - (0.5 + 0.5 * (5 / 9))) < 1e-9, `interpolated inside (0.5, 1]: ${p95.v}`);
const [p999] = quantile(0.999, buckets);
assert.equal(p999.v, 1, "a rank only +Inf reaches answers the highest finite bound");
const second = quantile(0.5, buckets)[1];
assert.equal(second.v, 0.05, "every observation in the first bucket: halfway into (0, 0.1]");

const grid = cells(buckets);
assert.deepEqual(grid.counts[0], [50, 40, 9, 1]);
assert.deepEqual(grid.counts[1], [10, 0, 0, 0]);
assert.equal(grid.times.length, 2);

console.log("histogram: ok");
