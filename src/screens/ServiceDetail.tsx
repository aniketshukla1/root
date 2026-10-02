import { useCallback, useEffect, useMemo, useState } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { DATA_INK, isProblem } from "../catalog";
import { formatDateTime, formatMetricValue, formatRelative, isBreaching } from "../format";
import { criticalityLabel, environmentLabel, type EstateModel } from "../model";
import type { ServiceTab, View } from "../nav";
import { spanOf } from "../ranges";
import type {
  ConfigDocument,
  Finding,
  Incident,
  LogEvent,
  MetricSeries,
  Row,
  RowAction,
  TopologyView,
} from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { ServiceDashboard } from "./ServiceDashboard";
import { AlertList } from "../ui/AlertList";
import { levelColor, TimeSeries, type Domain } from "../ui/charts";
import { Horizon, Rate } from "../ui/capacity";
import { ConfigTab } from "../ui/config";
import { SectionList } from "../ui/SectionTable";
import {
  balancedTracks,
  Button,
  Chip,
  cx,
  Dot,
  Empty,
  HealthBadge,
  Panel,
  Sep,
  ServiceMark,
  Stat,
  Tabs,
  Tag,
} from "../ui/primitives";
import { ChevronLeft, ChevronRight, ExternalIcon, TimelineIcon } from "../ui/icons";

export function ServiceDetail({
  profileId,
  tab = "dashboard",
  model,
  catalog,
  incidents,
  topology,
  logs,
  history,
  onNavigate,
  onRefresh,
  onAsk,
  onAcknowledge,
}: {
  profileId: string;
  tab?: ServiceTab;
  model: EstateModel;
  catalog: Catalog;
  incidents: Incident[];
  topology: TopologyView;
  logs: LogEvent[];
  history: MetricSeries[];
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAsk: () => void;
  onAcknowledge: (alertId: string) => void;
}) {
  const [pending, setPending] = useState<{ action: RowAction; row: Row } | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);

  const [documents, setDocuments] = useState<ConfigDocument[]>([]);
  const [findings, setFindings] = useState<Finding[]>([]);
  const [configLoading, setConfigLoading] = useState(false);

  /**
   * Read on demand rather than polled.
   *
   * Config is not telemetry: it changes when somebody changes it, and reading
   * every declared file on every collection cycle would put the app in a hot
   * loop over the operator's disk for a value that is the same every time.
   */
  const loadConfig = useCallback(async () => {
    setConfigLoading(true);
    try {
      const [docs, found] = await Promise.all([
        api.getConfigDocuments(profileId),
        api.getFindings(profileId),
      ]);
      setDocuments(docs);
      setFindings(found);
    } catch (error) {
      console.error("config load failed:", error);
    } finally {
      setConfigLoading(false);
    }
  }, [profileId]);

  // The tab count has to be right before the tab is opened, so this runs on
  // mount rather than when the tab is selected.
  useEffect(() => {
    loadConfig();
  }, [loadConfig]);

  const service = model.services.find((s) => s.snapshot.profileId === profileId);

  /**
   * The larger thing this connection is part of, if any.
   *
   * Without it a drill-down is a dead end: an operator who arrives here from a
   * link, a bookmark or a search sees one broken connection and starts working
   * on it, with no sign that three others turned with it and that this one may
   * not even be where it started.
   */
  const incident = useMemo(
    () => incidents.find((i) => i.profileIds.includes(profileId)),
    [incidents, profileId],
  );

  const serviceLogs = useMemo(
    () => logs.filter((event) => event.profileId === profileId),
    [logs, profileId],
  );

  if (!service) {
    return (
      <>
        <ScreenHeader title="Connection" />
        <Screen>
          <Panel>
            <Empty
              title="Connection not found"
              hint="It may have been removed, or it has not reported a snapshot yet."
            />
            <div className="flex justify-center pb-4">
              <Button onClick={() => onNavigate({ name: "services" })}>Back to services</Button>
            </div>
          </Panel>
        </Screen>
      </>
    );
  }

  const { snapshot } = service;
  const descriptor = catalog.get(snapshot.kind);

  const runAction = async () => {
    if (!pending) return;
    setBusy(true);
    try {
      const text = await api.runRowAction(profileId, pending.action.tool, pending.action.args);
      setResult({ ok: true, text });
      onRefresh();
    } catch (error) {
      setResult({ ok: false, text: errorMessage(error) });
    } finally {
      setBusy(false);
      setPending(null);
    }
  };

  const counts = {
    components: service.entities.length,
    problems: service.entities.filter((e) => isProblem(e.health)).length,
    alerts: service.alerts.length,
    logs: serviceLogs.length,
    metrics: history.filter((s) => s.points.length > 1).length,
  };

  return (
    <>
      <ScreenHeader
        onAsk={onAsk}
        title={
          <span className="flex items-center gap-2">
            <button
              onClick={() => onNavigate({ name: "services" })}
              title="Back to services"
              className="text-ink-4 transition-colors hover:text-ink"
            >
              <ChevronLeft size={16} />
            </button>
            <ServiceMark descriptor={descriptor} size={20} />
            {snapshot.profileName}
          </span>
        }
        meta={
          <>
            {descriptor.label}
            <Sep />
            {service.category}
            <Sep />
            {/* Reserved for the same reason as the one in ScreenHeader: it
                changes width on every poll, and the latency after it moved. */}
            <span className="num inline-block min-w-[8ch]">
              {formatRelative(snapshot.checkedAt)}
            </span>
            <Sep />
            <span className="num" title="Time the last collection cycle took">
              {snapshot.latencyMs}ms
            </span>
          </>
        }
        onRefresh={() => {
          api.refreshConnection(profileId).catch(() => undefined);
          onRefresh();
        }}
      >
        <HealthBadge health={service.health} />
      </ScreenHeader>

      <Screen className="flex flex-col gap-3">
        {incident && incident.profileIds.length > 1 && (
          <button
            type="button"
            onClick={() => onNavigate({ name: "overview" })}
            className="hue row-hover flex items-center gap-2.5 rounded-md border border-line bg-panel px-3 py-2 text-left"
            style={
              {
                borderLeftWidth: 2,
                borderLeftColor:
                  incident.severity === "critical" ? "var(--color-crit)" : "var(--color-warn)",
                "--hue":
                  incident.severity === "critical" ? "var(--color-crit)" : "var(--color-warn)",
              } as React.CSSProperties
            }
          >
            <Dot health={incident.health} size={9} />
            <span className="label shrink-0">Part of an incident</span>
            <span className="min-w-0 flex-1 truncate text-xs text-ink-2" title={incident.title}>
              {incident.title}
            </span>
            <span className="num shrink-0 text-2xs text-ink-4">
              {incident.leadProfileId === profileId
                ? "this turned first"
                : `started on ${incident.leadProfileId}`}
            </span>
            <ChevronRight size={12} style={{ color: "var(--color-ink-4)" }} />
          </button>
        )}

        {snapshot.error && (
          <div
            className="rounded-md border px-3 py-2 text-xs"
            style={{
              borderColor: "color-mix(in oklab, var(--color-crit) 40%, transparent)",
              background: "color-mix(in oklab, var(--color-crit) 10%, transparent)",
            }}
          >
            <span className="font-medium text-crit">Collection failed.</span>{" "}
            <span className="num text-ink-2">{snapshot.error}</span>
          </div>
        )}

        {result && (
          <div
            className="flex items-center gap-2 rounded-md border border-line bg-panel px-3 py-2 text-xs"
            role="status"
          >
            <span className={result.ok ? "text-ok" : "text-crit"}>
              {result.ok ? "Action completed." : "Action failed."}
            </span>
            <span className="num min-w-0 flex-1 truncate text-ink-2" title={result.text}>
              {result.text}
            </span>
            <Button variant="ghost" onClick={() => setResult(null)}>
              Dismiss
            </Button>
          </div>
        )}

        {/* The Dashboard tab opens with Signals — the same four numbers, with
            their trend, threshold and sparkline. Repeating them in the bar
            above it said each one three times on one screen, so the bar keeps
            only the summary and ownership while that tab is open. */}
        <SummaryBar service={service} history={history} showMetrics={tab !== "dashboard"} />

        {/* `shrink-0` matters: Screen is a flex column, so without it this
            panel shrinks to fit and `overflow-hidden` silently clips the tab
            content instead of letting the screen scroll. */}
        <div className="panel shrink-0 overflow-hidden">
          <Tabs
            value={tab}
            onChange={(next) => onNavigate({ name: "service", profileId, tab: next })}
            options={[
              { value: "dashboard", label: "Dashboard" },
              { value: "components", label: "Components", count: counts.problems },
              { value: "alerts", label: "Alerts", count: counts.alerts },
              { value: "metrics", label: "Metrics", count: counts.metrics },
              { value: "logs", label: "Logs", count: counts.logs },
              // Two different things called config, so neither is called that.
              // "Config" is the target's own; "Connection" is root's record of
              // how to reach it.
              { value: "config", label: "Config", count: findings.length || undefined },
              { value: "configuration", label: "Connection" },
            ]}
            panelId="service-tab-panel"
          />

          {/* Named so the tabs can point at it: `role="tab"` without
              `aria-controls` describes a relationship it never states. */}
          <div id="service-tab-panel" role="tabpanel" tabIndex={-1} className="p-3">
            {tab === "dashboard" && (
              <ServiceDashboard
                service={service}
                catalog={catalog}
                logs={logs}
                onNavigate={onNavigate}
                onAcknowledge={onAcknowledge}
              />
            )}

            {tab === "components" && (
              snapshot.sections.length === 0 ? (
                <Empty
                  title="No inventory reported"
                  hint={`${descriptor.label} reports service-level metrics only, or the last collection returned nothing.`}
                />
              ) : (
                <SectionList
                  sections={snapshot.sections}
                  onAction={(action, row) => setPending({ action, row })}
                  onOpenRow={(section, row) =>
                    onNavigate({
                      name: "component",
                      profileId,
                      sectionId: section.id,
                      rowId: row.id,
                    })
                  }
                  onOpenTrace={(traceId) =>
                    onNavigate({ name: "trace", profileId, traceId })
                  }
                  onOpenProfile={(query) =>
                    onNavigate({ name: "profile", profileId, query })
                  }
                />
              )
            )}

            {tab === "alerts" && (
              <AlertsTab
                service={service}
                catalog={catalog}
                onAcknowledge={onAcknowledge}
                onOpenComponents={() =>
                  onNavigate({ name: "service", profileId, tab: "components" })
                }
              />
            )}

            {tab === "metrics" && <MetricsTab history={history} />}

            {tab === "logs" && (
              <LogsTab logs={serviceLogs} onNavigate={onNavigate} />
            )}

            {tab === "config" && (
              <ConfigTab
                documents={documents}
                findings={findings}
                loading={configLoading}
                onRefresh={loadConfig}
              />
            )}

            {tab === "configuration" && (
              <ConfigurationTab
                service={service}
                catalog={catalog}
                topology={topology}
                onOpenSettings={() =>
                  onNavigate({ name: "settings", profileId: snapshot.profileId })
                }
                onOpenService={(id) => onNavigate({ name: "service", profileId: id })}
              />
            )}
          </div>
        </div>
      </Screen>

      {pending && (
        <ConfirmAction
          action={pending.action}
          row={pending.row}
          target={snapshot.profileName}
          busy={busy}
          onCancel={() => setPending(null)}
          onConfirm={runAction}
        />
      )}
    </>
  );
}

// --------------------------------------------------------------- summary bar

function SummaryBar({
  service,
  history,
  showMetrics,
}: {
  service: EstateModel["services"][number];
  history: MetricSeries[];
  showMetrics: boolean;
}) {
  const { snapshot, ownership } = service;

  return (
    <div className="panel flex shrink-0 flex-wrap items-center gap-x-6 gap-y-3 px-4 py-3">
      {/* A floor rather than `min-w-0`: the metric strip beside this does not
          shrink, so without one the summary is squeezed to a single word per
          line instead of wrapping the strip below it. */}
      <div className="min-w-64 flex-1">
        <p className="text-base text-ink-2">{snapshot.summary}</p>
        <div className="mt-1 flex flex-wrap items-center gap-1.5 text-2xs text-ink-4">
          <span className={ownership.assigned ? "" : "italic"}>{ownership.team}</span>
          {ownership.assigned && (
            <>
              <Sep />
              <span>{ownership.owner}</span>
              <Tag>{environmentLabel(ownership.environment)}</Tag>
              <Tag>{criticalityLabel(ownership.criticality)}</Tag>
              {ownership.contact && (
                <>
                  <Sep />
                  <span className="num">{ownership.contact}</span>
                </>
              )}
              {ownership.runbookUrl && (
                <a
                  href={ownership.runbookUrl}
                  target="_blank"
                  rel="noreferrer"
                  className="flex items-center gap-1 text-accent hover:underline"
                >
                  Runbook
                  <ExternalIcon size={10} />
                </a>
              )}
            </>
          )}
        </div>
      </div>

      {showMetrics && snapshot.metrics.length > 0 && (
        <div className="flex flex-wrap gap-x-5 gap-y-2">
          {snapshot.metrics.slice(0, 6).map((metric) => {
            const series = history.find((s) => s.key === metric.key);
            const over = isBreaching(metric);
            return (
              <Stat
                key={metric.key}
                size="md"
                label={metric.label}
                value={formatMetricValue(metric.value, metric.unit)}
                tone={over ? "var(--color-warn)" : undefined}
                hint={
                  metric.warnAbove !== undefined
                    ? `Threshold ${formatMetricValue(metric.warnAbove, metric.unit)}`
                    : metric.label
                }
              >
                {series?.trend && (
                  <span className="num flex gap-1.5 pb-0.5 text-2xs">
                    <Rate trend={series.trend} unit={metric.unit} />
                    {metric.warnAbove !== undefined && (
                      <Horizon trend={series.trend} unit={metric.unit} />
                    )}
                  </span>
                )}
              </Stat>
            );
          })}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- tabs

/**
 * Everything wrong with this one connection, in one place: the alerts its
 * thresholds raised, plus the failing inventory rows that never produced an
 * alert. Those are separate signals in the backend, and a responder needs both
 * without having to know the difference.
 */
function AlertsTab({
  service,
  catalog,
  onAcknowledge,
  onOpenComponents,
}: {
  service: EstateModel["services"][number];
  catalog: Catalog;
  onAcknowledge: (alertId: string) => void;
  onOpenComponents: () => void;
}) {
  const unalerted = service.issues.filter((issue) => !issue.alert);

  return (
    <div className="grid gap-3 xl:grid-cols-2">
      <Panel
        title="Alerts"
        subtitle={service.alerts.length > 0 ? `${service.alerts.length} active` : undefined}
        flush
      >
        <AlertList
          alerts={service.alerts}
          catalog={catalog}
          onAcknowledge={onAcknowledge}
          showConnection={false}
          emptyTitle="No active alerts"
          emptyHint="No threshold on this connection is currently breached."
        />
      </Panel>

      <Panel
        title="Failing components"
        subtitle={unalerted.length > 0 ? `${unalerted.length} with no alert` : undefined}
        actions={
          unalerted.length > 0 && (
            <Button variant="ghost" onClick={onOpenComponents}>
              View inventory
            </Button>
          )
        }
        flush
      >
        {unalerted.length === 0 ? (
          <Empty
            title="Nothing failing without an alert"
            hint="Every degraded or down component here already has an alert behind it."
          />
        ) : (
          <ul className="divide-y divide-line">
            {unalerted.map((issue) => (
              <li key={issue.id} className="row-hover flex items-start gap-2.5 px-3 py-2">
                <span className="mt-0.5 shrink-0" title={issue.health}>
                  <Dot health={issue.health} />
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex min-w-0 items-center gap-1.5">
                    <span className="truncate text-base text-ink">{issue.name}</span>
                    <Tag>{issue.section}</Tag>
                  </div>
                  <p className="line-clamp-1 text-xs text-ink-3" title={issue.summary}>
                    {issue.summary}
                  </p>
                </div>
                <span className="num shrink-0 text-2xs text-ink-4">
                  {formatRelative(issue.firstSeen)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </Panel>
    </div>
  );
}

function MetricsTab({ history }: { history: MetricSeries[] }) {
  const plotted = history.filter((s) => s.points.length > 1);
  const line = DATA_INK;

  // This tab has no range control of its own — the Dashboard tab owns that —
  // so the axis is whatever the data actually covers rather than a window the
  // ring may not have filled.
  const span = spanOf(history);
  const domain: Domain =
    span.from !== undefined && span.to !== undefined && span.to > span.from
      ? [span.from, span.to]
      : [Date.now() - 3600_000, Date.now()];

  if (plotted.length === 0) {
    return (
      <Empty
        title="No history yet"
        hint="Series build as this connection polls. Give it a couple of collection cycles."
      />
    );
  }

  return (
    <div className="grid gap-3" style={balancedTracks(plotted.length, 360)}>
      {plotted.map((series) => (
        <Panel
          key={series.key}
          title={series.label}
          actions={
            <span className="flex items-center gap-2">
              <span className="num text-2xs">
                <Rate trend={series.trend} unit={series.unit} />
              </span>
              {series.warnAbove !== undefined && (
                <span className="num text-2xs">
                  <Horizon trend={series.trend} unit={series.unit} />
                </span>
              )}
              <span
                className={cx(
                  "num text-xs",
                  series.warnAbove !== undefined && series.latest > series.warnAbove
                    ? "text-warn"
                    : "text-ink",
                )}
              >
                {formatMetricValue(series.latest, series.unit)}
              </span>
            </span>
          }
        >
          {/* One series, so no legend box — the panel title names it. */}
          <TimeSeries
            series={[
              {
                key: series.key,
                label: series.label,
                color: line,
                points: series.points,
                warnAbove: series.warnAbove,
              },
            ]}
            unit={series.unit}
            domain={domain}
            height={132}
            showLegend={false}
          />
          <div className="mt-1.5 flex gap-4 border-t border-line pt-1.5 text-2xs text-ink-4">
            <span>
              min <span className="num text-ink-3">{formatMetricValue(series.min, series.unit)}</span>
            </span>
            <span>
              max <span className="num text-ink-3">{formatMetricValue(series.max, series.unit)}</span>
            </span>
            {series.warnAbove !== undefined && (
              <span className="ml-auto">
                threshold{" "}
                <span className="num text-warn">
                  {formatMetricValue(series.warnAbove, series.unit)}
                </span>
              </span>
            )}
          </div>
        </Panel>
      ))}
    </div>
  );
}

function LogsTab({
  logs,
  onNavigate,
}: {
  logs: LogEvent[];
  onNavigate: (view: View) => void;
}) {
  if (logs.length === 0) {
    return (
      <Empty
        title="No lines buffered"
        hint="This connection has not produced log output, or its collector does not tail logs."
      />
    );
  }

  return (
    <div className="max-h-[560px] overflow-y-auto rounded-md border border-line">
      <table className="w-full border-collapse">
        <tbody>
          {logs.slice(0, 400).map((event) => (
            <tr
              key={event.id}
              className="group border-b border-line/40 align-baseline row-hover"
            >
              <td className="w-px px-3 py-1 whitespace-nowrap">
                <button
                  onClick={() => onNavigate({ name: "correlate", at: event.timestamp })}
                  title={`${formatDateTime(event.timestamp)} — correlate across every connection`}
                  className="num flex items-center gap-1 text-2xs text-ink-4 transition-colors hover:text-accent"
                >
                  {new Date(event.timestamp).toLocaleTimeString([], { hour12: false })}
                  <TimelineIcon
                    size={11}
                    className="opacity-0 transition-opacity group-hover:opacity-100"
                  />
                </button>
              </td>
              <td className="w-px px-1 py-1">
                <span className="num text-2xs" style={{ color: levelColor(event.level) }}>
                  {event.level.toUpperCase()}
                </span>
              </td>
              <td className="w-px max-w-32 px-2 py-1">
                <span className="num block truncate text-2xs text-ink-4" title={event.source}>
                  {event.source}
                </span>
              </td>
              <td className="num px-2 py-1 break-all text-ink-2">{event.message}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ConfigurationTab({
  service,
  catalog,
  topology,
  onOpenSettings,
  onOpenService,
}: {
  service: EstateModel["services"][number];
  catalog: Catalog;
  topology: TopologyView;
  onOpenSettings: () => void;
  onOpenService: (profileId: string) => void;
}) {
  const { snapshot, profile, ownership } = service;
  const descriptor = catalog.get(snapshot.kind);

  const rows: { label: string; value: React.ReactNode }[] = [
    { label: "Type", value: descriptor.label },
    { label: "Category", value: service.category },
    { label: "Endpoint", value: <span className="num">{profile?.endpoint || "—"}</span> },
    { label: "Username", value: <span className="num">{profile?.username || "—"}</span> },
    {
      label: "Credential",
      value: profile?.hasSecret ? "Stored in the OS keychain" : "None",
    },
    {
      label: "Poll interval",
      value: <span className="num">{profile?.pollIntervalSecs ?? descriptor.defaultPollSecs}s</span>,
    },
    { label: "Enabled", value: profile ? (profile.enabled ? "Yes" : "No") : "—" },
    { label: "Last check", value: <span className="num">{formatDateTime(snapshot.checkedAt)}</span> },
  ];

  const options = Object.entries(profile?.options ?? {}).filter(
    ([, value]) => value !== null && value !== undefined && value !== "",
  );

  return (
    <div className="grid gap-3 lg:grid-cols-2">
      <Panel
        title="Connection"
        actions={
          <Button variant="ghost" onClick={onOpenSettings}>
            Edit in Settings
          </Button>
        }
      >
        <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1.5 text-xs">
          {rows.map((row) => (
            <div key={row.label} className="contents">
              <dt className="text-ink-4">{row.label}</dt>
              <dd className="min-w-0 truncate text-ink-2">{row.value}</dd>
            </div>
          ))}
        </dl>
      </Panel>

      {/* Dependencies are configuration, and since the correlator started
          reading them they are the configuration that decides whether an
          incident can be explained at all. Leaving them visible only inside
          the Settings edit form meant the one screen that says "no
          dependencies declared" could not show you what WAS declared. */}
      <Panel
        title="Dependencies"
        subtitle="declared, not discovered"
        actions={
          <Button variant="ghost" onClick={onOpenSettings}>
            Declare
          </Button>
        }
      >
        <DependencyList
          label="Depends on"
          empty="Nothing declared. Without this, a failure here cannot be explained by anything upstream — only by when it happened."
          ids={topology.dependsOn[snapshot.profileId] ?? []}
          model={service}
          onOpen={onOpenService}
        />
        <div className="mt-3 border-t border-line pt-3">
          <DependencyList
            label="Depended on by"
            empty="Nothing declares a dependency on this connection."
            ids={topology.dependents[snapshot.profileId] ?? []}
            model={service}
            onOpen={onOpenService}
            derived
          />
        </div>
      </Panel>

      <Panel title="Ownership">
        {ownership.assigned ? (
          <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1.5 text-xs">
            <dt className="text-ink-4">Team</dt>
            <dd className="text-ink-2">{ownership.team}</dd>
            <dt className="text-ink-4">Responder</dt>
            <dd className="text-ink-2">{ownership.owner}</dd>
            <dt className="text-ink-4">Contact</dt>
            <dd className="num text-ink-2">{ownership.contact || "—"}</dd>
            <dt className="text-ink-4">Environment</dt>
            <dd className="text-ink-2">{environmentLabel(ownership.environment)}</dd>
            <dt className="text-ink-4">Criticality</dt>
            <dd className="text-ink-2">Tier {criticalityLabel(ownership.criticality)}</dd>
            <dt className="text-ink-4">Runbook</dt>
            <dd className="min-w-0 truncate">
              {ownership.runbookUrl ? (
                <a
                  href={ownership.runbookUrl}
                  target="_blank"
                  rel="noreferrer"
                  className="text-accent hover:underline"
                >
                  {ownership.runbookUrl}
                </a>
              ) : (
                <span className="text-ink-4">—</span>
              )}
            </dd>
          </dl>
        ) : (
          <Empty
            title="No owner assigned"
            hint="Ownership is never guessed. Set a team and responder in Settings so alerts route to someone."
          />
        )}
      </Panel>

      {options.length > 0 && (
        <Panel title="Collector options">
          <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1.5 text-xs">
            {options.map(([key, value]) => {
              const spec = descriptor.fields.find((f) => f.key === key);
              return (
                <div key={key} className="contents">
                  <dt className="text-ink-4">{spec?.label ?? key}</dt>
                  <dd className="num min-w-0 truncate text-ink-2">{String(value)}</dd>
                </div>
              );
            })}
          </dl>
        </Panel>
      )}

      <Panel title="Capabilities">
        <div className="flex flex-col gap-2 text-xs">
          <div className="flex items-center gap-2">
            <span className="text-ink-4">Log collection</span>
            <span className="text-ink-2">
              {descriptor.capabilities.logs ? "Supported" : "Not supported"}
            </span>
          </div>
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="text-ink-4">Actions</span>
            {descriptor.capabilities.actions.length === 0 ? (
              <span className="text-ink-2">None — this collector is read-only</span>
            ) : (
              descriptor.capabilities.actions.map((action) => (
                <Tag key={action.tool} title={action.tool}>
                  {action.label}
                </Tag>
              ))
            )}
          </div>
          {descriptor.capabilities.diagnostics.length > 0 && (
            <div className="flex flex-wrap items-center gap-1.5">
              <span className="text-ink-4">Diagnostics</span>
              {descriptor.capabilities.diagnostics.map((diagnostic) => (
                <Tag key={diagnostic}>{diagnostic}</Tag>
              ))}
            </div>
          )}
        </div>
      </Panel>
    </div>
  );
}

// ------------------------------------------------------------------- confirm

/**
 * Mutating actions always stop here first, and the dialog names the exact
 * target. There is no "don't ask again" — the whole point is that a destructive
 * call against production infrastructure is deliberate.
 */
function ConfirmAction({
  action,
  row,
  target,
  busy,
  onCancel,
  onConfirm,
}: {
  action: RowAction;
  row: Row;
  target: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/60 p-4 backdrop-blur-sm"
      onClick={onCancel}
      role="dialog"
      aria-modal
    >
      <div
        className="panel w-full max-w-md bg-overlay p-4 overlay-plate"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-sm font-semibold text-ink">{action.label}</h2>
        <p className="mt-1.5 text-xs text-ink-2">
          This runs against <span className="font-medium text-ink">{row.name}</span> on{" "}
          <span className="font-medium text-ink">{target}</span>.
        </p>

        <dl className="mt-3 rounded-md border border-line bg-page p-2.5 text-2xs">
          <div className="flex gap-2">
            <dt className="w-16 shrink-0 text-ink-4">Tool</dt>
            <dd className="num text-ink-2">{action.tool}</dd>
          </div>
          {Object.entries(action.args).map(([key, value]) => (
            <div key={key} className="mt-1 flex gap-2">
              <dt className="w-16 shrink-0 text-ink-4">{key}</dt>
              <dd className="num min-w-0 break-all text-ink-2">{String(value)}</dd>
            </div>
          ))}
        </dl>

        {action.destructive && (
          <p className="mt-2.5 text-2xs text-warn">
            This changes the state of running infrastructure.
          </p>
        )}

        <div className="mt-4 flex justify-end gap-2">
          <Button onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant={action.destructive ? "danger" : "primary"}
            onClick={onConfirm}
            disabled={busy}
          >
            {busy ? "Running…" : action.label}
          </Button>
        </div>
      </div>
    </div>
  );
}

/**
 * One direction of the declared graph.
 *
 * "Depended on by" is marked derived because it is: only one direction is ever
 * declared, and showing both without saying which was typed invites someone to
 * go looking for a field that does not exist.
 */
function DependencyList({
  label,
  empty,
  ids,
  onOpen,
  derived,
}: {
  label: string;
  empty: string;
  ids: string[];
  model: EstateModel["services"][number];
  onOpen: (profileId: string) => void;
  derived?: boolean;
}) {
  return (
    <>
      <div className="mb-2 flex items-center gap-2">
        <span className="label">{label}</span>
        {derived && <span className="num text-2xs text-ink-4">derived</span>}
        <span className="num ml-auto text-2xs text-ink-4">{ids.length}</span>
      </div>
      {ids.length === 0 ? (
        <p className="text-2xs leading-relaxed text-ink-4">{empty}</p>
      ) : (
        <div className="flex flex-wrap gap-1.5">
          {ids.map((id) => (
            <Chip key={id} onClick={() => onOpen(id)} className="num text-ink-2">
              {id}
              <ChevronRight size={11} style={{ color: "var(--color-ink-4)" }} />
            </Chip>
          ))}
        </div>
      )}
    </>
  );
}
