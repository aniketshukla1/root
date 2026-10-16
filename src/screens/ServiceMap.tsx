// The service map: every connection, who calls whom — declared, and seen in
// traces — with health on the nodes and trouble on the edges. An incident's
// path is drawn over it, numbered in the order things turned, so where it
// started and where it went is visible before any text is read.

import { useEffect, useMemo, useState } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { propagation } from "../incident";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import { edgeKey, edgeTone, layout, mergeEdges, spreadEdges, type MapEdge, type Tone } from "../servicemap";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import type { Health, HealthRun, Incident, ServiceMap as Observed, TopologyView } from "../types";
import { useWidth } from "../ui/charts";
import { cx, Empty, Select, ServiceMark } from "../ui/primitives";

const NODE_W = 184;
const NODE_H = 54;
const COL_GAP = 112;
const ROW_GAP = 26;
/** Turned on its side for a narrow window: layers down the page, not across. */
const LAYER_GAP = 64;
const SIDE_GAP = 28;
const PAD = 20;

const TONE: Record<Tone, string> = {
  crit: "var(--color-crit)",
  warn: "var(--color-warn)",
  ok: "var(--color-data)",
  quiet: "var(--color-line-strong)",
};

const HEALTH: Record<Health, string> = {
  healthy: "var(--color-ok)",
  degraded: "var(--color-warn)",
  down: "var(--color-crit)",
  unknown: "var(--color-ink-4)",
};

export function ServiceMap({
  model,
  catalog,
  topology,
  incidents,
  healthRuns,
  syncedAt,
  onNavigate,
  onRefresh,
  onAsk,
}: {
  model: EstateModel;
  catalog: Catalog;
  topology: TopologyView;
  incidents: Incident[];
  healthRuns: Record<string, HealthRun[]>;
  syncedAt?: string;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAsk: () => void;
}) {
  const [observed, setObserved] = useState<Observed | null>(null);
  const [error, setError] = useState<string>();
  const [hover, setHover] = useState<string | null>(null);
  const [ref, width] = useWidth<HTMLDivElement>();

  useEffect(() => {
    let live = true;
    const load = () =>
      api
        .getServiceMap(30)
        .then((map) => live && (setObserved(map), setError(undefined)))
        .catch((e) => live && setError(errorMessage(e)));
    void load();
    const timer = setInterval(load, 60_000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, []);

  const byId = useMemo(() => new Map(model.services.map((s) => [s.snapshot.profileId, s])), [model.services]);
  const edges = useMemo(
    () => mergeEdges(topology.dependsOn, observed?.edges ?? [], new Set(byId.keys())),
    [topology.dependsOn, observed, byId],
  );
  const linked = model.services
    .map((s) => s.snapshot.profileId)
    .filter((id) => edges.some((e) => e.from === id || e.to === id));
  const unlinked = model.services.filter((s) => !linked.includes(s.snapshot.profileId));
  const placed = useMemo(() => layout(linked, edges), [linked.join(), edges]); // eslint-disable-line react-hooks/exhaustive-deps

  // The incident on show: the worst open one, unless someone picks another.
  const open = incidents.filter((i) => i.health !== "healthy").sort((a, b) => Number(a.acknowledged) - Number(b.acknowledged));
  const [chosen, setChosen] = useState<string | null>(null);
  const incident = chosen === "none" ? undefined : (open.find((i) => i.id === chosen) ?? open[0]);
  const order = incident
    ? propagation(incident, healthRuns, model)
        .filter((row) => row.turnedAt)
        .map((row) => row.profileId)
    : [];
  const path = incident ? spreadEdges(order, edges) : new Set<string>();
  const members = new Set(incident?.profileIds ?? []);

  const healthOf = (id: string): Health => byId.get(id)?.health ?? "unknown";
  const nameOf = (id: string) => byId.get(id)?.snapshot.profileName ?? id;
  const cols = Math.max(1, ...[...placed.values()].map((p) => p.col + 1));
  const rows = Math.max(1, ...[...placed.values()].map((p) => p.row + 1));
  const layerSize = new Map<number, number>();
  for (const p of placed.values()) layerSize.set(p.col, (layerSize.get(p.col) ?? 0) + 1);
  const acrossW = PAD * 2 + cols * NODE_W + (cols - 1) * COL_GAP;
  // Layers across the page while they fit at three quarters' size; down the
  // page when they would not, because a request read top to bottom at full
  // size beats one read sideways in a scrollbox.
  const vertical = width > 0 && width < acrossW * 0.75;
  const naturalW = vertical ? PAD * 2 + rows * NODE_W + (rows - 1) * SIDE_GAP : acrossW;
  const naturalH = vertical
    ? PAD * 2 + cols * NODE_H + (cols - 1) * LAYER_GAP
    : PAD * 2 + rows * NODE_H + (rows - 1) * ROW_GAP;
  // Down to two thirds to fit; past that it scrolls rather than shrinking
  // names below reading size.
  const scale = width > 0 ? Math.max(0.66, Math.min(1, width / naturalW)) : 1;
  const at = (id: string) => {
    const p = placed.get(id) ?? { col: 0, row: 0 };
    // Each layer centred against the widest, so a lone node sits mid-way.
    const offset = (rows - (layerSize.get(p.col) ?? 1)) / 2 + p.row;
    return vertical
      ? { x: PAD + offset * (NODE_W + SIDE_GAP), y: PAD + p.col * (NODE_H + LAYER_GAP) }
      : { x: PAD + p.col * (NODE_W + COL_GAP), y: PAD + offset * (NODE_H + ROW_GAP) };
  };
  const busiest = Math.max(1, ...edges.map((e) => e.calls));

  const route = (e: MapEdge) => {
    const a = at(e.from);
    const b = at(e.to);
    if (vertical) {
      if (b.y > a.y) {
        const x1 = a.x + NODE_W / 2;
        const y1 = a.y + NODE_H;
        const x2 = b.x + NODE_W / 2;
        const y2 = b.y - 7;
        const dy = (y2 - y1) / 2;
        return `M ${x1} ${y1} C ${x1} ${y1 + dy}, ${x2} ${y2 - dy}, ${x2} ${y2}`;
      }
      const x1 = a.x + NODE_W;
      const y1 = a.y + NODE_H / 2;
      const x2 = b.x + NODE_W + 7;
      const y2 = b.y + NODE_H / 2;
      return `M ${x1} ${y1} C ${x1 + 70} ${y1}, ${x2 + 70} ${y2}, ${x2} ${y2}`;
    }
    if (b.x > a.x) {
      const x1 = a.x + NODE_W;
      const y1 = a.y + NODE_H / 2;
      const x2 = b.x - 7;
      const y2 = b.y + NODE_H / 2;
      const dx = (x2 - x1) / 2;
      return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
    }
    // A call back left — a cycle — loops under both boxes instead of
    // crossing back through the columns.
    const x1 = a.x + NODE_W / 2;
    const y1 = a.y + NODE_H;
    const x2 = b.x + NODE_W / 2;
    const y2 = b.y + NODE_H + 7;
    return `M ${x1} ${y1} C ${x1} ${y1 + 70}, ${x2} ${y2 + 70}, ${x2} ${y2}`;
  };

  const lit = (e: MapEdge) => hover === null || e.from === hover || e.to === hover;
  const describe = (e: MapEdge) =>
    `${nameOf(e.from)} → ${nameOf(e.to)}` +
    (e.calls > 0
      ? ` · ${e.calls} calls in the sample, ${e.errors} failed (${((e.errors / e.calls) * 100).toFixed(1)}%), ${Math.round(e.meanMs ?? 0)}ms on average`
      : " · declared, not seen in traces yet");
  const tones: Tone[] = ["crit", "warn", "ok", "quiet"];

  return (
    <>
      <ScreenHeader
        title="Service map"
        meta={`${linked.length} connected · ${edges.length} dependencies · ${edges.filter((e) => e.calls > 0).length} seen in ${observed?.traces ?? 0} traces`}
        syncedAt={syncedAt}
        onRefresh={onRefresh}
        onAsk={onAsk}
      >
        {open.length > 0 && (
          <label className="flex items-center gap-1.5 text-2xs text-ink-4">
            Showing
            <Select size="sm" value={incident?.id ?? "none"} onChange={(e) => setChosen(e.target.value)} aria-label="Incident on the map">
              {open.map((i) => (
                <option key={i.id} value={i.id}>
                  {i.title}
                </option>
              ))}
              <option value="none">No incident</option>
            </Select>
          </label>
        )}
      </ScreenHeader>
      <Screen className="flex flex-col gap-3">
        {error && <p className="text-xs text-crit">{error}</p>}
        {incident && order.length > 1 && (
          <p className="text-xs text-ink-3">
            <span className="text-crit">How it spread:</span>{" "}
            {order.map((id, i) => (
              <span key={id}>
                {i > 0 && <span className="text-ink-4"> → </span>}
                <span className="num text-ink-4">{i + 1}</span> {nameOf(id)}
              </span>
            ))}
          </p>
        )}

        {linked.length === 0 ? (
          <Empty
            title="Nothing to map yet"
            hint="Declare what each connection depends on in its settings, or connect a store that keeps traces — calls between services appear here as they are seen."
          />
        ) : (
          <div ref={ref} className="overflow-x-auto rounded-md border border-line bg-page">
            <div className="relative origin-top-left" style={{ width: naturalW * scale, height: naturalH * scale }}>
              <div className="absolute top-0 left-0 origin-top-left" style={{ width: naturalW, height: naturalH, transform: `scale(${scale})` }}>
                <svg width={naturalW} height={naturalH} className="absolute inset-0" aria-hidden>
                  <defs>
                    {tones.map((tone) => (
                      // In user space: an arrow the size of the stroke would be a
                      // wedge on the busiest call and a speck on the quiet ones.
                      <marker key={tone} id={`map-arrow-${tone}`} viewBox="0 0 8 8" refX="1" refY="4" markerUnits="userSpaceOnUse" markerWidth="11" markerHeight="11" orient="auto-start-reverse">
                        <path d="M0,0 L8,4 L0,8 z" style={{ fill: TONE[tone] }} />
                      </marker>
                    ))}
                  </defs>
                  {edges.map((e) => {
                    const tone = edgeTone(e, healthOf);
                    const on = path.has(edgeKey(e.from, e.to));
                    if (on) return null;
                    return (
                      <path
                        key={edgeKey(e.from, e.to)}
                        d={route(e)}
                        fill="none"
                        markerEnd={`url(#map-arrow-${tone})`}
                        strokeDasharray={e.calls === 0 ? "4 4" : undefined}
                        style={{
                          stroke: TONE[tone],
                          strokeWidth: e.calls > 0 ? 1.5 + 3.5 * Math.sqrt(e.calls / busiest) : 1.2,
                          opacity: lit(e) ? (incident ? 0.45 : 0.9) : 0.15,
                        }}
                      >
                        <title>{describe(e)}</title>
                      </path>
                    );
                  })}
                  {/* The path last, on top, drawn in once along its own direction. */}
                  {edges
                    .filter((e) => path.has(edgeKey(e.from, e.to)))
                    .map((e) => (
                      <path
                        key={`path-${edgeKey(e.from, e.to)}`}
                        d={route(e)}
                        fill="none"
                        pathLength={1}
                        className="a-draw"
                        markerEnd="url(#map-arrow-crit)"
                        style={{ stroke: TONE.crit, strokeWidth: 3 + 2.5 * Math.sqrt(e.calls / busiest), ["--len" as string]: 1, opacity: lit(e) ? 1 : 0.3 }}
                      >
                        <title>{describe(e)}</title>
                      </path>
                    ))}
                </svg>
                {linked.map((id) => {
                  const service = byId.get(id);
                  if (!service) return null;
                  const { x, y } = at(id);
                  const step = order.indexOf(id);
                  const dim = (incident && !members.has(id)) || (hover !== null && hover !== id && !edges.some((e) => (e.from === hover && e.to === id) || (e.to === hover && e.from === id)));
                  return (
                    <button
                      key={id}
                      type="button"
                      onClick={() => onNavigate({ name: "service", profileId: id })}
                      onMouseEnter={() => setHover(id)}
                      onMouseLeave={() => setHover(null)}
                      onFocus={() => setHover(id)}
                      onBlur={() => setHover(null)}
                      className={cx(
                        "panel absolute flex items-center gap-2 overflow-hidden px-2.5 text-left transition-opacity hover:border-ink-4/60",
                        dim && "opacity-40",
                      )}
                      style={{ left: x, top: y, width: NODE_W, height: NODE_H, borderColor: service.health === "healthy" ? undefined : HEALTH[service.health] }}
                      title={`${service.snapshot.profileName} — ${service.snapshot.summary}`}
                    >
                      <span className="absolute inset-y-0 left-0 w-[3px]" style={{ background: HEALTH[service.health] }} />
                      <ServiceMark descriptor={catalog.get(service.snapshot.kind)} size={22} />
                      <span className="flex min-w-0 flex-col">
                        <span className="truncate text-xs text-ink">{service.snapshot.profileName}</span>
                        <span className="num truncate text-2xs text-ink-4">
                          {catalog.label(service.snapshot.kind)} · {service.health}
                        </span>
                      </span>
                      {step >= 0 && (
                        <span
                          className="num absolute top-1 right-1 grid h-4 min-w-4 place-items-center rounded-full px-1 text-[10px] text-white"
                          style={{ background: TONE.crit }}
                          title={`Turned ${step === 0 ? "first" : `${step + 1}${step === 1 ? "nd" : step === 2 ? "rd" : "th"}`}`}
                        >
                          {step + 1}
                        </span>
                      )}
                    </button>
                  );
                })}
              </div>
            </div>
          </div>
        )}

        <p className="text-2xs text-ink-4">
          Arrows point from caller to callee; width is traffic in sampled traces, dashed is declared but not yet seen. Red is
          a failing path — a callee down, or 5% of its calls failing; amber, a callee degraded or 1% failing.
          {observed && observed.unknown.length > 0 && <> Traces also name {observed.unknown.join(", ")}, which no connection covers.</>}
        </p>

        {unlinked.length > 0 && linked.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5 text-2xs text-ink-4">
            Not linked to anything yet:
            {unlinked.map((s) => (
              <button
                key={s.snapshot.profileId}
                type="button"
                onClick={() => onNavigate({ name: "service", profileId: s.snapshot.profileId })}
                className="row-hover flex items-center gap-1 rounded-sm px-1.5 py-0.5 text-ink-3"
              >
                <span className="h-1.5 w-1.5 rounded-full" style={{ background: HEALTH[s.health] }} />
                {s.snapshot.profileName}
              </button>
            ))}
          </div>
        )}
      </Screen>
    </>
  );
}
