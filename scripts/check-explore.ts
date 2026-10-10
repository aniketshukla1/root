// Run: node --experimental-strip-types scripts/check-explore.ts
import assert from "node:assert/strict";

import { accept, completionAt, rank, resolve, traceIdIn, volume, zoomOut, fromLocalInput, toLocalInput } from "../src/explore.ts";

/** `‸` marks the cursor; LogQL has pipes of its own. */
const at = (text: string, language: "promql" | "logql" = "promql") => {
  const cursor = text.indexOf("‸");
  return completionAt(text.replace("‸", ""), cursor, language);
};

// Where the cursor is decides what fits.
assert.deepEqual(at("node_cp‸"), { kind: "name", prefix: "node_cp", from: 0 });
assert.deepEqual(at("rate(http_requests_total{‸"), { kind: "label", prefix: "", from: 25 });
assert.deepEqual(at('up{job="api", inst‸'), { kind: "label", prefix: "inst", from: 14 });
assert.deepEqual(at('up{job="no‸'), { kind: "value", label: "job", prefix: "no", from: 8 });
assert.deepEqual(at('up{job=~"a,b‸'), { kind: "value", label: "job", prefix: "a,b", from: 9 });
assert.deepEqual(at("sum by (instance, jo‸"), { kind: "label", grouping: true, prefix: "jo", from: 18 });
assert.equal(at('up{job="api"} ‸'), null, "nothing typed: no list on a space");
assert.equal(at("rate(x[5m‸"), null, "a range is not a name");
assert.equal(at('{app="x"} |= "time‸', "logql"), null, "inside a line filter");
assert.deepEqual(at('{app="x"} | js‸', "logql"), { kind: "name", prefix: "js", from: 12 });
assert.equal(completionAt("x", 1, "traces"), null);

// A choice lands where typing would have put it.
const label = accept("up{jo}", 5, { kind: "label", prefix: "jo", from: 3 }, "job");
assert.deepEqual(label, { text: 'up{job=""}', cursor: 8 }, "into the quotes, ready for a value");
const value = accept('up{job="no"}', 10, { kind: "value", label: "job", prefix: "no", from: 8 }, "node");
assert.deepEqual(value, { text: 'up{job="node"}', cursor: 13 }, "past the closing quote");
const open = accept('up{job="no', 10, { kind: "value", label: "job", prefix: "no", from: 8 }, 'a"b');
assert.deepEqual(open, { text: 'up{job="a\\"b"', cursor: 13 }, "escaped and closed");
const grouped = accept("sum by (jo", 10, { kind: "label", grouping: true, prefix: "jo", from: 8 }, "job");
assert.equal(grouped.text, "sum by (job");

assert.deepEqual(rank(["node_load1", "up", "node_cpu_seconds_total", "go_node"], "node"), ["node_load1", "node_cpu_seconds_total", "go_node"]);
assert.deepEqual(rank(["rate"], "rate"), [], "an exact match needs no list");

// Windows.
const now = Date.UTC(2026, 9, 10, 12);
assert.deepEqual(resolve({ minutes: 60 }, now), [now - 3_600_000, now]);
assert.deepEqual(zoomOut({ minutes: 60 }, now), { minutes: 120 });
assert.deepEqual(zoomOut({ from: now - 4_000, to: now - 2_000 }, now), { from: now - 5_000, to: now - 1_000 });
assert.deepEqual(zoomOut({ from: now - 2_000, to: now }, now), { from: now - 4_000, to: now }, "never past now");
assert.equal(fromLocalInput(toLocalInput(now)), now);
assert.equal(fromLocalInput("nonsense"), null);

assert.deepEqual(volume([0, 1, 5, 9, 10, 11], [0, 10], 2), [2, 3]);
assert.equal(traceIdIn("GET / 200 trace_id=4bf92f3577b34da6a3ce929d0e0e4736 done"), "4bf92f3577b34da6a3ce929d0e0e4736");
assert.equal(traceIdIn('{"traceId":"a3ce929d0e0e4736"}'), "a3ce929d0e0e4736");
assert.equal(traceIdIn("no id here"), null);

console.log("explore: ok");
