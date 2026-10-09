// The Overview's pictures for a calm estate: what is connected to what, the
// four golden signals across every connection, and findings as a ranked list
// whose long explanations open on demand. A calm screen should be read at a
// glance, so each of these says its piece without a paragraph.

import { useState } from "react";

import type { Catalog } from "../catalog";
import { DATA_INK, healthColor } from "../catalog";
import { formatMetricValue } from "../format";
import { SIGNAL_LABEL, signalOf, type Signal } from "../golden";
import type { ServiceView } from "../model";
import type { Finding, Health, MetricSeries, RecordedVerdict, TopologyView, Verdict } from "../types";
import { Sparkline, staticKind, useWidth } from "./charts";
import { FindingList, SEVERITY } from "./config";
import { ChevronRight } from "./icons";
import { cx, Dot, Panel, ServiceMark, Tag } from "./primitives";

const HEALTH_RANK: Record<Health, number> = { down: 0, degraded: 1, unknown: 2, healthy: 3 };

// ------------------------------------------------------------ status grid

/**
 * Every connection as one tile, worst first: its mark, its name, how many
 * components it watches, and its health as the edge colour and the dot.
 */
export function ConnectionGrid({
  services,
  catalog,
  onOpen,
}: {
  services: ServiceView[];
  catalog: Catalog;
  onOpen: (profileId: string) => void;
}) {
  const ordered = [...services].sort(
    (a, b) =>
      HEALTH_RANK[a.health] - HEALTH_RANK[b.health] ||
      a.snapshot.profileName.localeCompare(b.snapshot.profileName),
  );
  return (
    <div className="grid gap-1.5 [grid-template-columns:repeat(auto-fill,minmax(156px,1fr))]">
      {ordered.map((s) => (
        <button
          key={s.snapshot.profileId}
          type="button"
          onClick={() => onOpen(s.snapshot.profileId)}
          className="row-hover flex min-w-0 items-center gap-2 rounded-md border border-line bg-page px-2 py-1.5 text-left"
          style={{ borderLeftColor: healthColor(s.health), borderLeftWidth: 2 }}
          title={`${s.snapshot.profileName} — ${s.health}`}
        >
          <ServiceMark descriptor={catalog.get(s.snapshot.kind)} size={16} />
          <span className="min-w-0 flex-1">
            <span className="block truncate text-xs text-ink">{s.snapshot.profileName}</span>
            <span className="num block truncate text-2xs text-ink-4">
              {s.entities.length} components
              {s.alerts.length > 0 && ` · ${s.alerts.length} alert${s.alerts.length === 1 ? "" : "s"}`}
            </span>
          </span>
          <Dot health={s.health} size={7} />
        </button>
      ))}
    </div>
  );
}

// ------------------------------------------------------------------- map

/**
 * Connections as nodes and declared dependencies as edges, callers on the
 * left and what they depend on to the right. A dependency in trouble colours
 * the edges into it, so a failing path is visible before any text is read.
 * Connections with no declared link are listed beneath rather than scattered
 * across the plot.
 */
export function EstateMap({
  services,
  topology,
  onOpen,
}: {
  services: ServiceView[];
  topology: TopologyView;
  onOpen: (profileId: string) => void;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const byId = new Map(services.map((s) => [s.snapshot.profileId, s]));
  const deps = (id: string) => (topology.dependsOn[id] ?? []).filter((d) => byId.has(d));
  const linked = services.filter(
    (s) => deps(s.snapshot.profileId).length > 0 || (topology.dependents[s.snapshot.profileId] ?? []).some((d) => byId.has(d)),
  );
  const unlinked = services.filter((s) => !linked.includes(s));

  // Rank: how many hops of dependencies sit under a connection. A cycle stops
  // where it closes rather than recursing forever.
  const rank = new Map<string, number>();
  const rankOf = (id: string, seen: Set<string>): number => {
    const known = rank.get(id);
    if (known !== undefined) return known;
    if (seen.has(id)) return 0;
    seen.add(id);
    const r = Math.max(-1, ...deps(id).map((d) => rankOf(d, seen))) + 1;
    seen.delete(id);
    rank.set(id, r);
    return r;
  };
  linked.forEach((s) => rankOf(s.snapshot.profileId, new Set()));
  const top = Math.max(0, ...rank.values());
  const columns: ServiceView[][] = Array.from({ length: top + 1 }, () => []);
  for (const s of linked) columns[top - (rank.get(s.snapshot.profileId) ?? 0)].push(s);
  for (const column of columns) {
    column.sort(
      (a, b) =>
        HEALTH_RANK[a.health] - HEALTH_RANK[b.health] ||
        a.snapshot.profileName.localeCompare(b.snapshot.profileName),
    );
  }

  const rowH = 30;
  const padY = 14;
  const height = Math.max(...columns.map((c) => c.length), 1) * rowH + padY * 2;
  const colW = columns.length > 0 ? width / columns.length : width;
  const pos = new Map<string, { x: number; y: number }>();
  columns.forEach((column, ci) => {
    const offset = (height - padY * 2 - column.length * rowH) / 2;
    column.forEach((s, ri) => {
      pos.set(s.snapshot.profileId, { x: ci * colW + 10, y: padY + offset + ri * rowH + rowH / 2 });
    });
  });
  const chars = Math.max(4, Math.floor((colW - 34) / 6.4));

  return (
    <div ref={ref} className="flex min-w-0 flex-col gap-2">
      {linked.length > 0 && width > 0 && (
        <svg width={width} height={height} className="block overflow-visible" role="img" aria-label="Connections and their declared dependencies">
          {linked.flatMap((s) =>
            deps(s.snapshot.profileId).map((d) => {
              const from = pos.get(s.snapshot.profileId);
              const to = pos.get(d);
              const target = byId.get(d);
              if (!from || !to || !target) return null;
              const failing = target.health === "down" || target.health === "degraded";
              const x1 = from.x + 6;
              const x2 = to.x - 6;
              const mid = (x1 + x2) / 2;
              return (
                <path
                  key={`${s.snapshot.profileId}->${d}`}
                  d={`M${x1},${from.y} C${mid},${from.y} ${mid},${to.y} ${x2},${to.y}`}
                  fill="none"
                  stroke={failing ? healthColor(target.health) : "var(--color-line-strong)"}
                  strokeWidth={failing ? 1.5 : 1}
                  opacity={failing ? 0.9 : 1}
                />
              );
            }),
          )}
          {linked.map((s) => {
            const p = pos.get(s.snapshot.profileId);
            if (!p) return null;
            const name = s.snapshot.profileName;
            const trouble = s.health === "down" || s.health === "degraded";
            return (
              <g
                key={s.snapshot.profileId}
                role="button"
                tabIndex={0}
                className="cursor-pointer outline-none [&:focus-visible>circle]:stroke-ink"
                onClick={() => onOpen(s.snapshot.profileId)}
                onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && onOpen(s.snapshot.profileId)}
              >
                <title>{`${name} — ${s.health}`}</title>
                {/* The glow is the status hue, so a node in trouble reads as
                    one from across the room without a new legend. */}
                {trouble && <circle cx={p.x} cy={p.y} r={9} fill={healthColor(s.health)} opacity={0.22} />}
                <circle cx={p.x} cy={p.y} r={5} fill={healthColor(s.health)} stroke="var(--color-panel)" strokeWidth={2} />
                <text x={p.x + 11} y={p.y + 3.5} fontSize={11} fill="var(--color-ink-2)">
                  {name.length > chars ? `${name.slice(0, chars - 1)}…` : name}
                </text>
              </g>
            );
          })}
        </svg>
      )}
      {unlinked.length > 0 && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
          <span className="label">{linked.length > 0 ? "Not linked" : "No dependencies declared"}</span>
          {unlinked.map((s) => (
            <button
              key={s.snapshot.profileId}
              type="button"
              onClick={() => onOpen(s.snapshot.profileId)}
              className="row-hover flex items-center gap-1.5 rounded-sm px-1 text-2xs text-ink-3"
            >
              <span className="h-2 w-2 shrink-0 rounded-full" style={{ background: healthColor(s.health) }} />
              {s.snapshot.profileName}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// --------------------------------------------------------------- signals

interface Reading {
  service: ServiceView;
  series: MetricSeries;
}

/** Over its limit first, then nearest to it, then whatever moved most. */
function concern(r: Reading): number {
  const { latest, warnAbove, points } = r.series;
  if (warnAbove !== undefined && warnAbove > 0) return 10 + latest / warnAbove;
  const first = points[0]?.v ?? latest;
  return Math.min(1, Math.abs(latest - first) / (Math.abs(first) || 1));
}

/**
 * One tile per golden signal, each listing the estate's series of that kind
 * with its line and its value, most concerning first. "How busy is
 * everything" becomes four pictures rather than a table of numbers.
 */
export function SignalTiles({
  services,
  history,
  catalog,
  onOpen,
}: {
  services: ServiceView[];
  history: Record<string, MetricSeries[]>;
  catalog: Catalog;
  onOpen: (profileId: string) => void;
}) {
  const readings: Record<Signal, Reading[]> = { latency: [], traffic: [], errors: [], saturation: [], other: [] };
  for (const service of services) {
    for (const series of history[service.snapshot.profileId] ?? []) {
      if (series.points.length < 2 || staticKind(series)) continue;
      readings[signalOf(series, service.snapshot.kind)].push({ service, series });
    }
  }
  const signals: Signal[] = ["latency", "traffic", "errors", "saturation"];

  return (
    <div className="grid gap-3 @3xl:grid-cols-2 @6xl:grid-cols-4">
      {signals.map((signal) => {
        const all = readings[signal];
        const shown = [...all].sort((a, b) => concern(b) - concern(a)).slice(0, 4);
        return (
          <Panel
            key={signal}
            title={SIGNAL_LABEL[signal]}
            subtitle={all.length > 0 ? `${all.length} series` : "none reported"}
          >
            {shown.length === 0 ? (
              <p className="num text-2xs text-ink-4">No connection reports one.</p>
            ) : (
              <div className="flex flex-col gap-1">
                {shown.map(({ service, series }) => {
                  const over = series.warnAbove !== undefined && series.latest > series.warnAbove;
                  return (
                    <button
                      key={`${service.snapshot.profileId}:${series.key}`}
                      type="button"
                      onClick={() => onOpen(service.snapshot.profileId)}
                      className="row-hover flex min-w-0 items-center gap-2 rounded-sm px-1 py-0.5 text-left"
                      title={`${service.snapshot.profileName} · ${series.label}`}
                    >
                      <ServiceMark descriptor={catalog.get(service.snapshot.kind)} size={14} />
                      <span className="min-w-0 flex-1 truncate text-2xs text-ink-2">
                        {service.snapshot.profileName}
                        <span className="text-ink-4"> · {series.label}</span>
                      </span>
                      <Sparkline points={series.points} width={56} height={18} color={DATA_INK} warnAbove={series.warnAbove} />
                      <span className={cx("num w-[60px] shrink-0 text-right text-2xs", over ? "text-warn" : "text-ink")}>
                        {formatMetricValue(series.latest, series.unit)}
                      </span>
                    </button>
                  );
                })}
              </div>
            )}
          </Panel>
        );
      })}
    </div>
  );
}

// -------------------------------------------------------------- findings

/**
 * Findings one line each, most urgent first. The reasoning, the evidence and
 * the commands are one click away instead of being the screen.
 */
export function FindingRows({
  findings,
  verdicts,
  onJudge,
  onOpenService,
}: {
  findings: Finding[];
  verdicts: Record<string, RecordedVerdict>;
  onJudge: (verdict: Verdict) => void;
  onOpenService: (profileId: string) => void;
}) {
  const [open, setOpen] = useState<string | null>(null);
  const ranked = [...findings].sort((a, b) => SEVERITY[b.severity].rank - SEVERITY[a.severity].rank);
  return (
    <div className="flex flex-col">
      {ranked.map((finding) => {
        const expanded = open === finding.id;
        const severity = SEVERITY[finding.severity];
        return (
          <div key={finding.id} className="border-b border-line-soft last:border-0">
            <button
              type="button"
              aria-expanded={expanded}
              onClick={() => setOpen(expanded ? null : finding.id)}
              className="row-hover flex w-full items-center gap-2.5 px-3 py-2 text-left"
            >
              <span className="w-[64px] shrink-0">
                <Tag color={severity.color}>{severity.label}</Tag>
              </span>
              <span className="min-w-0 flex-1 truncate text-xs text-ink">{finding.title}</span>
              <span className="num hidden shrink-0 text-2xs text-ink-4 @2xl:inline">{finding.profileName}</span>
              <ChevronRight
                size={12}
                className={cx("shrink-0 text-ink-4 transition-transform", expanded && "rotate-90")}
              />
            </button>
            {expanded && (
              <div className="px-3 pb-3">
                <FindingList
                  findings={[finding]}
                  verdicts={verdicts}
                  onJudge={onJudge}
                  onOpenService={onOpenService}
                />
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
