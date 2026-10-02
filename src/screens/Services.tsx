import { useMemo, useState } from "react";

import type { Catalog } from "../catalog";
import { DATA_INK, healthColor, healthLabel, isProblem } from "../catalog";
import { formatMetricValue, formatRelative, isBreaching } from "../format";
import {
  criticalityLabel,
  environmentLabel,
  groupServices,
  GROUP_LABELS,
  type EstateModel,
  type GroupBy,
  type ServiceView,
} from "../model";
import type { View } from "../nav";
import type { Incident, MetricSeries } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Sparkline } from "../ui/charts";
import {
  cx,
  Dot,
  Empty,
  HealthBadge,
  Input,
  Panel,
  Segmented,
  ServiceMark,
  Tag,
} from "../ui/primitives";
import { ChevronDown, ChevronRight, SearchIcon } from "../ui/icons";

export function Services({
  model,
  catalog,
  incidents,
  history,
  syncedAt,
  onNavigate,
  onRefresh,
  onAsk,
}: {
  model: EstateModel;
  catalog: Catalog;
  incidents: Incident[];
  history: Record<string, MetricSeries[]>;
  syncedAt?: string;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAsk: () => void;
}) {
  /**
   * Which incident each connection belongs to.
   *
   * A table of twelve rows shows three failing connections as three problems.
   * Two of them may be the same one, and an operator who splits them up starts
   * three investigations into a single failure.
   */
  const incidentOf = useMemo(() => {
    const map = new Map<string, Incident>();
    for (const incident of incidents) {
      if (incident.profileIds.length < 2) continue;
      for (const id of incident.profileIds) map.set(id, incident);
    }
    return map;
  }, [incidents]);

  const [groupBy, setGroupBy] = useState<GroupBy>("category");
  const [query, setQuery] = useState("");
  const [onlyProblems, setOnlyProblems] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return model.services.filter((service) => {
      if (onlyProblems && !isProblem(service.health)) return false;
      if (!needle) return true;
      return (
        service.snapshot.profileName.toLowerCase().includes(needle) ||
        service.category.toLowerCase().includes(needle) ||
        service.ownership.team.toLowerCase().includes(needle) ||
        catalog.label(service.snapshot.kind).toLowerCase().includes(needle) ||
        service.snapshot.summary.toLowerCase().includes(needle)
      );
    });
  }, [model.services, query, onlyProblems, catalog]);

  /**
   * Whether the operator has narrowed the list — not whether the list happens
   * to be narrower. The header used to show "of 8" only while the counts
   * differed, so under "Needs attention" it appeared and vanished as
   * connections recovered and broke again. What the header looks like should
   * change when somebody does something, never because a poll came back.
   */
  const filtering = onlyProblems || query.trim() !== "";

  const groups = useMemo(
    () => groupServices(filtered, groupBy, catalog),
    [filtered, groupBy, catalog],
  );

  const toggle = (id: string) =>
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  return (
    <>
      <ScreenHeader
        onAsk={onAsk}
        title="Services"
        meta={
          <>
            <span className="num">{filtered.length}</span>
            {filtering && (
              <>
                <span className="text-ink-4/50">of</span>
                <span className="num">{model.services.length}</span>
              </>
            )}
            connections
          </>
        }
        syncedAt={syncedAt}
        live
        onRefresh={onRefresh}
      >
        <div className="relative">
          <SearchIcon
            size={13}
            className="pointer-events-none absolute top-1/2 left-2 -translate-y-1/2 text-ink-4"
          />
          <Input
            className="w-56 pl-7"
            placeholder="Filter by name, type, team…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <Segmented
          label="Attention"
          value={onlyProblems ? "problems" : "all"}
          onChange={(v) => setOnlyProblems(v === "problems")}
          options={[
            { value: "all", label: "All" },
            { value: "problems", label: "Needs attention" },
          ]}
        />
        <Segmented
          label="Group by"
          value={groupBy}
          onChange={setGroupBy}
          options={(Object.keys(GROUP_LABELS) as GroupBy[]).map((key) => ({
            value: key,
            label: GROUP_LABELS[key],
            title: `Group by ${GROUP_LABELS[key].toLowerCase()}`,
          }))}
        />
      </ScreenHeader>

      <Screen>
        {groups.length === 0 ? (
          <Panel>
            <Empty
              title="No connections match"
              hint={
                model.services.length === 0
                  ? "Add a connection in Settings to start collecting."
                  : "Clear the filter to see the rest of the estate."
              }
            />
          </Panel>
        ) : (
          /* One table for every group rather than one per group: a single
             column model is what keeps the numbers aligned down the whole
             page, and the header only has to be read once.

             Columns come and go with the width this screen has — not the
             window's, which counts the sidebar and the assistant dock as room
             the table does not get. Whatever still does not fit scrolls:
             clipped, Trend and Checked were simply gone below 1280px. */
          <div className="panel overflow-x-auto">
            <table className="w-full border-collapse">
              <thead className="sticky top-0 z-[1] bg-panel">
                <tr className="border-b border-line">
                  <th className="w-px" />
                  <th className="label cell-pad-x py-1.5 text-left font-semibold">Connection</th>
                  <th className="label cell-pad-x py-1.5 text-left font-semibold">Status</th>
                  <th className="label hidden cell-pad-x py-1.5 text-left font-semibold @6xl:table-cell">
                    Type
                  </th>
                  <th className="label hidden cell-pad-x py-1.5 text-left font-semibold @5xl:table-cell">
                    Owner
                  </th>
                  <th className="label hidden cell-pad-x py-1.5 text-right font-semibold @lg:table-cell">
                    Components
                  </th>
                  <th className="label cell-pad-x py-1.5 text-right font-semibold">Open</th>
                  <th className="label hidden cell-pad-x py-1.5 text-right font-semibold @2xl:table-cell">
                    Key metric
                  </th>
                  <th className="label hidden cell-pad-x py-1.5 text-right font-semibold @3xl:table-cell">
                    Trend
                  </th>
                  <th className="label hidden cell-pad-x py-1.5 text-right font-semibold @4xl:table-cell">
                    Checked
                  </th>
                </tr>
              </thead>

              {groups.map((group) => (
                <tbody key={group.key}>
                  <tr className="border-y border-line bg-page/60">
                    <td colSpan={10} className="cell-pad-x py-1.5">
                      <div className="flex items-center gap-2">
                        <Dot health={group.health} />
                        <h2 className="text-xs font-semibold text-ink">{group.label}</h2>
                        <span className="num text-2xs text-ink-4">{group.services.length}</span>
                        {group.issues > 0 && (
                          <Tag color="var(--color-warn)">{group.issues} open</Tag>
                        )}
                        <span className="ml-auto text-2xs text-ink-4">
                          {group.services.reduce((sum, s) => sum + s.entities.length, 0)} components
                        </span>
                      </div>
                    </td>
                  </tr>

                  {group.services.map((service) => (
                    <ServiceRow
                      key={service.snapshot.profileId}
                      service={service}
                      catalog={catalog}
                      incident={incidentOf.get(service.snapshot.profileId)}
                      series={history[service.snapshot.profileId] ?? []}
                      expanded={expanded.has(service.snapshot.profileId)}
                      onToggle={() => toggle(service.snapshot.profileId)}
                      onOpen={() =>
                        onNavigate({ name: "service", profileId: service.snapshot.profileId })
                      }
                    />
                  ))}
                </tbody>
              ))}
            </table>
          </div>
        )}
      </Screen>
    </>
  );
}

function ServiceRow({
  service,
  catalog,
  incident,
  series,
  expanded,
  onToggle,
  onOpen,
}: {
  service: ServiceView;
  catalog: Catalog;
  /** Set only when this connection shares an incident with others. */
  incident?: Incident;
  series: MetricSeries[];
  expanded: boolean;
  onToggle: () => void;
  onOpen: () => void;
}) {
  const { snapshot, ownership } = service;
  const descriptor = catalog.get(snapshot.kind);

  const primary = useMemo(
    () =>
      [...snapshot.metrics].sort((a, b) => Number(isBreaching(b)) - Number(isBreaching(a)))[0],
    [snapshot.metrics],
  );
  const trend = primary ? series.find((s) => s.key === primary.key) : undefined;
  const problems = service.entities.filter((e) => isProblem(e.health));

  // A failing row is lit from its left edge in its own colour — the hue,
  // across the first two cells and fading out by the end of the name. Two
  // cells rather than the row, because a gradient on a <tr> is painted per
  // cell in some engines and would restart in every column.
  const failing = service.health === "down" || service.health === "degraded";
  const hue = healthColor(service.health);
  const lit = (from: number, to: number) =>
    failing
      ? {
          background: `linear-gradient(to right, color-mix(in srgb, ${hue} ${from}%, transparent), color-mix(in srgb, ${hue} ${to}%, transparent))`,
        }
      : undefined;

  return (
    <>
      <tr
        className="group border-b border-line/50 row-hover"
        style={{ height: "var(--row-h)" }}
      >
        <td
          className="pl-2"
          style={
            failing ? { ...lit(16, 13), boxShadow: `inset 2px 0 0 ${hue}` } : undefined
          }
        >
          <button
            onClick={onToggle}
            disabled={problems.length === 0}
            title={
              problems.length === 0
                ? "No failing components"
                : expanded
                  ? "Hide components"
                  : `Show ${problems.length} failing components`
            }
            className="grid h-5 w-5 place-items-center rounded-md text-ink-4 transition-colors hover:text-ink disabled:opacity-25"
          >
            {expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
          </button>
        </td>

        <td className="max-w-64 cell-pad-x" style={lit(13, 0)}>
          <button onClick={onOpen} className="flex min-w-0 items-center gap-2 text-left">
            <ServiceMark descriptor={descriptor} size={17} />
            <span className="truncate text-xs font-medium text-ink transition-colors group-hover:text-accent">
              {snapshot.profileName}
            </span>
          </button>
        </td>

        <td className="cell-pad-x">
          <div className="flex items-center gap-2">
            <HealthBadge health={service.health} />
            {incident && (
              <Tag
                color={
                  incident.severity === "critical" ? "var(--color-crit)" : "var(--color-warn)"
                }
                title={
                  service.health === "healthy"
                    ? `${incident.title} — this connection is reporting healthy but is grouped into the incident, which is still open.`
                    : `${incident.title} — ${incident.profileIds.length} connections, started on ${incident.leadProfileId}`
                }
              >
                {/* A connection name, not a shout: uppercasing an identifier
                    makes it a different string from the one in every other
                    column. And a connection whose collector reports healthy
                    while it sits in an open incident is not a contradiction to
                    leave the reader to resolve — the tag says which it is. */}
                {incident.leadProfileId === snapshot.profileId
                  ? "incident origin"
                  : service.health === "healthy"
                    ? "in open incident"
                    : `with ${incident.leadProfileId}`}
              </Tag>
            )}
            <span
              className="hidden max-w-64 truncate text-2xs text-ink-4 @7xl:inline"
              title={snapshot.error ?? snapshot.summary}
            >
              {snapshot.error ?? snapshot.summary}
            </span>
          </div>
        </td>

        <td className="hidden cell-pad-x text-xs text-ink-3 @6xl:table-cell">{descriptor.label}</td>

        <td className="hidden max-w-48 cell-pad-x @5xl:table-cell">
          <div className="flex items-center gap-1.5">
            <span
              className={cx("truncate text-xs", ownership.assigned ? "text-ink-3" : "text-ink-4 italic")}
              title={ownership.assigned ? `${ownership.team} · ${ownership.owner}` : "No team assigned"}
            >
              {ownership.team}
            </span>
            {ownership.assigned && (
              <>
                <Tag title={`Environment: ${environmentLabel(ownership.environment)}`}>
                  {environmentLabel(ownership.environment)}
                </Tag>
                <Tag title={`Criticality tier ${criticalityLabel(ownership.criticality)}`}>
                  {criticalityLabel(ownership.criticality)}
                </Tag>
              </>
            )}
          </div>
        </td>

        <td className="num hidden cell-pad-x text-right text-ink-3 @lg:table-cell">
          {service.entities.length}
        </td>

        <td className="cell-pad-x text-right">
          {service.issues.length > 0 ? (
            <span className="num text-warn">{service.issues.length}</span>
          ) : (
            <span className="text-ink-4/60">—</span>
          )}
        </td>

        <td className="hidden cell-pad-x text-right @2xl:table-cell">
          {primary ? (
            <span className="whitespace-nowrap" title={primary.label}>
              <span className={cx("num", isBreaching(primary) && "text-warn")}>
                {formatMetricValue(primary.value, primary.unit)}
              </span>
              <span className="ml-1.5 text-2xs text-ink-4">{primary.label}</span>
            </span>
          ) : (
            <span className="text-ink-4/60">—</span>
          )}
        </td>

        <td className="hidden cell-pad-x @3xl:table-cell">
          <div className="flex justify-end">
            {trend ? (
              <Sparkline
                points={trend.points}
                width={64}
                height={18}
                color={DATA_INK}
                warnAbove={trend.warnAbove}
              />
            ) : (
              <span className="text-ink-4/60">—</span>
            )}
          </div>
        </td>

        <td className="num hidden cell-pad-x text-right whitespace-nowrap text-ink-4 @4xl:table-cell">
          {formatRelative(snapshot.checkedAt)}
        </td>
      </tr>

      {expanded && problems.length > 0 && (
        <tr className="border-b border-line/50 bg-page/40">
          <td />
          <td colSpan={9} className="cell-pad-x py-2">
            <ul className="flex flex-col gap-1">
              {problems.slice(0, 12).map((entity) => (
                <li key={entity.id} className="flex min-w-0 items-baseline gap-2 text-xs">
                  <span title={healthLabel(entity.health)} className="translate-y-px">
                    <Dot health={entity.health} size={7} />
                  </span>
                  <span className="shrink-0 font-medium text-ink-2">{entity.name}</span>
                  <span className="text-2xs text-ink-4">{entity.section}</span>
                  <span className="min-w-0 truncate text-ink-4" title={entity.summary}>
                    {entity.summary}
                  </span>
                </li>
              ))}
              {problems.length > 12 && (
                <li className="text-2xs text-ink-4">
                  and {problems.length - 12} more —{" "}
                  <button onClick={onOpen} className="text-accent hover:underline">
                    open {snapshot.profileName}
                  </button>
                </li>
              )}
            </ul>
          </td>
        </tr>
      )}
    </>
  );
}
