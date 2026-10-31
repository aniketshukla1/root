// Run: node --experimental-strip-types scripts/check-estategraph.ts
import assert from "node:assert/strict";

import { estateGraph } from "../src/contracts.generated.ts";
import { componentAt, groupsOf, HEAD_H, ROW_H, SHOWN, stack, type Group } from "../src/estategraph.ts";

// The backend's own component id, from the contract sample, still names a page.
const component = estateGraph.nodes.find((n) => n.kind === "component")!;
assert.deepEqual(componentAt(component.id), { host: "orders", section: "containers", row: "worker" });
assert.equal(componentAt("service:orders"), null);

// Links group by relation in one fixed order, whatever order they arrive in.
const byId = new Map(estateGraph.nodes.map((n) => [n.id, n]));
const into = groupsOf([...estateGraph.edges].reverse().filter((e) => e.to === "service:orders"), (e) => e.from, byId);
assert.deepEqual(into.map((g) => g.relation), ["calls", "part_of"]);

// A fold only when it hides more than it costs, and the column is as tall as
// the boxes the lines are drawn to.
const calls = (n: number): Group[] => [
  {
    relation: "calls",
    links: Array.from({ length: n }, (_, i) => ({
      edge: { from: `n${i}`, to: "x", relation: "calls" as const, provenance: "observed" as const },
      node: { id: `n${i}`, kind: "service" as const, label: `n${i}` },
    })),
  },
];
assert.equal(stack(calls(SHOWN + 2), "in", new Set()).placed[0].more, 0);
const folded = stack(calls(SHOWN + 3), "in", new Set());
assert.equal(folded.placed[0].rows.length, SHOWN);
assert.equal(folded.placed[0].more, 3);
assert.equal(folded.height, HEAD_H + (SHOWN + 1) * ROW_H);
assert.equal(stack(calls(SHOWN + 3), "in", new Set(["in:calls"])).placed[0].more, 0, "unfolded on request");

console.log("estategraph: ok");
