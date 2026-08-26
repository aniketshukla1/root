import { useMemo } from "react";

import { healthColor, healthLabel, isProblem, markColor } from "../catalog";
import type { Catalog } from "../catalog";
import { formatMetricValue, formatRelative } from "../format";
import { isBreaching } from "../format";
import { lanesFor } from "../model";
import type { EstateModel, Entity, ServiceView } from "../model";
import type { View } from "../nav";
import type { Alert, Health, HealthRun, LogEvent, MetricSeries } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { AlertList } from "../ui/AlertList";
import { LogHistogram, levelColor, Sparkline, type Bucket } from "../ui/charts";
import { Lanes, Ticks } from "../ui/plots";
import {
  Button,
  cx,
  Dot,
  Empty,
  HealthBadge,
  Panel,
  ServiceMark,
} from "../ui/primitives";
import { ChevronRight } from "../ui/icons";

export function Overview({
  model,
  catalog,
  history,
  buckets,
  healthRuns,
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
  healthRuns: Record<string, HealthRun[]>;
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
        {/* Hero: what the estate is right now, beside what every connection
            has been doing for the window. */}
        <section className="grid gap-3 xl:grid-cols-[340px_1fr]" style={{ minHeight: 300 }}>
          <EstatePanel model={model} catalog={catalog} onNavigate={onNavigate} />
          <LanesPanel
            model={model}
            minutes={minutes}
            healthRuns={healthRuns}
            onNavigate={onNavigate}
          />
        </section>

        <section>
          <BandTitle
            title="Needs attention"
            action={{ label: "All services", onClick: () => onNavigate({ name: "services" }) }}
          />
          <div className="grid gap-2 [grid-template-columns:repeat(auto-fill,minmax(272px,1fr))]">
            {[...model.services]
              .filter((s) => s.health !== "healthy" || s.issues.length > 0)
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

// ------------------------------------------------------------------- hero

/**
 * The estate panel answers only what nothing else on this screen does: how
 * long this has been going on, what failed first, and what caused it. The
 * ribbon, the lanes and the alert list already carry the aggregate, so
 * repeating it here was the old strip's whole problem.
 */
function EstatePanel({
  model,
  catalog,
  onNavigate,
}: {
  model: EstateModel;
  catalog: Catalog;
  onNavigate: (view: View) => void;
}) {
  const { counts } = model;

  // The oldest still-firing alert is what "how long" means.
  const oldest = useMemo(() => {
    const active = model.alerts.filter((a) => a.active);
    if (active.length === 0) return undefined;
    return active.reduce((a, b) => (Date.parse(a.firstSeen) <= Date.parse(b.firstSeen) ? a : b));
  }, [model.alerts]);

  const state: Health = counts.down > 0 ? "down" : counts.degraded > 0 ? "degraded" : "healthy";
  const first = oldest
    ? model.services.find((s) => s.snapshot.profileId === oldest.profileId)
    : undefined;

  return (
    <section className="panel flex min-h-0 flex-col p-4">
      <div className="flex items-center gap-2.5">
        <span className="label">Estate</span>
        <span className="num ml-auto text-2xs text-ink-4">{counts.connections} connections</span>
      </div>

      <div className="mt-3.5 flex items-center gap-2">
        <Dot health={state} size={11} />
        <span
          className="font-mono text-xs tracking-[.16em] uppercase"
          style={{ color: healthColor(state) }}
        >
          {healthLabel(state)}
        </span>
        {oldest && (
          <span className="num text-xs text-ink-4">
            since {new Date(oldest.firstSeen).toLocaleTimeString([], { hour12: false })}
          </span>
        )}
      </div>

      <div className="mt-1.5 flex items-baseline gap-2.5">
        <span className="num text-[40px] leading-none font-medium tracking-[-.04em] text-ink">
          {oldest ? formatRelative(oldest.firstSeen).replace(" ago", "") : "—"}
        </span>
        <span className="label tracking-[.14em]">{oldest ? "unresolved" : "all clear"}</span>
      </div>

      <div className="mt-3.5 flex items-center gap-3.5">
        {([["down", counts.down], ["degraded", counts.degraded], ["unknown", counts.unknown], ["healthy", counts.healthy]] as const).map(
          ([health, n]) => (
            <span key={health} className="flex items-center gap-1.5" title={healthLabel(health)}>
              <Dot health={health} size={8} />
              <span className="num text-xs text-ink-2">{n}</span>
            </span>
          ),
        )}
      </div>

      <div className="my-3.5 h-px bg-line" />

      {first && oldest ? (
        <>
          <div className="label mb-2">First failure</div>
          <div className="flex items-center gap-2">
            <ServiceMark descriptor={catalog.get(first.snapshot.kind)} size={17} />
            <span className="text-[12.5px] text-ink">{first.snapshot.profileName}</span>
            <span className="num truncate text-xs text-ink-4" title={oldest.title}>
              {oldest.title}
            </span>
          </div>
          <p className="mt-2 text-[11.5px] leading-snug text-ink-3">{oldest.detail}</p>
        </>
      ) : (
        <p className="text-[12.5px] text-ink-3">
          Nothing is alerting. Every connection reported on its last poll.
        </p>
      )}

      <div className="mt-auto flex gap-1.5 pt-3.5">
        <Button className="flex-1" onClick={() => onNavigate({ name: "alerts" })}>
          All alerts
        </Button>
        {first && (
          <Button
            className="flex-1"
            onClick={() => onNavigate({ name: "service", profileId: first.snapshot.profileId })}
          >
            Open {first.snapshot.profileName}
          </Button>
        )}
      </div>
    </section>
  );
}

/**
 * Every connection's window, time-aligned. Reading across a lane gives one
 * service's hour; reading down a column gives what everything was doing at a
 * single moment — which is the whole point of correlation.
 */
function LanesPanel({
  model,
  minutes,
  healthRuns,
  onNavigate,
}: {
  model: EstateModel;
  minutes: number;
  healthRuns: Record<string, HealthRun[]>;
  onNavigate: (view: View) => void;
}) {
  const lanes = useMemo(
    () => lanesFor(model, healthRuns, minutes * 60_000),
    [model, healthRuns, minutes],
  );
  const shown = lanes.slice(0, 9);
  const rest = lanes.length - shown.length;

  return (
    <Panel
      title="Lanes"
      subtitle={`last ${minutes >= 60 ? `${minutes / 60}h` : `${minutes}m`} · state per connection`}
      actions={
        <div className="flex items-center gap-2.5">
          <LaneKey />
          <button
            onClick={() => onNavigate({ name: "correlate", at: new Date().toISOString() })}
            className="num flex items-center gap-1 text-2xs text-ink-4 transition-colors hover:text-accent"
          >
            Correlate
            <ChevronRight size={11} />
          </button>
        </div>
      }
      bodyClassName="px-4 pt-3 pb-3"
    >
      <Lanes
        lanes={shown.map((l) => ({ ...l, runs: l.runs }))}
        labelWidth={132}
        footer={
          <>
            <Ticks count={60} />
            <div className="mt-1 flex justify-between">
              <span className="num text-[9.5px] text-ink-4">
                −{minutes >= 60 ? `${minutes / 60}h` : `${minutes}m`}
              </span>
              {rest > 0 && (
                <span className="num text-[9.5px] text-ink-4">+{rest} more healthy</span>
              )}
              <span className="num text-[9.5px] text-ink-2">now</span>
            </div>
          </>
        }
      />
    </Panel>
  );
}

function LaneKey() {
  const swatch = (background: string, label: string, extra?: string) => (
    <span key={label} className="num flex items-center gap-1 text-[9.5px] text-ink-4">
      <span className={cx("h-2 w-2.5", extra)} style={{ background }} />
      {label}
    </span>
  );
  return (
    <div className="flex items-center gap-2">
      {swatch("var(--color-lane-ok)", "healthy")}
      {swatch("var(--color-warn)", "degraded")}
      {swatch("var(--color-crit)", "down")}
      <span className="num flex items-center gap-1 text-[9.5px] text-ink-4">
        <span
          className="h-2 w-2.5"
          style={{
            background: "var(--color-cell-todo)",
            backgroundImage:
              "repeating-linear-gradient(45deg, var(--color-ink-4) 0 1.6px, transparent 1.6px 5px)",
          }}
        />
        no data
      </span>
    </div>
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
