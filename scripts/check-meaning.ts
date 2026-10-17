// Run: node --experimental-strip-types scripts/check-meaning.ts
import assert from "node:assert/strict";

import { forecastCone, normalRange, troubleRegions } from "../src/meaning.ts";

const t0 = Date.UTC(2026, 9, 17, 12);
const at = (minute: number) => new Date(t0 + minute * 60_000).toISOString();

// Forty readings wobbling around 50, one spike to 90 at minute 30.
const readings = Array.from({ length: 40 }, (_, i) => ({ t: at(i), v: i === 30 ? 90 : 50 + ((i * 7) % 5) - 2 }));
const normal = normalRange(readings);
assert.equal(normal.upper.length, 40 - 12, "nothing said before twelve readings of history");
assert.deepEqual(normal.anomalies.map((p) => p.v), [90], "the spike, and only the spike");
const after = normal.upper.find((p) => p.t === at(31))!;
assert.ok(after.v < 60, `one spike does not widen the range that judges the next reading: ${after.v}`);

// A flat line still gets a range, and a small wobble on it is not an anomaly.
const flat = Array.from({ length: 20 }, (_, i) => ({ t: at(i), v: i === 15 ? 100.5 : 100 }));
assert.equal(normalRange(flat).anomalies.length, 0);

// Rising 1 a minute from 50 toward a limit of 80: thirty minutes away.
const rising = Array.from({ length: 10 }, (_, i) => ({ t: at(i), v: 50 + i }));
const trend = { perSecond: 1 / 60, fit: 0.9, windowSecs: 540, samples: 10, secondsToLimit: 21 * 60 };
const cone = forecastCone(rising, trend, 80, 60 * 60_000)!;
assert.equal(cone.hitsAt, t0 + 9 * 60_000 + 21 * 60_000);
assert.ok(Math.abs(cone.center[cone.center.length - 1].v - 80) < 1e-9, "the line meets the limit where the trend said");
assert.equal(cone.upper[0].v, cone.lower[0].v, "the cone starts at the last reading");
assert.ok(cone.upper[16].v - cone.lower[16].v > 0, "and widens");
assert.equal(forecastCone(rising, { ...trend, secondsToLimit: undefined }, 80, 3_600_000), null, "not heading there: nothing");
assert.equal(forecastCone(rising, { ...trend, fit: 0.1 }, 80, 3_600_000), null, "a line that fits badly draws no direction");
const capped = forecastCone(rising, { ...trend, secondsToLimit: 10 * 3600 }, 80, 60 * 60_000)!;
assert.equal(Date.parse(capped.center[16].t), t0 + 9 * 60_000 + 60 * 60_000, "stops at the horizon when the limit is further");

const regions = troubleRegions(
  [
    { health: "healthy", from: at(0), to: at(10) },
    { health: "down", from: at(10), to: at(20) },
    { health: "degraded", from: at(-30), to: at(5) },
  ],
  [t0, t0 + 15 * 60_000],
);
assert.deepEqual(
  regions.map((r) => [r.label, (r.from - t0) / 60_000, (r.to - t0) / 60_000]),
  [["Down", 10, 15], ["Degraded", 0, 5]],
  "clipped to the window",
);

console.log("meaning: ok");
