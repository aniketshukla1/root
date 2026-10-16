// The service map's arithmetic, apart from React so scripts/check-servicemap.ts
// can run it under plain Node: which edges there are, where each connection
// sits, which edges carry an incident, and what colour each one is.

import type { Health, ObservedEdge } from "./types";

export interface MapEdge {
  from: string;
  to: string;
  /** Someone wrote it down as a dependency. */
  declared: boolean;
  /** Calls the sampled traces saw; zero when only declared. */
  calls: number;
  errors: number;
  meanMs?: number;
}

export const edgeKey = (from: string, to: string) => `${from}>${to}`;

/** Declared dependencies and observed calls, one edge per caller and callee. */
export function mergeEdges(dependsOn: Record<string, string[]>, observed: ObservedEdge[], known: Set<string>): MapEdge[] {
  const edges = new Map<string, MapEdge>();
  for (const [from, deps] of Object.entries(dependsOn)) {
    for (const to of deps) {
      if (from === to || !known.has(from) || !known.has(to)) continue;
      edges.set(edgeKey(from, to), { from, to, declared: true, calls: 0, errors: 0 });
    }
  }
  for (const o of observed) {
    if (o.from === o.to || !known.has(o.from) || !known.has(o.to)) continue;
    const seen = edges.get(edgeKey(o.from, o.to));
    edges.set(edgeKey(o.from, o.to), {
      from: o.from,
      to: o.to,
      declared: seen?.declared ?? false,
      calls: o.calls,
      errors: o.errors,
      meanMs: o.meanMs,
    });
  }
  return [...edges.values()];
}

/**
 * Columns by depth — a connection one column left of everything it calls,
 * so a request reads left to right — and, within a column, rows ordered by
 * where their neighbours sit, which untangles most crossings. A cycle stops
 * where it closes.
 */
export function layout(nodes: string[], edges: MapEdge[]): Map<string, { col: number; row: number }> {
  const out = new Map<string, string[]>(nodes.map((n) => [n, []]));
  const callers = new Map<string, number>(nodes.map((n) => [n, 0]));
  for (const e of edges) {
    if (!out.has(e.from) || !out.has(e.to)) continue;
    out.get(e.from)!.push(e.to);
    callers.set(e.to, (callers.get(e.to) ?? 0) + 1);
  }

  // Break cycles before layering: walk depth-first from the connections that
  // the fewest things call, and leave out only the edge that closes each loop.
  // A guard inside the depth count alone layered a ring of calls from
  // wherever the count happened to start — the callee could land first.
  const calls = new Map<string, string[]>(nodes.map((n) => [n, []]));
  const mark = new Map<string, 1 | 2>();
  const walk = (n: string) => {
    mark.set(n, 1);
    for (const to of out.get(n) ?? []) {
      if (mark.get(to) === 1) continue;
      calls.get(n)!.push(to);
      if (!mark.has(to)) walk(to);
    }
    mark.set(n, 2);
  };
  const starts = [...nodes].sort(
    (a, b) => (callers.get(a) ?? 0) - (callers.get(b) ?? 0) || (out.get(b)?.length ?? 0) - (out.get(a)?.length ?? 0),
  );
  for (const n of starts) if (!mark.has(n)) walk(n);

  const depth = new Map<string, number>();
  const depthOf = (id: string, path: Set<string>): number => {
    const known = depth.get(id);
    if (known !== undefined) return known;
    if (path.has(id)) return 0;
    path.add(id);
    const d = Math.max(-1, ...(calls.get(id) ?? []).map((to) => depthOf(to, path))) + 1;
    path.delete(id);
    depth.set(id, d);
    return d;
  };
  nodes.forEach((n) => depthOf(n, new Set()));
  const deepest = Math.max(0, ...depth.values());

  const columns: string[][] = Array.from({ length: deepest + 1 }, () => []);
  for (const n of nodes) columns[deepest - (depth.get(n) ?? 0)].push(n);

  const neighbours = new Map<string, string[]>(nodes.map((n) => [n, []]));
  for (const e of edges) {
    neighbours.get(e.from)?.push(e.to);
    neighbours.get(e.to)?.push(e.from);
  }
  const rowOf = new Map<string, number>();
  const settle = () => columns.forEach((col) => col.forEach((n, i) => rowOf.set(n, i)));
  settle();
  // Barycentre sweeps, left to right and back, twice: each node moves to the
  // average row of the neighbours already placed; one with none keeps its own.
  for (let pass = 0; pass < 2; pass += 1) {
    for (const order of [columns.keys(), [...columns.keys()].reverse()]) {
      for (const c of order) {
        const keyOf = (n: string) => {
          const rows = (neighbours.get(n) ?? []).filter((m) => !columns[c].includes(m)).map((m) => rowOf.get(m) ?? 0);
          return rows.length > 0 ? rows.reduce((a, b) => a + b, 0) / rows.length : (rowOf.get(n) ?? 0);
        };
        const keyed = columns[c].map((n) => [n, keyOf(n)] as const);
        columns[c] = keyed.sort((a, b) => a[1] - b[1]).map(([n]) => n);
        columns[c].forEach((n, i) => rowOf.set(n, i));
      }
    }
  }

  const placed = new Map<string, { col: number; row: number }>();
  columns.forEach((col, c) => col.forEach((n, row) => placed.set(n, { col: c, row })));
  return placed;
}

/**
 * The edges an incident travelled along: every call between two members that
 * turned. A failure usually climbs from what is called to what calls it, but
 * a caller's retry storm takes its callee down too, so either way counts —
 * the numbers on the nodes say which way it went, and the arrows keep saying
 * who calls whom.
 */
export function spreadEdges(order: string[], edges: MapEdge[]): Set<string> {
  const turned = new Set(order);
  return new Set(edges.filter((e) => turned.has(e.from) && turned.has(e.to)).map((e) => edgeKey(e.from, e.to)));
}

export type Tone = "crit" | "warn" | "ok" | "quiet";

/** What colour a call is: the callee's health first, then the calls that failed. */
export function edgeTone(edge: MapEdge, healthOf: (id: string) => Health): Tone {
  const callee = healthOf(edge.to);
  if (callee === "down") return "crit";
  const failing = edge.calls > 0 ? edge.errors / edge.calls : 0;
  if (failing >= 0.05) return "crit";
  // Under one call in a hundred is the background rate of any busy path.
  if (callee === "degraded" || failing >= 0.01) return "warn";
  return edge.calls > 0 ? "ok" : "quiet";
}
