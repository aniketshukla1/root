// The estate graph's arithmetic, apart from React so scripts/check-estategraph.ts
// can run it under plain Node: which links group under which relation, where
// each box sits for the lines to find it, and how a node id splits back into
// the page it names.

import type { GraphEdge, GraphNode, GraphRelation, Health, LinkProvenance } from "./types";

/** A neighbour's slot: a 32px box and the gap under it. */
export const ROW_H = 36;
export const HEAD_H = 24;
export const GROUP_GAP = 10;
/** Boxes a relation shows before the rest fold into "N more". */
export const SHOWN = 6;

/** How each relation reads from either end of it, and as a sentence. */
export const RELATIONS: Record<GraphRelation, { out: string; in: string; verb: string }> = {
  depends_on: { out: "Depends on", in: "Needed by", verb: "depends on" },
  calls: { out: "Calls", in: "Called by", verb: "calls" },
  runs_on: { out: "Runs on", in: "Running here", verb: "runs on" },
  part_of: { out: "Part of", in: "Containers and pods", verb: "is part of" },
  owned_by: { out: "Owned by", in: "Owns", verb: "is owned by" },
  in_environment: { out: "Environment", in: "In this environment", verb: "is in" },
  configured_by: { out: "Config files", in: "Configures", verb: "is configured by" },
  logs_to: { out: "Log files", in: "Logs from", verb: "logs to" },
  located_on: { out: "Read on", in: "Files read here", verb: "is read on" },
  history_in: { out: "History kept in", in: "Keeps history for", verb: "keeps its history in" },
};
const ORDER = Object.keys(RELATIONS) as GraphRelation[];

const KNOWN: Record<LinkProvenance, string> = {
  declared: "declared",
  reported: "reported by host",
  observed: "seen in traces",
  inferred: "inferred",
};

/** Trouble first, so a failing neighbour is never folded away. */
const RANK: Record<Health, number> = { down: 0, degraded: 1, unknown: 2, healthy: 2 };
export const rank = (node: GraphNode) => (node.health ? RANK[node.health] : 3);

export interface Link {
  edge: GraphEdge;
  node: GraphNode;
}

export interface Group {
  relation: GraphRelation;
  links: Link[];
}

/** `short` drops the mean, which matters least, where a box is narrow. */
export function howKnown(edge: GraphEdge, short = false): string {
  if (edge.traffic) {
    const { calls, errors, meanMs } = edge.traffic;
    return `${calls} calls · ${errors} failed${short ? "" : ` · ${Math.round(meanMs)} ms`}`;
  }
  return edge.detail ? `${KNOWN[edge.provenance]} · ${edge.detail}` : KNOWN[edge.provenance];
}

/** `component:<host>/<section>/<row>`, split back into where it lives. */
export function componentAt(id: string): { host: string; section: string; row: string } | null {
  const m = /^component:([^/]+)\/([^/]+)\/(.+)$/.exec(id);
  return m ? { host: m[1], section: m[2], row: m[3] } : null;
}

/** One end's links by relation, in a fixed order; trouble, then traffic, then name. */
export function groupsOf(edges: GraphEdge[], other: (e: GraphEdge) => string, byId: Map<string, GraphNode>): Group[] {
  return ORDER.map((relation) => ({
    relation,
    links: edges
      .filter((e) => e.relation === relation)
      .flatMap((edge) => {
        const node = byId.get(other(edge));
        return node ? [{ edge, node }] : [];
      })
      .sort(
        (a, b) =>
          rank(a.node) - rank(b.node) ||
          (b.edge.traffic?.calls ?? 0) - (a.edge.traffic?.calls ?? 0) ||
          a.node.label.localeCompare(b.node.label),
      ),
  })).filter((g) => g.links.length > 0);
}

/** Where every box in a column sits, so the lines can find them. */
export function stack(groups: Group[], side: "in" | "out", open: Set<string>) {
  let y = 0;
  const placed = groups.map((g) => {
    const key = `${side}:${g.relation}`;
    // Folding is only worth it when it hides more than the fold costs.
    const rows = !open.has(key) && g.links.length > SHOWN + 2 ? g.links.slice(0, SHOWN) : g.links;
    const more = g.links.length - rows.length;
    const first = y + HEAD_H;
    y = first + (rows.length + (more > 0 ? 1 : 0)) * ROW_H + GROUP_GAP;
    return { ...g, key, rows, more, first };
  });
  return { placed, height: Math.max(0, y - GROUP_GAP) };
}
