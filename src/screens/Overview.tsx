import { useMemo, useState } from "react";

import { healthColor, healthLabel } from "../catalog";
import type { Catalog } from "../catalog";
import { formatMetricValue, formatRelative } from "../format";
import { flapped, homeState, leadIncident } from "../incident";
import type { EstateModel, Entity, Issue } from "../model";
import type { View } from "../nav";
import type {
  Alert,
  CapacityRisk,
  ChangeEvent,
  Finding,
  Recall,
  RecordedVerdict,
  Verdict,
  Correlation,
  Health,
  HealthRun,
  Incident,
  LogEvent,
  MetricSeries,
  TopologyView,
} from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Horizon } from "../ui/capacity";
import { FindingList, RecallStrip } from "../ui/config";
import { AlertList } from "../ui/AlertList";
import { LogHistogram, levelColor, type Bucket } from "../ui/charts";
import {
  EvidencePanel,
  IncidentHero,
  IncidentQueue,
  PropagationPanel,
  StormBanner,
  type IncidentActions,
  type IncidentContext,
} from "../ui/incident";
import { Bullet } from "../ui/plots";
import { Heatmap } from "../ui/Heatmap";
import { Button, cx, Dot, Empty, Panel, ServiceMark, Tag } from "../ui/primitives";
import { CheckIcon, ChevronRight } from "../ui/icons";

/**
 * The home screen answers one question — is anything wrong, and if so what is
 * actually happening — and it answers it differently depending on the answer.
 *
 * Four states, chosen by the estate rather than by the operator:
 *
 *   quiet     nothing is alerting. The screen earns its restraint by showing
 *             what it is watching and what changed, not a wall of green.
 *   incident  one thing is happening. It gets the whole screen.
 *   triage    several things are happening. They get a ranked queue, and the
 *             top one gets the detail.
 *   storm     many things turned at once. That is one hypothesis worth
 *             testing before it is a list worth working through.
 *
 * The heatmap and the estate lanes did not go away — they moved below the
 * fold, where they belong. They answer "how much of the estate" once you
 * already know what is wrong; they were never a good way to find out.
 */
export function Overview({
  model,
  catalog,
  incidents,
  changes,
  topology,
  correlation,
  buckets,
  healthRuns,
  history,
  capacity,
  findings,
  verdicts,
  judgedIncidents,
  onJudge,
  onJudgeIncident,
  logs,
  loading,
  minutes,
  syncedAt,
  onNavigate,
  onRefresh,
  onAcknowledge,
  onAsk,
  onAskIncident,
  onExplain,
  onSelectComponent,
}: {
  model: EstateModel;
  catalog: Catalog;
  incidents: Incident[];
  changes: ChangeEvent[];
  topology: TopologyView;
  correlation?: Correlation;
  buckets: Bucket[];
  healthRuns: Record<string, HealthRun[]>;
  history: Record<string, MetricSeries[]>;
  capacity: CapacityRisk[];
  findings: Finding[];
  verdicts: Record<string, RecordedVerdict>;
  judgedIncidents: string[];
  onJudge: (verdict: Verdict) => void;
  onJudgeIncident: (recall: Recall) => void;
  logs: LogEvent[];
  loading: boolean;
  minutes: number;
  syncedAt?: string;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAcknowledge: (alertId: string) => void;
  onAsk: () => void;
  onAskIncident: (incident: Incident) => void;
  onExplain: (alert: Alert) => void;
  onSelectComponent: (entity: Entity, issue?: Issue) => void;
}) {
  const [selectedId, setSelectedId] = useState<string>();
  const [split, setSplit] = useState<string[]>([]);

  // Splitting a storm is an override, not a setting: it lasts until the
  // grouping that prompted it stops existing.
  const shown = useMemo(
    () => incidents.filter((i) => !split.includes(i.id)),
    [incidents, split],
  );

  const state = homeState(shown);
  const selected =
    shown.find((i) => i.id === selectedId) ?? leadIncident(shown);

  const context: IncidentContext = {
    model,
    catalog,
    alerts: model.alerts,
    changes,
    logs,
    healthRuns,
    history,
    topology,
    correlation,
    minutes,
  };

  const actions: IncidentActions = {
    onOpenService: (profileId) => onNavigate({ name: "service", profileId }),
    onOpenSettingsFor: (profileId) =>
      onNavigate({ name: "service", profileId, tab: "configuration" }),
    onOpenAlerts: () => onNavigate({ name: "alerts" }),
    onOpenLogs: (profileId) => onNavigate({ name: "service", profileId, tab: "logs" }),
    onCorrelate: (at) => onNavigate({ name: "correlate", at }),
    onAcknowledgeAll: (alertIds) => alertIds.forEach(onAcknowledge),
    onAsk: onAskIncident,
  };

  const errors = useMemo(
    () => logs.filter((l) => l.level === "error" || l.level === "fatal").slice(0, 14),
    [logs],
  );

  if (loading && model.services.length === 0) {
    return (
      <>
        <ScreenHeader title="Overview" />
        <Screen>
          <div className="flex flex-col gap-3">
            <div className="panel h-[240px] animate-pulse opacity-40" />
            <div className="grid gap-3 @4xl:grid-cols-2">
              <div className="panel h-[200px] animate-pulse opacity-30" />
              <div className="panel h-[200px] animate-pulse opacity-30" />
            </div>
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
        meta={<HeaderState state={state} incidents={shown} model={model} />}
        syncedAt={syncedAt}
        live
        onRefresh={onRefresh}
        onAsk={onAsk}
      />

      <Screen className="flex flex-col gap-3">
        {state === "quiet" ? (
          <QuietState
            model={model}
            catalog={catalog}
            changes={changes}
            healthRuns={healthRuns}
            capacity={capacity}
            findings={findings}
            verdicts={verdicts}
            onJudge={onJudge}
            minutes={minutes}
            onNavigate={onNavigate}
          />
        ) : (
          <>
            {state === "storm" && selected && (
              <StormBanner
                incident={selected}
                onSplit={() => setSplit((prev) => [...prev, selected.id])}
                onCorrelate={(at) => onNavigate({ name: "correlate", at })}
              />
            )}

            {state === "triage" && shown.length > 1 && (
              <div className="grid gap-3 @5xl:grid-cols-[340px_1fr]">
                <Panel
                  title={`${shown.length} incidents`}
                  subtitle={queueSubtitle(shown)}
                  bodyClassName="overflow-y-auto"
                  className="max-h-[280px]"
                >
                  <IncidentQueue
                    incidents={shown}
                    selected={selected?.id}
                    context={context}
                    onSelect={(incident) => setSelectedId(incident.id)}
                  />
                </Panel>
                {selected && (
                  <IncidentHero
                    incident={selected}
                    context={context}
                    actions={actions}
                    compact
                  />
                )}
              </div>
            )}

            {state !== "triage" && selected && (
              <IncidentHero incident={selected} context={context} actions={actions} />
            )}

            {/* Under the hero, above the evidence: after somebody has read
                what happened and before they start working it. */}
            {selected && (
              <RecallStrip
                incident={{
                  id: selected.id,
                  headline: selected.title,
                  profileIds: selected.profileIds,
                  startedAt: selected.started,
                }}
                findings={findings}
                judged={judgedIncidents.includes(selected.id)}
                onJudge={onJudgeIncident}
              />
            )}

            {selected && (
              <div className="grid min-h-[280px] gap-3 @5xl:grid-cols-[1.35fr_1fr]">
                <PropagationPanel
                  incident={selected}
                  context={context}
                  onOpenService={actions.onOpenService}
                />
                <EvidencePanel incident={selected} context={context} actions={actions} />
              </div>
            )}

            <RestOfEstate
              model={model}
              incident={selected}
              onNavigate={onNavigate}
            />
          </>
        )}

        {/* Below the fold: the estate at large. It answers "how much of it" —
            never "what is wrong", which is what the top of the screen is for. */}
        <Panel
          title="Components"
          subtitle={`${model.counts.entities} across ${model.services.length} connections · hover to name, click to open`}
          actions={<HealthKey />}
        >
          <Heatmap
            entities={model.entities}
            issues={model.issues}
            catalog={catalog}
            onSelect={onSelectComponent}
          />
        </Panel>

        <div className="grid gap-3 @5xl:grid-cols-[3fr_2fr]">
          <AlertsPanel
            alerts={model.alerts}
            catalog={catalog}
            onNavigate={onNavigate}
            onAcknowledge={onAcknowledge}
            onExplain={onExplain}
          />
          <ActivityPanel
            buckets={buckets}
            errors={errors}
            minutes={minutes}
            onNavigate={onNavigate}
          />
        </div>
      </Screen>
    </>
  );
}

/**
 * The header counts what is still unattended; this list shows everything. Left
 * unexplained those two numbers look like a bug, so the subtitle reconciles
 * them rather than leaving the operator to.
 */
function queueSubtitle(incidents: Incident[]): string {
  const acked = incidents.filter((i) => i.acknowledged).length;
  if (acked === 0) return "worst and oldest first";
  return `${acked} acknowledged, below`;
}

// ------------------------------------------------------------------ header

/** The header states the answer before the screen argues it. */
function HeaderState({
  state,
  incidents,
  model,
}: {
  state: ReturnType<typeof homeState>;
  incidents: Incident[];
  model: EstateModel;
}) {
  const live = incidents.filter((i) => !i.acknowledged);
  const grouped = live.reduce((sum, i) => sum + i.alertIds.length, 0);

  if (state === "quiet") {
    return (
      <span className="flex items-center gap-2" style={{ color: "var(--color-ok)" }}>
        <CheckIcon size={13} />
        <span className="num">nothing alerting</span>
        <span className="text-ink-4/50">·</span>
        <span className="num text-ink-4">{model.counts.connections} connections watched</span>
      </span>
    );
  }

  const tone = live.some((i) => i.severity === "critical")
    ? "var(--color-crit)"
    : "var(--color-warn)";

  return (
    <span className="flex items-center gap-2" style={{ color: tone }}>
      <Dot health={live[0]?.health ?? "degraded"} size={8} />
      <span className="num">
        {live.length} incident{live.length === 1 ? "" : "s"}
        {grouped > live.length && ` from ${grouped} alerts`}
      </span>
    </span>
  );
}

// ------------------------------------------------------------- quiet state

/**
 * The quiet state has to earn its restraint.
 *
 * A screen that says only "all good" is one an operator stops opening, and a
 * monitoring tool nobody opens when things are fine is one nobody trusts when
 * they are not. So it shows the two things that are true and useful when
 * nothing is wrong: what changed recently, and what every connection has been
 * doing. Not a wall of green.
 */
function QuietState({
  model,
  catalog,
  changes,
  healthRuns,
  capacity,
  findings,
  verdicts,
  onJudge,
  minutes,
  onNavigate,
}: {
  model: EstateModel;
  catalog: Catalog;
  changes: ChangeEvent[];
  healthRuns: Record<string, HealthRun[]>;
  capacity: CapacityRisk[];
  findings: Finding[];
  verdicts: Record<string, RecordedVerdict>;
  onJudge: (verdict: Verdict) => void;
  minutes: number;
  onNavigate: (view: View) => void;
}) {
  // Ranked by the backend, which is the only place that can see the component
  // series — the export, the container's volume, the one hot node. Those are
  // where the answer usually is, and they are invisible from up here.
  const limits = capacity;
  const names = useMemo(
    () => new Map(model.services.map((s) => [s.snapshot.profileId, s.snapshot.profileName])),
    [model.services],
  );
  const flaps = useMemo(() => flapped(healthRuns, model), [healthRuns, model]);

  const recent = useMemo(
    () => [...changes].sort((a, b) => Date.parse(b.at) - Date.parse(a.at)).slice(0, 6),
    [changes],
  );

  const clearFor = useMemo(() => {
    const times = Object.values(healthRuns)
      .flat()
      .filter((r) => r.health !== "healthy")
      .map((r) => Date.parse(r.to));
    return times.length > 0 ? Math.max(...times) : undefined;
  }, [healthRuns]);

  return (
    <>
      <section className="panel flex shrink-0 flex-col items-start gap-4 px-4 py-4 @2xl:flex-row">
        <span className="mt-1" style={{ color: "var(--color-ok)" }}>
          <CheckIcon size={20} />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="text-[22px] leading-tight font-medium tracking-[-.025em] text-ink">
            Nothing is wrong.
          </h2>
          <p className="mt-2 max-w-[92ch] text-[12.5px] leading-relaxed text-ink-2">
            All {model.counts.connections} connections reported on their last poll, and{" "}
            {model.counts.entities} components are healthy.
            {clearFor !== undefined
              ? ` The last thing to leave healthy recovered ${formatRelative(new Date(clearFor).toISOString())}.`
              : ` Nothing has left healthy in the last ${minutes} minutes.`}{" "}
            {findings.length > 0
              ? " What follows is what its own configuration says is coming, what is closest to a limit, and what changed while it was quiet."
              : " What follows is what is closest to a limit, and what changed while it was quiet — the two things that decide whether this lasts."}
          </p>
        </div>
        <div className="flex w-full shrink-0 flex-col gap-1.5 @2xl:w-[168px]">
          <Button onClick={() => onNavigate({ name: "metrics" })}>Open metrics</Button>
          <Button onClick={() => onNavigate({ name: "logs" })}>Open logs</Button>
        </div>
      </section>

      {/* Above capacity on purpose. A headroom row says something will become a
          problem; a finding says what setting decides it and what to change.
          The one with an action attached goes first. */}
      {findings.length > 0 && (
        <Panel
          title="From configuration"
          subtitle="read against this estate's own telemetry"
          actions={
            <Button variant="ghost" onClick={() => onNavigate({ name: "services" })}>
              All connections
            </Button>
          }
        >
          <FindingList
            findings={findings.slice(0, 3)}
            verdicts={verdicts}
            onJudge={onJudge}
            onOpenService={(profileId) =>
              onNavigate({ name: "service", profileId, tab: "config" })
            }
          />
          {findings.length > 3 && (
            <p className="num mt-2 text-2xs text-ink-4">
              {findings.length - 3} more on their own connections.
            </p>
          )}
        </Panel>
      )}

      <div className="grid min-h-[240px] gap-3 @4xl:grid-cols-[1fr_340px]">
        <Panel
          title="Closest to a limit"
          subtitle={
            limits.length > 0
              ? "soonest to reach it first, not fullest"
              : "no thresholds declared"
          }
        >
          {limits.length === 0 ? (
            <p className="num text-2xs leading-relaxed text-ink-4">
              None of the metrics these collectors return carry a declared threshold, so there is
              no headroom to report. A number with no limit is not a warning waiting to happen —
              it is just a number.
            </p>
          ) : (
            <div className="flex flex-col gap-2.5">
              {limits.map((row) => (
                <button
                  key={`${row.profileId}:${row.component?.row ?? ""}:${row.key}`}
                  type="button"
                  onClick={() =>
                    onNavigate(
                      row.component
                        ? {
                            name: "component",
                            profileId: row.profileId,
                            sectionId: row.component.section,
                            rowId: row.component.row,
                          }
                        : { name: "service", profileId: row.profileId },
                    )
                  }
                  className="row-hover flex items-center gap-3 rounded-sm px-1 py-0.5 text-left"
                  title={`${names.get(row.profileId) ?? row.profileId} · ${
                    row.component ? `${row.component.row} · ` : ""
                  }${row.label}`}
                >
                  {/* The component is the answer when there is one. "nfs-prod
                      is 71% full" sends you looking; "/mnt/teams/alpha" is
                      the thing to go and look at. */}
                  <span className="num w-[110px] shrink-0 truncate text-2xs text-ink-3">
                    {row.component?.row ?? names.get(row.profileId) ?? row.profileId}
                  </span>
                  <span className="num w-[120px] shrink-0 truncate text-2xs text-ink-2">
                    {row.label}
                  </span>
                  <span className="min-w-0 flex-1">
                    {/* The bar runs past the limit so the limit can be DRAWN
                        inside it. With max == limit the threshold tick lands
                        on the end cap and never renders, and the fill stays
                        green until the moment it breaches — which is the one
                        moment the drawing no longer has anything to warn. */}
                    <Bullet value={row.value} max={row.limit * 1.25} warnAbove={row.limit} />
                  </span>
                  <span className="num w-[124px] shrink-0 text-right text-2xs text-ink-3">
                    {formatMetricValue(row.value, row.unit)}
                    <span className="text-ink-4"> of {formatMetricValue(row.limit, row.unit)}</span>
                  </span>
                  {/* The capacity answer. A row with no projection says so
                      rather than showing a blank column, because a blank one
                      reads as "no rush" and the truth is "not knowable yet". */}
                  <span className="num w-[104px] shrink-0 text-right text-2xs">
                    <Horizon trend={row.trend} unit={row.unit} />
                  </span>
                </button>
              ))}
            </div>
          )}

          {/* A flap that healed is invisible on a screen that only shows now,
              and it is usually the connection that stops healing next. */}
          <div className="mt-3.5 border-t border-line pt-3">
            <div className="label mb-2">Flapped in the window</div>
            {flaps.length === 0 ? (
              <p className="num text-2xs text-ink-4">
                Nothing left healthy in the last {minutes} minutes. Not one recovery, not one
                blip.
              </p>
            ) : (
              <div className="flex flex-wrap gap-1.5">
                {flaps.map((flap) => (
                  <button
                    key={flap.profileId}
                    type="button"
                    onClick={() => onNavigate({ name: "service", profileId: flap.profileId })}
                    className="row-hover num flex items-center gap-1.5 rounded-sm border border-line px-2 py-1 text-2xs text-ink-2"
                  >
                    <Dot health={flap.worst} size={7} />
                    {flap.profileName}
                    <span className="text-ink-4">recovered</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        </Panel>

        <Panel
          title="Recently changed"
          subtitle={`${recent.length} in the window`}
          flush
          bodyClassName="overflow-y-auto"
        >
          {recent.length === 0 ? (
            <p className="num px-3 py-4 text-2xs leading-relaxed text-ink-4">
              Nothing has been deployed or added in this window.
            </p>
          ) : (
            recent.map((change) => (
              <button
                key={change.id}
                type="button"
                onClick={() =>
                  onNavigate({ name: "service", profileId: change.profileId })
                }
                className="row-hover flex w-full items-start gap-2.5 border-b border-line-soft px-3 py-2 text-left last:border-0"
              >
                <ServiceMark descriptor={catalog.get(change.kind)} size={15} />
                <span className="min-w-0 flex-1">
                  <span className="num block truncate text-2xs text-ink-2">
                    {change.summary}
                  </span>
                  <span className="num mt-1 flex items-center gap-1.5 text-2xs text-ink-4">
                    {formatRelative(change.at)}
                    <Tag>{change.source}</Tag>
                  </span>
                </span>
              </button>
            ))
          )}
        </Panel>
      </div>
    </>
  );
}

// ---------------------------------------------------------- rest of estate

/**
 * Everything not in the incident, collapsed to one line. It is not the story,
 * and giving it a panel would say otherwise.
 */
function RestOfEstate({
  model,
  incident,
  onNavigate,
}: {
  model: EstateModel;
  incident?: Incident;
  onNavigate: (view: View) => void;
}) {
  const members = new Set(incident?.profileIds ?? []);
  const others = model.services.filter((s) => !members.has(s.snapshot.profileId));
  const nominal = others.filter((s) => s.health === "healthy").length;
  const affected = model.issues.filter((i) => members.has(i.profileId)).length;

  return (
    <section className="panel flex shrink-0 flex-wrap items-center gap-3.5 px-4 py-2.5">
      <span style={{ color: "var(--color-ok)" }}>
        <CheckIcon size={14} />
      </span>
      <span className="num text-xs text-ink-2">
        {nominal} other connection{nominal === 1 ? "" : "s"} nominal
      </span>
      <span className="num text-2xs text-ink-4">
        {model.counts.entities - affected} of {model.counts.entities} components healthy
      </span>
      <button
        type="button"
        onClick={() => onNavigate({ name: "services" })}
        className="row-hover num ml-auto flex items-center gap-1 rounded-sm text-2xs text-ink-4"
      >
        Show all
        <ChevronRight size={11} />
      </button>
    </section>
  );
}

// ------------------------------------------------------------ lower panels

function AlertsPanel({
  alerts,
  catalog,
  onNavigate,
  onAcknowledge,
  onExplain,
}: {
  alerts: Alert[];
  catalog: Catalog;
  onNavigate: (view: View) => void;
  onAcknowledge: (alertId: string) => void;
  onExplain: (alert: Alert) => void;
}) {
  const active = alerts.filter((a) => a.active);

  return (
    <Panel
      title="Active alerts"
      subtitle={`${active.length} firing`}
      flush
      bodyClassName="overflow-y-auto max-h-[340px]"
      actions={
        <Button variant="ghost" onClick={() => onNavigate({ name: "alerts" })}>
          All
          <ChevronRight size={12} />
        </Button>
      }
    >
      {active.length === 0 ? (
        <Empty title="Nothing firing" hint="Every condition the collectors watch is within range." />
      ) : (
        <AlertList
          alerts={active.slice(0, 8)}
          catalog={catalog}
          onAcknowledge={onAcknowledge}
          onExplain={onExplain}
          onOpenService={(profileId) => onNavigate({ name: "service", profileId })}
        />
      )}
    </Panel>
  );
}

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
  return (
    <Panel
      title="Activity"
      subtitle={`log volume · last ${minutes}m`}
      actions={
        <Button variant="ghost" onClick={() => onNavigate({ name: "logs" })}>
          Logs
          <ChevronRight size={12} />
        </Button>
      }
      bodyClassName="flex flex-col gap-3"
    >
      <LogHistogram buckets={buckets} height={64} />

      <div className="min-h-0 flex-1 overflow-y-auto">
        {errors.length === 0 ? (
          <p className="num text-2xs leading-relaxed text-ink-4">
            No errors in the window.
          </p>
        ) : (
          errors.map((log) => (
            <button
              key={log.id}
              type="button"
              onClick={() =>
                onNavigate({ name: "service", profileId: log.profileId, tab: "logs" })
              }
              className="row-hover flex w-full items-start gap-2 rounded-sm px-1 py-1 text-left"
            >
              <span
                className="mt-1 h-1.5 w-1.5 shrink-0 rounded-full"
                style={{ background: levelColor(log.level) }}
              />
              <span className="num min-w-0 flex-1 truncate text-2xs text-ink-3" title={log.message}>
                {log.message}
              </span>
              <span className="num shrink-0 text-2xs text-ink-4">
                {log.profileName}
              </span>
            </button>
          ))
        )}
      </div>
    </Panel>
  );
}

function HealthKey() {
  const items: Health[] = ["down", "degraded", "unknown", "healthy"];
  return (
    <div className="flex items-center gap-2.5">
      {items.map((health) => (
        <span key={health} className="flex items-center gap-1.5">
          <span
            className={cx("h-2 w-2 rounded-[1px]")}
            style={{ background: healthColor(health) }}
          />
          <span className="num text-2xs text-ink-4">{healthLabel(health)}</span>
        </span>
      ))}
    </div>
  );
}
