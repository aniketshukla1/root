import { useMemo } from "react";

import { healthColor, healthLabel, isProblem, markColor } from "../catalog";
import type { Catalog } from "../catalog";
import { formatMetricValue, formatRelative } from "../format";
import { isBreaching } from "../format";
import type { EstateModel, Entity, ServiceView } from "../model";
import type { View } from "../nav";
import type { Alert, LogEvent, MetricSeries } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { AlertList } from "../ui/AlertList";
import { LogHistogram, levelColor, Sparkline, type Bucket } from "../ui/charts";
import {
  Button,
  cx,
  Dot,
  Empty,
  HealthBadge,
  Panel,
  ServiceMark,
  Stat,
} from "../ui/primitives";
import { ChevronRight } from "../ui/icons";

export function Overview({
  model,
  catalog,
  history,
  buckets,
  logs,
  loading,
  minutes,
  syncedAt,
  onNavigate,
  onRefresh,
  onAcknowledge,
}: {
  model: EstateModel;
  catalog: Catalog;
  history: Record<string, MetricSeries[]>;
  buckets: Bucket[];
  logs: LogEvent[];
  loading: boolean;
  minutes: number;
  syncedAt?: string;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAcknowledge: (alertId: string) => void;
}) {
  const { counts } = model;

  const errors = useMemo(
    () => logs.filter((l) => l.level === "error" || l.level === "fatal").slice(0, 14),
    [logs],
  );

  if (loading && model.services.length === 0) {
    return (
      <>
        <ScreenHeader title="Overview" />
        <Screen>
          <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
            {Array.from({ length: 6 }).map((_, i) => (
              <div key={i} className="panel h-[74px] animate-pulse opacity-40" />
            ))}
          </div>
        </Screen>
      </>
    );
  }

  if (model.services.length === 0) {
    return (
      <>
        <ScreenHeader title="Overview" />
        <Screen>
          <Panel>
            <Empty
              title="No connections yet"
              hint="Add a connection in Settings to start collecting health, metrics, and logs."
            />
            <div className="flex justify-center pb-4">
              <Button variant="primary" onClick={() => onNavigate({ name: "settings" })}>
                Add a connection
              </Button>
            </div>
          </Panel>
        </Screen>
      </>
    );
  }

  return (
    <>
      <ScreenHeader
        title="Overview"
        meta={
          <>
            <span className="num">{counts.connections}</span> connections
            <span className="text-ink-4/50">·</span>
            <span className="num">{counts.entities}</span> components
          </>
        }
        syncedAt={syncedAt}
        live
        onRefresh={onRefresh}
      />

      <Screen className="flex flex-col gap-3">
        <StatusStrip model={model} onNavigate={onNavigate} />

        <section>
          <BandTitle
            title="Connections"
            action={{ label: "All services", onClick: () => onNavigate({ name: "services" }) }}
          />
          <div className="grid gap-2 [grid-template-columns:repeat(auto-fill,minmax(272px,1fr))]">
            {[...model.services]
              .sort(
                (a, b) =>
                  Number(isProblem(b.health)) - Number(isProblem(a.health)) ||
                  b.issues.length - a.issues.length ||
                  a.snapshot.profileName.localeCompare(b.snapshot.profileName),
              )
              .map((service) => (
                <ConnectionTile
                  key={service.snapshot.profileId}
                  service={service}
                  catalog={catalog}
                  series={history[service.snapshot.profileId] ?? []}
                  onOpen={() =>
                    onNavigate({ name: "service", profileId: service.snapshot.profileId })
                  }
                />
              ))}
          </div>
        </section>

        <div className="grid gap-3 xl:grid-cols-[3fr_2fr]">
          <AlertsPanel
            alerts={model.alerts}
            catalog={catalog}
            onNavigate={onNavigate}
            onAcknowledge={onAcknowledge}
          />
          <ActivityPanel
            buckets={buckets}
            errors={errors}
            minutes={minutes}
            onNavigate={onNavigate}
          />
        </div>

        <ComponentMatrix
          model={model}
          catalog={catalog}
          onNavigate={onNavigate}
        />
      </Screen>
    </>
  );
}

function BandTitle({
  title,
  action,
}: {
  title: string;
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div className="mb-1.5 flex h-5 items-center">
      <h2 className="text-xs font-semibold text-ink-2">{title}</h2>
      {action && (
        <button
          onClick={action.onClick}
          className="ml-auto flex items-center gap-0.5 text-xs text-ink-4 transition-colors hover:text-accent"
        >
          {action.label}
          <ChevronRight size={12} />
        </button>
      )}
    </div>
  );
}

// -------------------------------------------------------------- status strip

function StatusStrip({
  model,
  onNavigate,
}: {
  model: EstateModel;
  onNavigate: (view: View) => void;
}) {
  const { counts } = model;
  const total = Math.max(1, counts.connections);
  const coverage = Math.round((counts.assigned / total) * 100);

  // Ordered worst-first so the eye lands on the problem end of the bar.
  const distribution = [
    { key: "down", n: counts.down, color: "var(--color-crit)", label: "Down" },
    { key: "degraded", n: counts.degraded, color: "var(--color-warn)", label: "Degraded" },
    { key: "unknown", n: counts.unknown, color: "var(--color-unknown)", label: "Unknown" },
    { key: "healthy", n: counts.healthy, color: "var(--color-ok)", label: "Healthy" },
  ].filter((d) => d.n > 0);

  return (
    <div className="panel flex flex-wrap items-center gap-x-7 gap-y-3 px-4 py-3">
      <Stat label="Connections" value={counts.connections} />

      <div className="min-w-44 flex-1">
        <div className="flex items-baseline gap-2">
          {distribution.map((d) => (
            <span key={d.key} className="num text-[19px] leading-tight" style={{ color: d.color }}>
              {d.n}
            </span>
          ))}
        </div>
        {/* A 2px gap between segments in the surface colour, so adjacent
            counts stay countable without a border darkening the fill. */}
        <div className="mt-1.5 flex h-1.5 gap-[2px] overflow-hidden">
          {distribution.map((d) => (
            <span
              key={d.key}
              title={`${d.n} ${d.label.toLowerCase()}`}
              className="h-full rounded-full first:rounded-l-full last:rounded-r-full"
              style={{ background: d.color, flexGrow: d.n }}
            />
          ))}
        </div>
        <div className="mt-1 flex flex-wrap gap-x-2.5 gap-y-0.5">
          {distribution.map((d) => (
            <span key={d.key} className="flex items-center gap-1 text-2xs text-ink-4">
              <span
                className="h-1.5 w-1.5 shrink-0 rounded-full"
                style={{ background: d.color }}
              />
              {d.label}
            </span>
          ))}
        </div>
      </div>

      <Divider />

      <button
        onClick={() => onNavigate({ name: "alerts" })}
        className="text-left transition-opacity hover:opacity-80"
      >
        <Stat
          label="Active alerts"
          value={counts.alerts}
          tone={counts.critical > 0 ? "var(--color-crit)" : undefined}
        />
      </button>
      <Stat
        label="Unacknowledged"
        value={counts.unacknowledged}
        tone={counts.unacknowledged > 0 ? "var(--color-warn)" : undefined}
      />

      <Divider />

      <Stat label="Components" value={counts.entities} />
      <Stat label="Production" value={counts.production} />
      <Stat
        label="Ownership"
        value={`${coverage}%`}
        tone={coverage < 100 ? "var(--color-ink-2)" : undefined}
        hint={`${counts.assigned} of ${counts.connections} connections name a responsible team`}
      />
    </div>
  );
}

function Divider() {
  return <span className="hidden h-8 w-px shrink-0 bg-line lg:block" />;
}

// ---------------------------------------------------------- connection tile

function ConnectionTile({
  service,
  catalog,
  series,
  onOpen,
}: {
  service: ServiceView;
  catalog: Catalog;
  series: MetricSeries[];
  onOpen: () => void;
}) {
  const { snapshot } = service;
  const descriptor = catalog.get(snapshot.kind);

  // Breaching metrics first — the reason this connection is interesting should
  // be the number you see, not whatever the collector listed first.
  const metrics = useMemo(
    () =>
      [...snapshot.metrics]
        .sort((a, b) => Number(isBreaching(b)) - Number(isBreaching(a)))
        .slice(0, 2),
    [snapshot.metrics],
  );

  const trend = useMemo(() => {
    const primary = metrics[0];
    return primary ? series.find((s) => s.key === primary.key) : undefined;
  }, [metrics, series]);

  return (
    <button
      onClick={onOpen}
      className={cx(
        "panel group relative flex flex-col gap-1.5 px-2.5 py-2 text-left transition-colors hover:bg-raised",
        isProblem(service.health) && "border-l-2",
      )}
      style={
        isProblem(service.health)
          ? { borderLeftColor: healthColor(service.health) }
          : undefined
      }
    >
      <div className="flex min-w-0 items-center gap-2">
        <ServiceMark descriptor={descriptor} size={18} />
        <span className="truncate text-[13px] font-medium text-ink">{snapshot.profileName}</span>
        <span className="ml-auto shrink-0">
          <HealthBadge health={service.health} />
        </span>
      </div>

      <p className="line-clamp-1 text-xs text-ink-3" title={snapshot.error ?? snapshot.summary}>
        {snapshot.error ?? snapshot.summary}
      </p>

      <div className="flex items-end gap-3">
        <div className="flex min-w-0 flex-1 gap-3">
          {metrics.map((metric) => (
            <div key={metric.key} className="min-w-0">
              <div
                className={cx("num text-[13px] leading-none", isBreaching(metric) && "text-warn")}
              >
                {formatMetricValue(metric.value, metric.unit)}
              </div>
              <div className="mt-0.5 truncate text-2xs text-ink-4" title={metric.label}>
                {metric.label}
              </div>
            </div>
          ))}
          {metrics.length === 0 && (
            <div className="text-2xs text-ink-4">
              {service.entities.length} component{service.entities.length === 1 ? "" : "s"}
            </div>
          )}
        </div>

        {trend && (
          <Sparkline
            points={trend.points}
            width={72}
            height={22}
            color={markColor(descriptor.accent)}
            warnAbove={trend.warnAbove}
          />
        )}
      </div>

      <div className="flex items-center gap-1.5 text-2xs text-ink-4">
        <span className="truncate">{service.ownership.team}</span>
        <span className="text-ink-4/50">·</span>
        <span className="num shrink-0">{formatRelative(snapshot.checkedAt)}</span>
        {service.issues.length > 0 && (
          <span className="num ml-auto shrink-0 text-warn">
            {service.issues.length} open
          </span>
        )}
      </div>
    </button>
  );
}

// ---------------------------------------------------------------- alerts

function AlertsPanel({
  alerts,
  catalog,
  onNavigate,
  onAcknowledge,
}: {
  alerts: Alert[];
  catalog: Catalog;
  onNavigate: (view: View) => void;
  onAcknowledge: (alertId: string) => void;
}) {
  return (
    <Panel
      title="Active alerts"
      subtitle={alerts.length > 0 ? `${alerts.length}` : undefined}
      actions={
        alerts.length > 6 && (
          <button
            onClick={() => onNavigate({ name: "alerts" })}
            className="flex items-center gap-0.5 text-xs text-ink-4 transition-colors hover:text-accent"
          >
            View all
            <ChevronRight size={12} />
          </button>
        )
      }
      flush
      bodyClassName="overflow-y-auto max-h-[290px]"
    >
      <AlertList
        alerts={alerts}
        catalog={catalog}
        onAcknowledge={onAcknowledge}
        onOpenService={(profileId) => onNavigate({ name: "service", profileId, tab: "alerts" })}
        limit={8}
      />
    </Panel>
  );
}

// -------------------------------------------------------------- log activity

function ActivityPanel({
  buckets,
  errors,
  minutes,
  onNavigate,
}: {
  buckets: Bucket[];
  errors: LogEvent[];
  minutes: number;
  onNavigate: (view: View) => void;
}) {
  const range =
    minutes >= 1440 ? "last 24 hours" : minutes >= 60 ? `last ${minutes / 60}h` : `last ${minutes}m`;

  return (
    <Panel
      title="Log activity"
      subtitle={range}
      actions={
        <button
          onClick={() => onNavigate({ name: "logs" })}
          className="flex items-center gap-0.5 text-xs text-ink-4 transition-colors hover:text-accent"
        >
          Open logs
          <ChevronRight size={12} />
        </button>
      }
      flush
      bodyClassName="flex flex-col max-h-[290px]"
    >
      <div className="border-b border-line px-3 pt-2 pb-1">
        <LogHistogram buckets={buckets} height={64} />
        <div className="mt-1.5 flex flex-wrap gap-x-2.5 gap-y-0.5">
          {(["fatal", "error", "warn", "info", "debug", "trace"] as const).map((level) => (
            <span key={level} className="flex items-center gap-1 text-2xs text-ink-4 capitalize">
              <span
                className="h-1.5 w-1.5 shrink-0 rounded-full"
                style={{ background: levelColor(level) }}
              />
              {level}
            </span>
          ))}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {errors.length === 0 ? (
          <Empty title="No errors buffered" hint="Nothing at error or fatal in the buffer." />
        ) : (
          <ul className="divide-y divide-line/60">
            {errors.map((event) => (
              <li key={event.id} className="row-hover flex items-baseline gap-2 px-3 py-1">
                <span className="num shrink-0 text-2xs text-ink-4">
                  {new Date(event.timestamp).toLocaleTimeString([], { hour12: false })}
                </span>
                <span
                  className="num shrink-0 text-2xs"
                  style={{ color: levelColor(event.level) }}
                  title={event.level}
                >
                  {event.level.toUpperCase()}
                </span>
                <span className="num shrink-0 truncate text-2xs text-ink-4" title={event.source}>
                  {event.source}
                </span>
                <span className="num min-w-0 flex-1 truncate text-xs text-ink-2" title={event.message}>
                  {event.message}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Panel>
  );
}

// ------------------------------------------------------------ component grid

/**
 * Every monitored component in the estate, grouped by connection. At this size
 * colour alone would not be readable, so each cell carries the health shape as
 * well, and the title attribute names the state in words.
 */
function ComponentMatrix({
  model,
  catalog,
  onNavigate,
}: {
  model: EstateModel;
  catalog: Catalog;
  onNavigate: (view: View) => void;
}) {
  const groups = useMemo(() => {
    const byService = new Map<string, Entity[]>();
    for (const entity of model.entities) {
      byService.set(entity.profileId, [...(byService.get(entity.profileId) ?? []), entity]);
    }
    return model.services
      .map((service) => ({
        service,
        entities: (byService.get(service.snapshot.profileId) ?? []).sort(
          (a, b) =>
            Number(isProblem(b.health)) - Number(isProblem(a.health)) ||
            a.name.localeCompare(b.name),
        ),
      }))
      .filter((g) => g.entities.length > 0)
      .sort(
        (a, b) =>
          Number(isProblem(b.service.health)) - Number(isProblem(a.service.health)) ||
          b.entities.length - a.entities.length,
      );
  }, [model]);

  if (groups.length === 0) return null;

  return (
    <Panel
      title="Components"
      subtitle={`${model.counts.entities} across ${groups.length} connections`}
      actions={
        <div className="flex items-center gap-2.5">
          {(["down", "degraded", "unknown", "healthy"] as const).map((health) => (
            <span key={health} className="flex items-center gap-1 text-2xs text-ink-4">
              <Dot health={health} size={7} />
              {healthLabel(health)}
            </span>
          ))}
        </div>
      }
    >
      <div className="flex flex-wrap gap-x-6 gap-y-3">
        {groups.map(({ service, entities }) => (
          <div key={service.snapshot.profileId} className="min-w-0">
            <button
              onClick={() =>
                onNavigate({ name: "service", profileId: service.snapshot.profileId })
              }
              className="mb-1 flex items-center gap-1.5 text-xs text-ink-3 transition-colors hover:text-accent"
            >
              <ServiceMark descriptor={catalog.get(service.snapshot.kind)} size={14} />
              <span className="truncate">{service.snapshot.profileName}</span>
              <span className="num text-2xs text-ink-4">{entities.length}</span>
            </button>
            <div className="flex flex-wrap gap-[3px]">
              {entities.map((entity) => (
                <button
                  key={entity.id}
                  title={`${entity.name} — ${healthLabel(entity.health)}\n${entity.section}: ${entity.summary}`}
                  onClick={() => onNavigate({ name: "service", profileId: entity.profileId })}
                  className="grid h-4 w-4 place-items-center rounded-[3px] transition-transform hover:scale-125"
                  style={{
                    background: `color-mix(in oklab, ${healthColor(entity.health)} 22%, transparent)`,
                  }}
                >
                  <Dot health={entity.health} size={7} />
                </button>
              ))}
            </div>
          </div>
        ))}
      </div>
    </Panel>
  );
}
