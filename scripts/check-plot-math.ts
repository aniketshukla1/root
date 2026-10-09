// Run: node --experimental-strip-types scripts/check-plot-math.ts
import assert from "node:assert/strict";

import {
  BYTE_INCRS,
  legendClick,
  nearestIndex,
  panRange,
  SECOND_INCRS,
  windowStats,
  withAlpha,
  yRange,
} from "../src/ui/plot-math.ts";

// A series near zero is drawn from zero; one far above it is not.
assert.deepEqual(yRange(2, 10, []), [0, 10.8]);
const high = yRange(100, 104, []);
assert.ok(high[0] > 99 && high[0] < 100, `high series keeps its floor: ${high}`);
// A limit outside the data is still on the chart; a flat line sits mid-band.
assert.ok(yRange(10, 20, [30])[1] > 30);
assert.deepEqual(yRange(50, 50, []), [47.5, 52.5]);
assert.deepEqual(yRange(-4, -4, []), [-4.2, -3.8]);
assert.deepEqual(yRange(null, null, []), [0, 1]);

assert.equal(nearestIndex([], 5), -1);
assert.equal(nearestIndex([10, 20, 30], 24), 1);
assert.equal(nearestIndex([10, 20, 30], 26), 2);
assert.equal(nearestIndex([10, 20, 30], -100), 0);
assert.equal(nearestIndex([10, 20, 30], 100), 2);

assert.deepEqual(windowStats([1, 2, 3, 4], [5, 1, 9, 3], [2, 3]), { min: 1, max: 9, avg: 5, last: 9 });
assert.equal(windowStats([1, 2], [5, 6], [10, 20]), null);
assert.deepEqual(windowStats([1, 2, 3], [5, NaN, 7], [0, 9]), { min: 5, max: 7, avg: 6, last: 7 });

// Dragging right by half the plot shows the half before; never past the data.
assert.deepEqual(panRange([100, 200], 50, 100, [0, 1000]), [50, 150]);
assert.deepEqual(panRange([100, 200], 500, 100, [0, 1000]), [0, 100]);
assert.deepEqual(panRange([800, 900], -500, 100, [0, 1000]), [900, 1000]);
assert.deepEqual(panRange([0, 1000], 50, 100, [0, 1000]), [0, 1000], "nothing to pan to");

const keys = ["a", "b", "c"];
assert.deepEqual([...legendClick(keys, new Set(), "a", false)], ["b", "c"]);
assert.deepEqual([...legendClick(keys, new Set(["b", "c"]), "a", false)], []);
assert.deepEqual([...legendClick(keys, new Set(["b"]), "b", true)], []);
assert.deepEqual([...legendClick(keys, new Set(["b", "c"]), "a", true)], [], "never hides everything");

assert.ok(BYTE_INCRS.includes(500 * 1024 ** 2) && BYTE_INCRS.includes(2.5 * 1024 ** 3));
assert.ok(SECOND_INCRS.includes(3600) && SECOND_INCRS.includes(86400));
assert.equal(withAlpha("#86a8cc", 0.14), "rgba(134, 168, 204, 0.14)");
assert.equal(withAlpha("#fff", 1), "rgba(255, 255, 255, 1)");
assert.match(withAlpha("oklch(0.7 0.1 250)", 0.2), /^color-mix/);

console.log("plot-math: ok");
