// Run: node --experimental-strip-types scripts/check-servicemap.ts
import assert from "node:assert/strict";

import { edgeKey, edgeTone, layout, mergeEdges, spreadEdges } from "../src/servicemap.ts";

const known = new Set(["edge", "web", "api", "db", "cache", "bus", "lonely"]);
const edges = mergeEdges(
  { web: ["api"], api: ["db", "cache"], ghost: ["api"] },
  [
    { from: "edge", to: "web", calls: 100, errors: 0, meanMs: 50 },
    { from: "api", to: "db", calls: 40, errors: 4, meanMs: 12 },
    { from: "api", to: "bus", calls: 10, errors: 0, meanMs: 3 },
    { from: "api", to: "api", calls: 99, errors: 0, meanMs: 1 },
  ],
  known,
);
assert.equal(edges.length, 5, "web>api, api>db, api>cache declared; edge>web, api>bus observed (api>db merging); the self-call and the unknown dropped");
const apiDb = edges.find((e) => e.from === "api" && e.to === "db")!;
assert.ok(apiDb.declared && apiDb.calls === 40, "declared and observed merge into one edge");

const placed = layout(["db", "cache", "bus", "api", "web", "edge"], edges);
const col = (n: string) => placed.get(n)!.col;
assert.ok(col("edge") < col("web") && col("web") < col("api") && col("api") < col("db"), "callers to the left of what they call");
assert.equal(col("db"), col("cache"));
assert.equal(col("db"), col("bus"));

// A ring of calls still reads from its entry: the one nothing outside the
// ring calls is not where it starts. status → edge → web → api → db, and db
// declared as depending on edge, closing the ring.
const ring = mergeEdges(
  { db: ["edge"], web: ["edge"] },
  [
    { from: "status", to: "edge", calls: 5, errors: 0, meanMs: 1 },
    { from: "edge", to: "web", calls: 5, errors: 0, meanMs: 1 },
    { from: "web", to: "api", calls: 5, errors: 0, meanMs: 1 },
    { from: "api", to: "db", calls: 5, errors: 0, meanMs: 1 },
  ],
  new Set(["status", "edge", "web", "api", "db"]),
);
const ringPlaced = layout(["api", "db", "edge", "status", "web"], ring);
const ringCol = (n: string) => ringPlaced.get(n)!.col;
assert.deepEqual(["status", "edge", "web", "api", "db"].map(ringCol), [0, 1, 2, 3, 4], "entry on the left, the loop's back edge dropped");

// A cycle closes rather than recursing forever.
const loop = layout(["a", "b"], mergeEdges({ a: ["b"], b: ["a"] }, [], new Set(["a", "b"])));
assert.equal(loop.size, 2);

// db turned first, then api, then web: the path runs along web>api and api>db.
const path = spreadEdges(["db", "api", "web"], edges);
assert.deepEqual([...path].sort(), [edgeKey("api", "db"), edgeKey("web", "api")]);
assert.deepEqual([...spreadEdges(["web", "api"], edges)], [edgeKey("web", "api")], "a retry storm travels caller to callee: still the path");
assert.equal(spreadEdges(["web", "cache"], edges).size, 0, "two members with no call between them have no path between them");

const health = (id: string) => (id === "db" ? "degraded" : id === "cache" ? "down" : "healthy") as const;
assert.equal(edgeTone(apiDb, health), "crit", "10% of calls failing");
assert.equal(edgeTone(edges.find((e) => e.to === "cache")!, health), "crit", "into a connection that is down");
assert.equal(edgeTone(edges.find((e) => e.to === "web")!, health), "ok");
assert.equal(edgeTone({ from: "x", to: "y", declared: true, calls: 0, errors: 0 }, () => "healthy"), "quiet");
assert.equal(edgeTone({ from: "x", to: "y", declared: false, calls: 1240, errors: 3 }, () => "healthy"), "ok", "0.2% failing is background");
assert.equal(edgeTone({ from: "x", to: "y", declared: false, calls: 100, errors: 2 }, () => "healthy"), "warn");

console.log("servicemap: ok");
