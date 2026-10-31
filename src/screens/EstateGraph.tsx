// The estate as one graph. Pick anything root knows of — a connection, a
// container, a team, a file, a service only traces have seen — and see every
// link it has: which way it runs, what kind of link it is, and how root knows.
// The service map answers "who calls whom"; this answers "what is this
// connected to", for every kind of thing at once.

import { useEffect, useMemo, useState, type ReactNode } from "react";

import { api, errorMessage } from "../api";
import { healthColor, type Catalog } from "../catalog";
import { componentAt, GROUP_GAP, groupsOf, HEAD_H, howKnown, rank, RELATIONS, ROW_H, stack } from "../estategraph";
import type { View } from "../nav";
import { edgeTone, type Tone } from "../servicemap";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import type { EstateGraph as Graph, GraphEdge, GraphNode, GraphNodeKind, Health } from "../types";
import { useWidth } from "../ui/charts";
import { BoxIcon, FileIcon, GlobeIcon, LayersIcon, SearchIcon, TeamIcon } from "../ui/icons";
import { Button, Chip, cx, Dot, Empty, Input, ServiceMark } from "../ui/primitives";

const CENTRE_H = 56;
/** Rows of one kind the index lists before "show all". */
const INDEX_SHOWN = 12;

const KINDS: [GraphNodeKind, string][] = [
  ["service", "Systems"],
  ["external", "Outside"],
  ["team", "Teams"],
  ["environment", "Environments"],
  ["component", "Containers and pods"],
  ["file", "Files"],
];

/** The service map's colours for a call, so a call reads the same on both. */
const TONE: Record<Tone, string> = {
  crit: "var(--color-crit)",
  warn: "var(--color-warn)",
  ok: "var(--color-data)",
  quiet: "var(--color-line-strong)",
};
const TONES = Object.keys(TONE) as Tone[];

/** The focus survives a trip to a service page and back. */
let lastFocus: string | undefined;

function kindLine(node: GraphNode, catalog: Catalog, byId: Map<string, GraphNode>): string {
  switch (node.kind) {
    case "service":
      return node.serviceKind ? catalog.label(node.serviceKind) : "Connection";
    case "component": {
      const at = componentAt(node.id);
      const what = at?.section === "pods" ? "pod" : "container";
      const host = at ? (byId.get(`service:${at.host}`)?.label ?? at.host) : "its host";
      return `${node.serviceKind ? `${catalog.label(node.serviceKind)} ${what}` : what === "pod" ? "Pod" : "Container"} on ${host}`;
    }
    case "external":
      return "Seen in traces, not watched";
    case "team":
      return "Team";
    case "environment":
      return "Environment";
    case "file":
      return node.facts?.journal ? "Journal" : "File";
  }
}

const GLYPH = { service: BoxIcon, component: BoxIcon, external: GlobeIcon, team: TeamIcon, environment: LayersIcon, file: FileIcon };

function NodeMark({ node, catalog, size = 18 }: { node: GraphNode; catalog: Catalog; size?: number }) {
  if (node.serviceKind) return <ServiceMark descriptor={catalog.get(node.serviceKind)} size={size} />;
  const Glyph = GLYPH[node.kind];
  return (
    <span className="grid shrink-0 place-items-center rounded-sm bg-raised text-ink-3" style={{ width: size, height: size }}>
      <Glyph size={Math.round(size * 0.72)} />
    </span>
  );
}

export function EstateGraph({
  catalog,
  syncedAt,
  onNavigate,
  onRefresh,
  onAsk,
}: {
  catalog: Catalog;
  syncedAt?: string;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAsk: () => void;
}) {
  const [graph, setGraph] = useState<Graph | null>(null);
  const [error, setError] = useState<string>();
  const [focus, setFocus] = useState(lastFocus);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<GraphNodeKind | null>(null);

  useEffect(() => {
    let live = true;
    // The last answer stays up while the next is read: no flash, no jump.
    const load = () =>
      api
        .getEstateGraph(30)
        .then((g) => live && (setGraph(g), setError(undefined)))
        .catch((e) => live && setError(errorMessage(e)));
    void load();
    const timer = setInterval(load, 60_000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, []);

  const byId = useMemo(() => new Map((graph?.nodes ?? []).map((n) => [n.id, n])), [graph]);
  const degree = useMemo(() => {
    const d = new Map<string, number>();
    for (const e of graph?.edges ?? []) {
      d.set(e.from, (d.get(e.from) ?? 0) + 1);
      d.set(e.to, (d.get(e.to) ?? 0) + 1);
    }
    return d;
  }, [graph]);

  // Opens on the worst-off system with the most links: where a look is most
  // likely to be wanted.
  const shown =
    (focus && byId.get(focus)) ||
    [...byId.values()]
      .filter((n) => n.kind === "service")
      .sort((a, b) => rank(a) - rank(b) || (degree.get(b.id) ?? 0) - (degree.get(a.id) ?? 0))[0] ||
    graph?.nodes[0];
  const choose = (id: string) => {
    lastFocus = id;
    setFocus(id);
  };

  const needle = query.trim().toLowerCase();
  const matches = (graph?.nodes ?? []).filter((n) => (!kind || n.kind === kind) && (!needle || n.label.toLowerCase().includes(needle)));
  const count = (k: GraphNodeKind) => graph?.nodes.filter((n) => n.kind === k).length ?? 0;

  const at = shown ? componentAt(shown.id) : null;
  const target: View | null =
    shown?.kind === "service"
      ? { name: "service", profileId: shown.id.slice("service:".length) }
      : at && { name: "component", profileId: at.host, sectionId: at.section, rowId: at.row };
  const trouble = shown?.health === "down" || shown?.health === "degraded";

  return (
    <>
      <ScreenHeader
        title="Estate graph"
        meta={
          graph
            ? `${count("service")} systems · ${count("external")} outside · ${count("component")} containers and pods · ${graph.edges.length} links`
            : undefined
        }
        syncedAt={syncedAt}
        onRefresh={onRefresh}
        onAsk={onAsk}
      />
      <Screen className="flex flex-col gap-3">
        {error && <p className="text-xs text-crit">{error}</p>}

        {graph && graph.nodes.length === 0 ? (
          <Empty
            title="Nothing to draw yet"
            hint="Add a connection in Settings. Its dependencies, owner, runtime and files, the containers its host reports and the calls traces show all appear here."
          />
        ) : (
          <>
            {/* One filter row over everything it scopes. */}
            <div className="flex flex-wrap items-center gap-1.5">
              <label className="relative flex min-w-[180px] flex-1 items-center sm:max-w-[260px]">
                <span className="pointer-events-none absolute left-2.5 text-ink-4">
                  <SearchIcon size={13} />
                </span>
                <Input
                  size="sm"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && matches[0] && choose(matches[0].id)}
                  placeholder="Find a system, team, container, file"
                  aria-label="Find in the estate graph"
                  className="w-full pl-8"
                />
              </label>
              <Chip quiet selected={kind === null} onClick={() => setKind(null)}>
                All <span className="num text-ink-4">{graph?.nodes.length ?? 0}</span>
              </Chip>
              {KINDS.filter(([k]) => count(k) > 0).map(([k, label]) => (
                <Chip key={k} quiet selected={kind === k} onClick={() => setKind(kind === k ? null : k)}>
                  {label} <span className="num text-ink-4">{count(k)}</span>
                </Chip>
              ))}
            </div>

            <div className="grid min-h-0 gap-3 @min-[900px]:grid-cols-[240px_minmax(0,1fr)] @min-[900px]:items-start">
              <Index
                nodes={matches}
                shown={shown?.id}
                degree={degree}
                catalog={catalog}
                onChoose={choose}
                searching={needle !== ""}
              />

              {graph && shown && (
                <div className="flex min-w-0 flex-col gap-3">
                  <section
                    className={cx("panel flex flex-col gap-2 px-3.5 py-3", trouble && "hue")}
                    style={trouble ? { ["--hue" as string]: healthColor(shown.health!) } : undefined}
                  >
                    <div className="flex items-center gap-2.5">
                      <NodeMark node={shown} catalog={catalog} size={28} />
                      <div className="flex min-w-0 flex-1 flex-col">
                        <h2 className="truncate text-sm font-medium text-ink">{shown.label}</h2>
                        <p className="flex min-w-0 items-center gap-1.5 text-2xs text-ink-4">
                          <span className="truncate">{kindLine(shown, catalog, byId)}</span>
                          {shown.health && (
                            <>
                              · <Dot health={shown.health} size={7} /> {shown.health}
                            </>
                          )}
                          <span className="num shrink-0">· {degree.get(shown.id) ?? 0} links</span>
                        </p>
                      </div>
                      {target && (
                        <Button size="sm" onClick={() => onNavigate(target)}>
                          Open {shown.kind === "service" ? "service" : "component"}
                        </Button>
                      )}
                    </div>
                    {shown.facts && Object.keys(shown.facts).length > 0 && (
                      <dl className="flex flex-wrap gap-x-4 gap-y-1 text-2xs">
                        {Object.entries(shown.facts).map(([k, v]) => (
                          <div key={k} className="flex min-w-0 gap-1.5">
                            <dt className="text-ink-4">{k}</dt>
                            <dd className="num truncate text-ink-2">{v}</dd>
                          </div>
                        ))}
                      </dl>
                    )}
                  </section>

                  {/* Keyed by the focus: what was unfolded and hovered belongs to the last one. */}
                  <Diagram key={shown.id} shown={shown} graph={graph} byId={byId} catalog={catalog} onChoose={choose} />
                  <Legend />
                </div>
              )}
            </div>
          </>
        )}
      </Screen>
    </>
  );
}

/**
 * The focus between what links to it and what it links to, each grouped by
 * relation, with a line from every box to the focus. Side by side while each
 * column keeps room for a name; one column under the other below that.
 */
function Diagram({
  shown,
  graph,
  byId,
  catalog,
  onChoose,
}: {
  shown: GraphNode;
  graph: Graph;
  byId: Map<string, GraphNode>;
  catalog: Catalog;
  onChoose: (id: string) => void;
}) {
  const [frame, width] = useWidth<HTMLDivElement>();
  const [open, setOpen] = useState(new Set<string>());
  const [hover, setHover] = useState<string | null>(null);

  const left = stack(groupsOf(graph.edges.filter((e) => e.to === shown.id), (e) => e.from, byId), "in", open);
  const right = stack(groupsOf(graph.edges.filter((e) => e.from === shown.id), (e) => e.to, byId), "out", open);
  const height = Math.max(left.height, right.height, CENTRE_H);
  const centre = height / 2;
  const across = width === 0 || width >= 480;
  const roomy = width >= 680;
  const gutterW = roomy ? 64 : 36;
  const centreW = roomy ? 176 : 128;
  const busiest = Math.max(1, ...graph.edges.map((e) => e.traffic?.calls ?? 0));
  const healthOf = (id: string): Health => byId.get(id)?.health ?? "unknown";
  const toneOf = (edge: GraphEdge): Tone =>
    edge.traffic ? edgeTone({ from: edge.from, to: edge.to, declared: false, ...edge.traffic }, healthOf) : "quiet";
  const trouble = shown.health === "down" || shown.health === "degraded";

  const column = (side: "in" | "out", placed: ReturnType<typeof stack>) => (
    <div className={cx("flex min-w-0 flex-col", across && "flex-1")} style={{ paddingTop: across ? (height - placed.height) / 2 : 0 }}>
      {placed.placed.length === 0 && (
        <p className={cx("py-2 text-2xs text-ink-4", across && "self-center")}>
          {side === "in" ? "Nothing links to it." : "It links to nothing."}
        </p>
      )}
      {placed.placed.map((g) => (
        <section key={g.key} style={{ marginBottom: GROUP_GAP }}>
          <h3 className="label flex items-center gap-1.5" style={{ height: HEAD_H }}>
            {RELATIONS[g.relation][side]}
            <span className="num text-ink-4">{g.links.length}</span>
          </h3>
          {g.rows.map((l) => {
            const key = `${g.key}:${l.node.id}`;
            const [from, to] = side === "in" ? [l.node.label, shown.label] : [shown.label, l.node.label];
            return (
              <button
                key={key}
                type="button"
                onClick={() => onChoose(l.node.id)}
                onMouseEnter={() => setHover(key)}
                onMouseLeave={() => setHover(null)}
                onFocus={() => setHover(key)}
                onBlur={() => setHover(null)}
                title={`${from} ${RELATIONS[g.relation].verb} ${to} (${howKnown(l.edge)}). Open its links.`}
                className={cx(
                  "relative flex w-full items-center gap-2 overflow-hidden rounded-md border border-line bg-panel pr-2 pl-2.5 text-left transition-opacity hover:border-ink-4/60",
                  hover !== null && hover !== key && "opacity-60",
                )}
                style={{ height: ROW_H - 4, marginBottom: 4 }}
              >
                {l.node.health && (
                  <span className="absolute inset-y-0 left-0 w-[3px]" style={{ background: healthColor(l.node.health) }} />
                )}
                <NodeMark node={l.node} catalog={catalog} />
                <span className="flex min-w-0 flex-1 flex-col leading-tight">
                  <span className="truncate text-xs text-ink">{l.node.label}</span>
                  <span className="num truncate text-2xs text-ink-4">{howKnown(l.edge, !roomy)}</span>
                </span>
                {l.node.health && l.node.health !== "healthy" && <Dot health={l.node.health} size={7} />}
              </button>
            );
          })}
          {g.more > 0 && (
            <button
              type="button"
              onClick={() => setOpen(new Set(open).add(g.key))}
              className="row-hover flex w-full items-center rounded-md border border-dashed border-line px-2.5 text-left text-2xs text-ink-3"
              style={{ height: ROW_H - 4, marginBottom: 4 }}
            >
              +{g.more} more
            </button>
          )}
        </section>
      ))}
    </div>
  );

  // The links between a column and the focus. Arrows point the way a link
  // reads: into the focus from the left, out of it on the right.
  const gutter = (side: "in" | "out", placed: ReturnType<typeof stack>) => {
    const pad = (height - placed.height) / 2;
    const links = placed.placed.flatMap((g) => [
      ...g.rows.map((l, i) => ({ key: `${g.key}:${l.node.id}`, edge: l.edge as GraphEdge | null, y: pad + g.first + i * ROW_H + (ROW_H - 4) / 2 })),
      ...(g.more > 0 ? [{ key: `${g.key}:more`, edge: null, y: pad + g.first + g.rows.length * ROW_H + (ROW_H - 4) / 2 }] : []),
    ]);
    return (
      <svg width={gutterW} height={height} className="shrink-0" aria-hidden>
        <defs>
          {TONES.map((tone) => (
            <marker
              key={tone}
              id={`graph-${side}-${tone}`}
              viewBox="0 0 8 8"
              refX="7"
              refY="4"
              markerUnits="userSpaceOnUse"
              markerWidth="8"
              markerHeight="8"
              orient="auto"
            >
              <path d="M0,0 L8,4 L0,8 z" style={{ fill: TONE[tone] }} />
            </marker>
          ))}
        </defs>
        {links.map(({ key, edge, y }) => {
          const tone = edge ? toneOf(edge) : "quiet";
          const [y1, y2] = side === "in" ? [y, centre] : [centre, y];
          return (
            <path
              key={key}
              d={`M 0 ${y1} C ${gutterW / 2} ${y1}, ${gutterW / 2} ${y2}, ${gutterW - 1} ${y2}`}
              fill="none"
              markerEnd={`url(#graph-${side}-${tone})`}
              strokeDasharray={edge?.provenance === "inferred" ? "2 3" : edge ? undefined : "3 3"}
              style={{
                stroke: TONE[tone],
                strokeWidth: edge?.traffic ? 1.5 + 2 * Math.sqrt(edge.traffic.calls / busiest) : 1.25,
                opacity: hover === null || hover === key ? 1 : 0.2,
              }}
            />
          );
        })}
      </svg>
    );
  };

  return (
    <div ref={frame} className={across ? "flex items-start" : "flex flex-col gap-3"}>
      {column("in", left)}
      {across && gutter("in", left)}
      <div
        className={cx("relative flex shrink-0 items-center gap-2 overflow-hidden rounded-md border bg-raised px-2.5", trouble && "hue")}
        style={{
          height: CENTRE_H,
          width: across ? centreW : undefined,
          marginTop: across ? centre - CENTRE_H / 2 : 0,
          borderColor: trouble ? healthColor(shown.health!) : "var(--color-line-strong)",
          ...(trouble ? { ["--hue" as string]: healthColor(shown.health!) } : {}),
        }}
      >
        {shown.health && <span className="absolute inset-y-0 left-0 w-[3px]" style={{ background: healthColor(shown.health) }} />}
        <NodeMark node={shown} catalog={catalog} size={22} />
        <span className="flex min-w-0 flex-col leading-tight">
          <span className="truncate text-xs font-medium text-ink">{shown.label}</span>
          <span className="truncate text-2xs text-ink-4">{kindLine(shown, catalog, byId)}</span>
        </span>
      </div>
      {across && gutter("out", right)}
      {column("out", right)}
    </div>
  );
}

/** Everything the graph holds, by kind: where a look starts. */
function Index({
  nodes,
  shown,
  degree,
  catalog,
  onChoose,
  searching,
}: {
  nodes: GraphNode[];
  shown?: string;
  degree: Map<string, number>;
  catalog: Catalog;
  onChoose: (id: string) => void;
  searching: boolean;
}) {
  const [all, setAll] = useState(new Set<GraphNodeKind>());
  return (
    <nav
      aria-label="Everything in the graph"
      className="panel flex max-h-44 min-h-0 flex-col overflow-y-auto py-1 @min-[900px]:sticky @min-[900px]:top-0 @min-[900px]:max-h-[calc(100vh_-_150px)]"
    >
      {nodes.length === 0 && <p className="px-3 py-2 text-2xs text-ink-4">Nothing matches.</p>}
      {KINDS.map(([kind, label]) => {
        const ofKind = nodes
          .filter((n) => n.kind === kind)
          .sort((a, b) => rank(a) - rank(b) || (degree.get(b.id) ?? 0) - (degree.get(a.id) ?? 0) || a.label.localeCompare(b.label));
        if (ofKind.length === 0) return null;
        const listed = all.has(kind) || searching || ofKind.length <= INDEX_SHOWN + 3 ? ofKind : ofKind.slice(0, INDEX_SHOWN);
        return (
          <section key={kind} className="flex flex-col">
            <h3 className="label flex items-center gap-1.5 px-3 pt-2 pb-1">
              {label} <span className="num text-ink-4">{ofKind.length}</span>
            </h3>
            {listed.map((n) => (
              <button
                key={n.id}
                type="button"
                onClick={() => onChoose(n.id)}
                aria-current={n.id === shown || undefined}
                className={cx(
                  "row-hover flex h-7 shrink-0 items-center gap-2 px-3 text-left text-xs",
                  n.id === shown ? "bg-raised text-ink" : "text-ink-2",
                )}
              >
                <NodeMark node={n} catalog={catalog} size={16} />
                <span className="min-w-0 flex-1 truncate">{n.label}</span>
                {n.health && n.health !== "healthy" && <Dot health={n.health} size={7} />}
                <span className="num shrink-0 text-2xs text-ink-4" title="Links">
                  {degree.get(n.id) ?? 0}
                </span>
              </button>
            ))}
            {listed.length < ofKind.length && (
              <button
                type="button"
                onClick={() => setAll(new Set(all).add(kind))}
                className="row-hover h-7 shrink-0 px-3 text-left text-2xs text-ink-3"
              >
                Show all {ofKind.length}
              </button>
            )}
          </section>
        );
      })}
    </nav>
  );
}

/** Every colour and line on the graph, said once. */
function Legend() {
  const line = (stroke: string, dash?: string, width = 1.5) => (
    <svg width="22" height="8" aria-hidden className="shrink-0">
      <path d="M1 4 H21" style={{ stroke, strokeWidth: width }} strokeDasharray={dash} />
    </svg>
  );
  const items: [ReactNode, string][] = [
    [line(TONE.quiet), "declared, or reported by a host"],
    [line(TONE.quiet, "2 3"), "inferred: root matched image and port"],
    [line(TONE.ok, undefined, 2.5), "calls seen in traces; wider is busier"],
    [line(TONE.warn, undefined, 2.5), "calls to a degraded system, or 1% failing"],
    [line(TONE.crit, undefined, 2.5), "calls to a down system, or 5% failing"],
  ];
  return (
    <div className="flex flex-col gap-1.5 text-2xs text-ink-4">
      <p>
        Arrows point the way a link reads: callers into what they call, a system into what it depends on, a container
        into its host. The bar on each box is its health.
      </p>
      <ul className="flex flex-wrap gap-x-4 gap-y-1">
        {items.map(([swatch, text]) => (
          <li key={text} className="flex items-center gap-1.5">
            {swatch}
            {text}
          </li>
        ))}
        {(["healthy", "degraded", "down", "unknown"] as Health[]).map((h) => (
          <li key={h} className="flex items-center gap-1.5">
            <span className="h-3 w-[3px] rounded-full" style={{ background: healthColor(h) }} />
            {h}
          </li>
        ))}
      </ul>
    </div>
  );
}
