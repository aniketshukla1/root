import { useMemo, useState } from "react";

import type { Catalog } from "../catalog";
import { DATA_INK, seriesColor, SERIES_CAP } from "../catalog";
import { formatMetricValue } from "../format";
import { bySignal } from "../golden";
import type { EstateModel, ServiceView } from "../model";
import type { View } from "../nav";
import type { MetricSeries, MetricUnit } from "../types";
import { rangesAcross } from "../ranges";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Horizon, Rate } from "../ui/capacity";
import { staticKind, SteadyStats, TimeSeries, type Domain, type Series } from "../ui/charts";
import {
  Button,
  cx,
  Empty,
  HealthBadge,
  Input,
  Panel,
  Segmented,
  ServiceMark,
  Tag,
} from "../ui/primitives";
import { SearchIcon } from "../ui/icons";

/**
 * Two ways to look at the same series, and they answer different questions:
 *
 *   byConnection — "how is orders-db doing?"  One panel per connection, its own
 *                  metrics inside. This is the default, because it is what you
 *                  want the other 90% of the time.
 *   byMetric     — "how does CPU compare across the fleet?"  One chart per
 *                  metric key, a line per connection reporting it.
 *
 * Mixing them was the old behaviour and it made both worse.
 */
type Mode = "connection" | "metric";

export function Metrics({
  history,
  model,
  catalog,
  minutes,
  onMinutesChange,
  syncedAt,
  onNavigate,
  onRefresh,
  onAsk,
}: {
  history: Record<string, MetricSeries[]>;
  model: EstateModel;
  catalog: Catalog;
  minutes: number;
  onMinutesChange: (minutes: number) => void;
  syncedAt?: string;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAsk: () => void;
}) {
  const [mode, setMode] = useState<Mode>("connection");
  const [query, setQuery] = useState("");
  const [scope, setScope] = useState<string>("all");

  const needle = query.trim().toLowerCase();

  // The ring holds a fixed sample count, so how far back a connection reaches
  // depends on its poll interval. Offer only windows something can satisfy —
  // a 24h button over a 3h ring renders the same chart with a bigger label.
  const ranges = useMemo(
    () => rangesAcross(model.services.map((s) => s.profile?.pollIntervalSecs ?? 15)),
    [model.services],
  );

  // Fixed per render pass rather than per chart, so every panel on the screen
  // shares one axis.
  const domain = useMemo<Domain>(() => {
    const now = Date.now();
    return [now - minutes * 60000, now];
  }, [minutes, history]);

  const byConnection = useMemo(() => {
    return model.services
      .filter((service) => scope === "all" || scope === service.snapshot.profileId)
      .map((service) => ({
        service,
        series: (history[service.snapshot.profileId] ?? []).filter(
          (s) => s.points.length > 1 && (!needle || s.label.toLowerCase().includes(needle)),
        ),
      }))
      .filter((entry) => entry.series.length > 0)
      .sort(
        (a, b) =>
          b.series.length - a.series.length ||
          a.service.snapshot.profileName.localeCompare(b.service.snapshot.profileName),
      );
  }, [model.services, history, scope, needle]);

  const byMetric = useMemo<MetricGroup[]>(() => {
    const groups = new Map<string, MetricGroup>();

    for (const service of model.services) {
      const profileId = service.snapshot.profileId;
      if (scope !== "all" && scope !== profileId) continue;

      for (const series of history[profileId] ?? []) {
        if (series.points.length < 2) continue;
        // Keyed by unit as well as key: a percentage and a byte count must
        // never end up sharing a y-axis.
        const groupKey = `${series.key}:${series.unit}`;
        const group = groups.get(groupKey) ?? {
          key: groupKey,
          label: series.label,
          unit: series.unit,
          entries: [],
        };
        group.entries.push({
          profileId,
          profileName: service.snapshot.profileName,
          kind: service.snapshot.kind,
          series,
        });
        groups.set(groupKey, group);
      }
    }

    return [...groups.values()]
      .filter((group) => !needle || group.label.toLowerCase().includes(needle))
      .sort((a, b) => b.entries.length - a.entries.length || a.label.localeCompare(b.label));
  }, [history, model.services, scope, needle]);

  const totalSeries = byConnection.reduce((sum, e) => sum + e.series.length, 0);
  const shared = byMetric.filter((g) => g.entries.length > 1).length;

  return (
    <>
      <ScreenHeader
        onAsk={onAsk}
        title="Metrics"
        meta={
          mode === "connection" ? (
            <>
              <span className="num">{byConnection.length}</span> connections
              <span className="text-ink-4/50">·</span>
              <span className="num">{totalSeries}</span> series
            </>
          ) : (
            <>
              <span className="num">{byMetric.length}</span> metrics
              {shared > 0 && (
                <>
                  <span className="text-ink-4/50">·</span>
                  <span className="num">{shared}</span> shared across connections
                </>
              )}
            </>
          )
        }
        syncedAt={syncedAt}
        onRefresh={onRefresh}
      >
        <Segmented
          label="View"
          value={mode}
          onChange={setMode}
          options={[
            {
              value: "connection",
              label: "By connection",
              title: "One panel per connection, showing its own metrics",
            },
            {
              value: "metric",
              label: "Compare",
              title: "One chart per metric, with a line per connection reporting it",
            },
          ]}
        />
        <div className="relative">
          <SearchIcon
            size={13}
            className="pointer-events-none absolute top-1/2 left-2 -translate-y-1/2 text-ink-4"
          />
          <Input
            className="w-44 pl-7"
            placeholder="Filter metrics…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <select
          value={scope}
          onChange={(e) => setScope(e.target.value)}
          className="h-7 cursor-pointer rounded-md border border-line-strong bg-page px-2 text-xs text-ink focus:border-accent focus:outline-none"
        >
          <option value="all">All connections</option>
          {model.services.map((service) => (
            <option key={service.snapshot.profileId} value={service.snapshot.profileId}>
              {service.snapshot.profileName}
            </option>
          ))}
        </select>
        <Segmented
          label="Time range"
          value={String(minutes)}
          onChange={(v) => onMinutesChange(Number(v))}
          options={ranges.map((r) => ({ value: String(r.minutes), label: r.label }))}
        />
      </ScreenHeader>

      <Screen className="flex flex-col gap-3">
        {(mode === "connection" ? byConnection.length : byMetric.length) === 0 ? (
          <Panel>
            <Empty
              title={query ? "No metrics match" : "No metric history yet"}
              hint={
                query
                  ? "Clear the filter to see every series."
                  : "History builds as connections poll. Give it a couple of collection cycles."
              }
            />
          </Panel>
        ) : mode === "connection" ? (
          byConnection.map(({ service, series }) => (
            <ConnectionMetrics
              key={service.snapshot.profileId}
              service={service}
              series={series}
              catalog={catalog}
              domain={domain}
              onOpen={() =>
                onNavigate({
                  name: "service",
                  profileId: service.snapshot.profileId,
                  tab: "metrics",
                })
              }
            />
          ))
        ) : (
          <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(420px,1fr))]">
            {byMetric.map((group) => (
              <ComparisonChart
                key={group.key}
                group={group}
                catalog={catalog}
                domain={domain}
              />
            ))}
          </div>
        )}
      </Screen>
    </>
  );
}

// ------------------------------------------------------- one connection's own

function ConnectionMetrics({
  service,
  series,
  catalog,
  domain,
  onOpen,
}: {
  service: ServiceView;
  series: MetricSeries[];
  catalog: Catalog;
  domain: Domain;
  onOpen: () => void;
}) {
  const descriptor = catalog.get(service.snapshot.kind);
  const line = DATA_INK;
  const breaching = series.filter(
    (s) => s.warnAbove !== undefined && s.latest > s.warnAbove,
  ).length;
  const plotted = bySignal(
    series.filter((s) => !staticKind(s)),
    service.snapshot.kind,
  ).flatMap(([, list]) => list);
  const steady = series.filter((s) => staticKind(s));

  return (
    <Panel
      title={
        <span className="flex min-w-0 items-center gap-2">
          <ServiceMark descriptor={descriptor} size={17} />
          {/* An identifier, not chrome: the panel title is uppercase mono,
              and "ORDERS-API" is not the name of anything. */}
          {/* Truncated here, not only by the header above it: that clips with
              `overflow-hidden`, which cuts a name off mid-word with no
              ellipsis — and a name that silently becomes a shorter name is
              worse than one that visibly ran out of room. */}
          <span
            className="min-w-0 truncate font-mono text-xs tracking-normal text-ink normal-case"
            title={service.snapshot.profileName}
          >
            {service.snapshot.profileName}
          </span>
        </span>
      }
      subtitle={`${descriptor.label} · ${series.length} metric${series.length === 1 ? "" : "s"}`}
      actions={
        <>
          {breaching > 0 && <Tag color="var(--color-warn)">{breaching} over threshold</Tag>}
          <HealthBadge health={service.health} />
          <Button variant="ghost" onClick={onOpen}>
            Open
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {plotted.length > 0 && (
          <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(268px,1fr))]">
            {plotted.map((s) => {
              const over = s.warnAbove !== undefined && s.latest > s.warnAbove;
              return (
                <div key={s.key} className="min-w-0 rounded-md border border-line bg-page p-2.5">
                  <div className="flex items-baseline gap-2">
                    <span className="min-w-0 truncate text-xs text-ink-3" title={s.label}>
                      {s.label}
                    </span>
                    {/* The fitted slope, not the first-to-last delta that used
                        to sit here — see Rate. Absent when the history cannot
                        support a fit, which is its own answer. */}
                    <span className="num ml-auto shrink-0 text-2xs">
                      <Rate trend={s.trend} unit={s.unit} />
                    </span>
                    <span
                      className={cx("num shrink-0 text-lg", over ? "text-warn" : "text-ink")}
                      title={
                        s.warnAbove !== undefined
                          ? `Threshold ${formatMetricValue(s.warnAbove, s.unit)}`
                          : undefined
                      }
                    >
                      {formatMetricValue(s.latest, s.unit)}
                    </span>
                  </div>

                  {/* One series per chart here, so no legend box — the label above
                      names it. */}
                  <TimeSeries
                    series={[
                      {
                        key: s.key,
                        label: s.label,
                        color: line,
                        points: s.points,
                        warnAbove: s.warnAbove,
                      },
                    ]}
                    unit={s.unit}
                    domain={domain}
                    height={104}
                    showLegend={false}
                  />

                  <div className="mt-1 flex gap-3 text-2xs text-ink-4">
                    <span>
                      min <span className="num text-ink-3">{formatMetricValue(s.min, s.unit)}</span>
                    </span>
                    <span>
                      max <span className="num text-ink-3">{formatMetricValue(s.max, s.unit)}</span>
                    </span>
                    {s.warnAbove !== undefined && (
                      <span className="ml-auto">
                        limit{" "}
                        <span className="num text-warn">
                          {formatMetricValue(s.warnAbove, s.unit)}
                        </span>
                        {" · "}
                        <Horizon trend={s.trend} unit={s.unit} />
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
        {/* Flat lines and clocks are one number each; they get a line, not a panel. */}
        <SteadyStats series={steady} />
      </div>
    </Panel>
  );
}

// ------------------------------------------------------------ across the fleet

interface MetricGroup {
  key: string;
  label: string;
  unit: MetricUnit;
  entries: { profileId: string; profileName: string; kind: string; series: MetricSeries }[];
}

function ComparisonChart({
  group,
  catalog,
  domain,
}: {
  group: MetricGroup;
  catalog: Catalog;
  domain: Domain;
}) {
  // Slots are handed out in fixed order to the series actually plotted, so
  // hiding one never repaints the others. Past the cap the palette stops being
  // separable, so the remainder is summarised rather than drawn.
  const shown = group.entries.slice(0, SERIES_CAP);
  const overflow = group.entries.length - shown.length;

  const series: Series[] = shown.map((entry, index) => ({
    key: entry.profileId,
    label: entry.profileName,
    color: seriesColor(index),
    points: entry.series.points,
    warnAbove: entry.series.warnAbove,
    icon: <ServiceMark descriptor={catalog.get(entry.kind)} size={14} />,
  }));
  const trends = new Map(shown.map((entry) => [entry.profileId, entry.series]));

  const breaching = shown.filter(
    (e) => e.series.warnAbove !== undefined && e.series.latest > e.series.warnAbove,
  ).length;

  return (
    <Panel
      title={group.label}
      subtitle={
        group.entries.length > 1 ? `${group.entries.length} connections` : "1 connection"
      }
      actions={breaching > 0 && <Tag color="var(--color-warn)">{breaching} over threshold</Tag>}
    >
      <TimeSeries
        series={series}
        unit={group.unit}
        domain={domain}
        height={150}
        // Comparing the same metric across the fleet, the question is which
        // one gets there first — so the extra column is the horizon.
        legendExtra={{
          label: "Limit in",
          render: (s) => {
            const metric = trends.get(s.key);
            return metric && <Horizon trend={metric.trend} unit={metric.unit} />;
          },
        }}
      />
      {overflow > 0 && (
        <p className="mt-1 text-2xs text-ink-4">
          {overflow} more connection{overflow === 1 ? "" : "s"} report this metric — scope to one
          connection to see {overflow === 1 ? "it" : "them"}.
        </p>
      )}
    </Panel>
  );
}
