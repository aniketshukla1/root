import { useMemo, useState } from "react";

import type { Catalog } from "../catalog";
import { healthLabel, isProblem, markColor } from "../catalog";
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
import type { MetricSeries } from "../types";
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
  history,
  syncedAt,
  onNavigate,
  onRefresh,
  onAsk,
}: {
  model: EstateModel;
  catalog: Catalog;
  history: Record<string, MetricSeries[]>;
  syncedAt?: string;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAsk: () => void;
}) {
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
            {filtered.length !== model.services.length && (
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
          value={onlyProblems ? "problems" : "all"}
          onChange={(v) => setOnlyProblems(v === "problems")}
          options={[
            { value: "all", label: "All" },
            { value: "problems", label: "Needs attention" },
          ]}
        />
        <Segmented
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
             page, and the header only has to be read once. */
          <div className="panel overflow-hidden">
            <table className="w-full border-collapse">
              <thead className="sticky top-0 z-[1] bg-panel">
                <tr className="border-b border-line">
                  <th className="w-px" />
                  <th className="label px-3 py-1.5 text-left font-semibold">Connection</th>
                  <th className="label px-3 py-1.5 text-left font-semibold">Status</th>
                  <th className="label hidden px-3 py-1.5 text-left font-semibold xl:table-cell">
                    Type
                  </th>
                  <th className="label hidden px-3 py-1.5 text-left font-semibold lg:table-cell">
                    Owner
                  </th>
                  <th className="label px-3 py-1.5 text-right font-semibold">Components</th>
                  <th className="label px-3 py-1.5 text-right font-semibold">Open</th>
                  <th className="label hidden px-3 py-1.5 text-right font-semibold md:table-cell">
                    Key metric
                  </th>
                  <th className="label px-3 py-1.5 text-right font-semibold">Trend</th>
                  <th className="label px-3 py-1.5 text-right font-semibold">Checked</th>
                </tr>
              </thead>

              {groups.map((group) => (
                <tbody key={group.key}>
                  <tr className="border-y border-line bg-page/60">
                    <td colSpan={10} className="px-3 py-1.5">
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
  series,
  expanded,
  onToggle,
  onOpen,
}: {
  service: ServiceView;
  catalog: Catalog;
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

  return (
    <>
      <tr
        className="group border-b border-line/50 hover:bg-white/[0.035]"
        style={{ height: "var(--row-h)" }}
      >
        <td className="pl-2">
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
            className="grid h-5 w-5 place-items-center rounded text-ink-4 transition-colors hover:text-ink disabled:opacity-25"
          >
            {expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
          </button>
        </td>

        <td className="max-w-64 px-3">
          <button onClick={onOpen} className="flex min-w-0 items-center gap-2 text-left">
            <ServiceMark descriptor={descriptor} size={17} />
            <span className="truncate text-xs font-medium text-ink transition-colors group-hover:text-accent">
              {snapshot.profileName}
            </span>
          </button>
        </td>

        <td className="px-3">
          <div className="flex items-center gap-2">
            <HealthBadge health={service.health} />
            <span
              className="hidden max-w-64 truncate text-2xs text-ink-4 2xl:inline"
              title={snapshot.error ?? snapshot.summary}
            >
              {snapshot.error ?? snapshot.summary}
            </span>
          </div>
        </td>

        <td className="hidden px-3 text-xs text-ink-3 xl:table-cell">{descriptor.label}</td>

        <td className="hidden max-w-48 px-3 lg:table-cell">
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

        <td className="num px-3 text-right text-ink-3">{service.entities.length}</td>

        <td className="px-3 text-right">
          {service.issues.length > 0 ? (
            <span className="num text-warn">{service.issues.length}</span>
          ) : (
            <span className="text-ink-4/60">—</span>
          )}
        </td>

        <td className="hidden px-3 text-right md:table-cell">
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

        <td className="px-3">
          <div className="flex justify-end">
            {trend ? (
              <Sparkline
                points={trend.points}
                width={64}
                height={18}
                color={markColor(descriptor.accent)}
                warnAbove={trend.warnAbove}
              />
            ) : (
              <span className="text-ink-4/60">—</span>
            )}
          </div>
        </td>

        <td className="num px-3 text-right whitespace-nowrap text-ink-4">
          {formatRelative(snapshot.checkedAt)}
        </td>
      </tr>

      {expanded && problems.length > 0 && (
        <tr className="border-b border-line/50 bg-page/40">
          <td />
          <td colSpan={9} className="px-3 py-2">
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
