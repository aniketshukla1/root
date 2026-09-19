/**
 * The incident, told in the order an operator needs it.
 *
 * What is wrong, what changed, how far it reaches, how sure we are, what it
 * could not see, how it spread, what proves it. Every panel here is fed by
 * `incident.ts` from data the collectors returned — none of it waits on a
 * model, and the screen is complete with no model configured at all.
 */

import type { ReactNode } from "react";

import { motion } from "motion/react";

import { healthColor } from "../catalog";
import type { Catalog } from "../catalog";
import { formatRelative } from "../format";
import {
  blastRadius,
  blindSpots,
  confidenceMeaning,
  evidenceFor,
  groupingSentence,
  propagation,
} from "../incident";
import type { BlindSpot, Evidence } from "../incident";
import type { EstateModel } from "../model";
import { enter, stagger } from "./motion";
import type {
  Alert,
  ChangeEvent,
  Correlation,
  Health,
  HealthRun,
  Incident,
  LogEvent,
  MetricSeries,
  TopologyView,
} from "../types";
import { Lanes, Ticks } from "./plots";
import type { Lane } from "./plots";
import { Button, Chip, cx, Dot, PanelHeader, ServiceMark, Stat, Tag } from "./primitives";
import { AssistantIcon, ChevronRight, ExternalIcon } from "./icons";

export interface IncidentContext {
  model: EstateModel;
  catalog: Catalog;
  alerts: Alert[];
  changes: ChangeEvent[];
  logs: LogEvent[];
  healthRuns: Record<string, HealthRun[]>;
  history: Record<string, MetricSeries[]>;
  topology: TopologyView;
  correlation?: Correlation;
  minutes: number;
}

export interface IncidentActions {
  onOpenService: (profileId: string) => void;
  /** Where a blind spot gets closed — the connection's own configuration. */
  onOpenSettingsFor: (profileId: string) => void;
  onOpenAlerts: () => void;
  onOpenLogs: (profileId: string) => void;
  onCorrelate: (at: string) => void;
  onAcknowledgeAll: (alertIds: string[]) => void;
  onAsk: (incident: Incident) => void;
  /** Ask the model why this happened. See `ui/RootCausePanel.tsx`. */
  onFindCause: (incident: Incident) => void;
}

/** How many of an incident's alerts somebody has already acknowledged. */
const acknowledgedIn = (incident: Incident, alerts: Alert[]) =>
  alerts.filter((a) => incident.alertIds.includes(a.id) && a.acknowledged).length;

const severityColor = (incident: Incident) =>
  incident.severity === "critical" ? "var(--color-crit)" : "var(--color-warn)";

// ------------------------------------------------------------------- hero

/**
 * The claim, and the three things that qualify it.
 *
 * The headline is the alert's own words rather than a generated summary: an
 * operator who has seen this failure before recognises the string, and a
 * paraphrase would cost them that recognition for no gain. The assistant is
 * one click away for the sentence that is worth generating.
 */
export function IncidentHero({
  incident,
  context,
  actions,
  compact,
  footer,
}: {
  incident: Incident;
  context: IncidentContext;
  actions: IncidentActions;
  compact?: boolean;
  /** The recall question, which is about THIS incident and nothing else. */
  footer?: ReactNode;
}) {
  const radius = blastRadius(incident, context.model);
  const spots = blindSpots(incident, {
    topology: context.topology,
    changes: context.changes,
    history: context.history,
  });

  const change = context.changes
    .filter((c) => incident.profileIds.includes(c.profileId))
    .filter((c) => Date.parse(c.at) <= Date.parse(incident.started) + 60_000)
    .sort((a, b) => Date.parse(b.at) - Date.parse(a.at))[0];

  const root = incident.leadProfileId || incident.profileIds[0];
  const rootService = context.model.services.find((s) => s.snapshot.profileId === root);

  return (
    <section
      className="panel flex shrink-0 flex-col overflow-hidden"
      style={{ borderLeft: `2px solid ${severityColor(incident)}` }}
    >
      <div className="flex flex-col items-start gap-3.5 px-4 pt-3.5 pb-3 @2xl:flex-row">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2.5">
            {/* The dot rides with the label rather than sitting in a column of
                its own: when the dock narrows this panel and the row stacks, a
                lone dot on its own line reads as a bullet for nothing. */}
            <span className="flex items-center gap-2">
              <Dot health={incident.health} size={11} />
              <span
                className="font-mono text-2xs tracking-[.16em] uppercase"
                style={{ color: severityColor(incident) }}
              >
                Incident
              </span>
            </span>
            <span className="num text-2xs text-ink-4">
              started {clock(incident.started)} · {formatRelative(incident.started).replace(" ago", "")}
            </span>
            {incident.alertIds.length > 1 && (
              <Tag color={severityColor(incident)}>
                {incident.alertIds.length} ALERTS GROUPED
                {acknowledgedIn(incident, context.alerts) > 0 &&
                  ` · ${acknowledgedIn(incident, context.alerts)} ACKED`}
              </Tag>
            )}
            {incident.acknowledged && <Tag>ACKNOWLEDGED</Tag>}
          </div>

          <h2
            className={cx(
              "mt-2 leading-[1.15] font-medium tracking-[-.025em] text-ink",
              compact ? "text-xl" : "text-2xl @2xl:text-4xl",
            )}
          >
            {incident.title}
          </h2>

          <p className="mt-2.5 max-w-[96ch] text-base leading-relaxed text-ink-2">
            {groupingSentence(incident)}
          </p>
        </div>

        <div className="flex w-full shrink-0 flex-col gap-1.5 @2xl:w-[168px]">
          <Button variant="primary" onClick={() => actions.onOpenService(root)}>
            Open {rootService?.snapshot.profileName ?? root}
          </Button>
          {/* Above "Ask a follow-up" on purpose. A follow-up is a
              conversation somebody has to steer; this is the one question
              they already have, asked once, on evidence root is holding. */}
          <Button onClick={() => actions.onFindCause(incident)}>
            <AssistantIcon size={13} style={{ color: "var(--color-accent)" }} />
            Root Cause Analysis
          </Button>
          <Button onClick={() => actions.onAsk(incident)}>
            Ask a follow-up
          </Button>
          {!incident.acknowledged && (
            <Button onClick={() => actions.onAcknowledgeAll(incident.alertIds)}>
              Acknowledge all
            </Button>
          )}
        </div>
      </div>

      <div className="grid gap-px border-t border-line bg-line @4xl:grid-cols-3">
        <ChangeCell change={change} catalog={context.catalog} incident={incident} />
        <RadiusCell radius={radius} onOpen={actions.onOpenService} />
        <ConfidenceCell incident={incident} spots={spots} onFix={actions.onOpenSettingsFor} />
      </div>

      {/* Inside the panel, under the claim it is asking about. Floating free
          between two panels it read as a survey the page had nothing to do
          with, and "this" in the question had no antecedent. */}
      {footer && <div className="border-t border-line bg-panel">{footer}</div>}
    </section>
  );
}

/**
 * The only cell that can explain rather than describe — so when there is
 * nothing in it, it says so in those words rather than going blank. An empty
 * panel reads as "not loaded"; "no change observed" is a finding.
 */
function ChangeCell({
  change,
  catalog,
  incident,
}: {
  change?: ChangeEvent;
  catalog: Catalog;
  incident: Incident;
}) {
  const lead = Math.round(
    (Date.parse(incident.started) - Date.parse(change?.at ?? incident.started)) / 60_000,
  );

  return (
    <div className="bg-panel px-4 py-3">
      <div className="label mb-2.5">What changed</div>
      {change ? (
        <>
          <div className="flex items-center gap-2">
            <ServiceMark descriptor={catalog.get(change.kind)} size={17} />
            <span className="num truncate text-xs text-ink">
              {change.profileName} · {change.component}
            </span>
          </div>
          <div className="num mt-2 text-sm text-ink-3">
            {change.from && <span className="text-ink-4">{change.from}</span>}
            {change.from && <span className="mx-1.5">→</span>}
            <span style={{ color: "var(--color-accent)" }}>{change.to}</span>
          </div>
          <div className="num mt-2 text-2xs leading-relaxed text-ink-4">
            {lead > 0
              ? `deployed ${clock(change.at)}, ${lead} minute${lead === 1 ? "" : "s"} before the first alert`
              : `recorded ${clock(change.at)}`}
          </div>
          <div className="mt-2.5 flex gap-1.5">
            <Tag>{change.source}</Tag>
          </div>
        </>
      ) : (
        <p className="text-sm leading-relaxed text-ink-3">
          Nothing changed on these connections before this started — no deploy, no new component.
          The cause is either older than the window or somewhere root does not watch.
        </p>
      )}
    </div>
  );
}

/**
 * Components, not connections. "Three connections" flatters a twelve-connection
 * estate; six of 364 components is the same failure told honestly, and it is
 * the number that decides whether anyone gets woken up.
 */
function RadiusCell({
  radius,
  onOpen,
}: {
  radius: ReturnType<typeof blastRadius>;
  onOpen: (profileId: string) => void;
}) {
  return (
    <div className="bg-panel px-4 py-3">
      <div className="label mb-2.5">Blast radius</div>
      <div className="flex flex-wrap items-baseline gap-x-5 gap-y-1">
        <Stat
          labelPosition="inline"
          label={`connection${radius.connections === 1 ? "" : "s"}`}
          value={radius.connections}
        />
        <Stat
          labelPosition="inline"
          label={`component${radius.components === 1 ? "" : "s"}`}
          value={radius.components}
        />
      </div>

      <motion.div variants={stagger(0.05)} {...enter} className="mt-3 flex flex-col gap-1.5">
        {radius.rows.slice(0, 6).map((row) => (
          <button
            key={row.profileId}
            type="button"
            onClick={() => onOpen(row.profileId)}
            className="row-hover flex items-center gap-2 rounded-sm text-left"
            title={`${row.profileName} — ${row.affected} of ${row.total} components affected`}
          >
            <span className="num w-[104px] shrink-0 truncate text-2xs text-ink-2">
              {row.profileName}
            </span>
            <ComponentPips affected={row.affected} total={row.total} health={row.health} />
            {row.root && <Tag color="var(--color-accent)">FIRST</Tag>}
          </button>
        ))}
      </motion.div>

      <div className="num mt-2.5 text-2xs leading-relaxed text-ink-4">
        {radius.rows.length > 6 && `${radius.rows.length - 6} more involved. `}
        {radius.unaffected} of {radius.totalComponents} components elsewhere are unaffected.
      </div>
    </div>
  );
}

/**
 * The small-box vocabulary, at the scale of one connection: a fixed strip where
 * the filled cells are what is wrong. Capped so a 400-pod cluster does not
 * become a wall — the ratio is the message, not the census.
 *
 * The healthy cells are tinted with the ok hue and ringed, exactly as the
 * heatmap draws them, rather than filled with `--color-cell-todo`. That token
 * is a GROUND — it is always paired with hatching to mean "no data" — and used
 * bare as a mark it measures 1.09:1 against the panel, which is not quiet but
 * invisible. A strip whose empty cells cannot be seen shows a count, and the
 * count was never the point; the ratio is.
 *
 * The RING is what makes a healthy cell legible, not the fill — and it is set
 * to the same 12% / 45% the heatmap uses, for the same reason: at 26 / 55 a
 * strip of healthy cells was louder than the failing ones beside it. The ring
 * measures 3.10:1 against the panel, above the 3:1 floor for a mark that
 * carries meaning. Both halves are load-bearing — raising the fill makes a
 * healthy estate shout, dropping the ring takes the cell below that floor.
 * Change one and re-measure the other.
 */
function ComponentPips({
  affected,
  total,
  health,
}: {
  affected: number;
  total: number;
  health: Health;
}) {
  const CAP = 24;
  const shown = Math.min(total, CAP);
  // At least one cell shows when anything is affected: a strip that rounds a
  // real failure down to zero is worse than one that overstates a small one.
  const bad =
    total === 0 ? 0 : Math.max(affected > 0 ? 1 : 0, Math.round((affected / total) * shown));
  const badColor = healthColor(health);
  const okColor = healthColor("healthy");

  return (
    <span className="flex gap-[3px]" aria-hidden>
      {Array.from({ length: shown }, (_, i) => (
        <span
          key={i}
          className="h-[9px] w-[9px] rounded-xs"
          style={
            i < bad
              ? { background: badColor, boxShadow: `inset 0 0 0 1px ${badColor}` }
              : {
                  background: `color-mix(in srgb, ${okColor} 12%, var(--color-panel))`,
                  boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${okColor} 45%, transparent)`,
                }
          }
        />
      ))}
      {total > CAP && (
        <span className="num ml-1 self-center text-2xs text-ink-4">+{total - CAP}</span>
      )}
    </span>
  );
}

/**
 * Confidence is about the grouping, not the diagnosis — and the "could not
 * see" list below it is what separates a finding from a guess. It gets more
 * important as the rest of the screen gets more confident, so it is never
 * collapsed away.
 */
function ConfidenceCell({
  incident,
  spots,
  onFix,
}: {
  incident: Incident;
  spots: BlindSpot[];
  onFix: (profileId: string) => void;
}) {
  const fill =
    incident.confidence === "high" ? 3 : incident.confidence === "medium" ? 2 : 1;
  const tone =
    incident.confidence === "high"
      ? "var(--color-ok)"
      : incident.confidence === "medium"
        ? "var(--color-warn)"
        : "var(--color-serious)";

  return (
    <div className="bg-panel px-4 py-3">
      <div className="label mb-2.5">Confidence</div>
      <div className="flex items-center gap-2.5">
        <span className="num text-3xl leading-none font-medium tracking-[-.03em] text-ink capitalize">
          {incident.confidence}
        </span>
        <span className="flex h-[5px] flex-1 gap-[2px]" aria-hidden>
          {[1, 2, 3].map((i) => (
            <span
              key={i}
              className="flex-1 rounded-xs"
              style={{ background: i <= fill ? tone : "var(--color-sunk)" }}
            />
          ))}
        </span>
      </div>
      <p className="num mt-2.5 text-xs leading-relaxed text-ink-3">
        {confidenceMeaning(incident)}
      </p>

      {spots.length > 0 && (
        <div className="mt-2.5 border-t border-line pt-2.5">
          <div className="label mb-1.5" style={{ color: "var(--color-warn)" }}>
            Could not see
          </div>
          <ul className="num flex flex-col gap-1.5 text-xs leading-relaxed text-ink-4">
            {spots.map((spot) => (
              <li key={spot.text}>
                {spot.text}
                {spot.fix && (
                  <button
                    type="button"
                    onClick={() => onFix(spot.fix!.profileId)}
                    className="ml-1.5 inline-flex min-h-6 items-center underline decoration-dotted underline-offset-2 transition-colors hover:text-accent"
                    style={{ color: "var(--color-accent)" }}
                  >
                    {spot.fix.label}
                  </button>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------ propagation

/**
 * The order it spread in, scoped to the incident rather than the estate.
 *
 * Reading down the column at the moment of onset is the argument: the lane
 * that turns first is the one to open. Unaffected connections collapse to a
 * single dimmed row — they are context, not evidence.
 */
export function PropagationPanel({
  incident,
  context,
  onOpenService,
}: {
  incident: Incident;
  context: IncidentContext;
  onOpenService: (profileId: string) => void;
}) {
  const windowMs = context.minutes * 60_000;
  const now = Date.now();
  const start = now - windowMs;
  const clamp = (v: number) => Math.max(0, Math.min(1, v));

  const rows = propagation(incident, context.healthRuns, context.model);

  const lanes: Lane[] = rows.map((row) => ({
    id: row.profileId,
    name: row.profileName,
    health: row.health,
    runs: (context.healthRuns[row.profileId] ?? [])
      .map((run) => ({
        health: run.health,
        from: clamp((Date.parse(run.from) - start) / windowMs),
        to: clamp((Date.parse(run.to) - start) / windowMs),
      }))
      .filter((run) => run.to > run.from),
  }));

  const unaffected = context.model.services.length - rows.length;
  if (unaffected > 0) {
    lanes.push({
      id: "__rest",
      name: `${unaffected} unaffected`,
      health: "healthy",
      runs: [{ health: "healthy", from: 0, to: 1 }],
    });
  }

  const deploy = context.changes
    .filter((c) => incident.profileIds.includes(c.profileId))
    .filter((c) => Date.parse(c.at) >= start)
    .sort((a, b) => Date.parse(a.at) - Date.parse(b.at))[0];
  const deployAt = deploy ? clamp((Date.parse(deploy.at) - start) / windowMs) : undefined;

  const ordered = rows.filter((r) => r.turnedAt);

  return (
    <section className="panel flex min-h-0 flex-col overflow-hidden">
      <PanelHeader
        title="How it spread"
        subtitle={`${rows.length} affected · last ${context.minutes}m`}
        actions={
          deployAt !== undefined ? (
            <span className="num flex items-center gap-1.5 text-2xs text-ink-4">
              <span className="w-2.5 border-t border-dashed border-accent" />
              deploy
            </span>
          ) : undefined
        }
      />

      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        <div className="[&_button]:w-full">
          <Lanes
            lanes={lanes}
            labelWidth={124}
            deployAt={deployAt}
            footer={
              <>
                <Ticks count={60} />
                <div className="mt-1 flex justify-between">
                  <span className="num text-2xs text-ink-4">−{context.minutes}m</span>
                  <span className="num text-2xs text-ink-2">now</span>
                </div>
              </>
            }
          />
        </div>

        {ordered.length > 0 && (
          <p className="num mt-3.5 text-xs leading-relaxed text-ink-3">
            {ordered
              .map((row, i) =>
                i === 0
                  ? `${row.profileName} turned first at ${clock(row.turnedAt!)}`
                  : `${row.profileName} ${gapFrom(ordered[i - 1].turnedAt!, row.turnedAt!)}`,
              )
              .join(", then ")}
            .
            {deploy && ` Nothing turned before the ${deploy.change === "deployed" ? "deploy" : "change"} at ${clock(deploy.at)}.`}
          </p>
        )}

        {rows.length > 1 && (
          <div className="mt-3 flex flex-wrap gap-1.5">
            {rows.map((row) => (
              <Chip
                key={row.profileId}
                onClick={() => onOpenService(row.profileId)}
                className="num text-ink-2"
              >
                <Dot health={row.health} size={7} />
                {row.profileName}
                <ChevronRight size={11} style={{ color: "var(--color-ink-4)" }} />
              </Chip>
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

function gapFrom(previous: string, current: string): string {
  const mins = Math.round((Date.parse(current) - Date.parse(previous)) / 60_000);
  if (mins <= 0) return "at the same moment";
  return `${mins} minute${mins === 1 ? "" : "s"} later`;
}

// --------------------------------------------------------------- evidence

/**
 * Every claim above, traced to a line that opens where it came from.
 *
 * This is the panel that makes the rest of the screen arguable. A summary
 * nobody can check is a rumour with good typography.
 */
export function EvidencePanel({
  incident,
  context,
  actions,
}: {
  incident: Incident;
  context: IncidentContext;
  actions: IncidentActions;
}) {
  const evidence = evidenceFor(incident, {
    alerts: context.alerts,
    changes: context.changes,
    logs: context.logs,
    correlation: context.correlation,
  });

  const open = (item: Evidence) => {
    switch (item.target.to) {
      case "alerts":
        return actions.onOpenAlerts();
      case "logs":
        return actions.onOpenLogs(item.target.profileId);
      case "service":
        return actions.onOpenService(item.target.profileId);
      case "correlate":
        return actions.onCorrelate(item.target.at);
    }
  };

  return (
    <section className="panel flex min-h-0 flex-col overflow-hidden">
      <PanelHeader
        title="Evidence"
        subtitle={`${evidence.length} source${evidence.length === 1 ? "" : "s"} · each opens where it came from`}
      />

      <motion.div
        variants={stagger(0.04)}
        {...enter}
        className="min-h-0 flex-1 overflow-y-auto py-1"
      >
        {evidence.map((item, i) => (
          <button
            key={item.id}
            type="button"
            onClick={() => open(item)}
            className="row-hover flex w-full items-start gap-2.5 border-b border-line-soft px-3 py-2 text-left last:border-0"
          >
            <span className="num w-4 shrink-0 pt-px text-2xs" style={{ color: "var(--color-accent)" }}>
              {String(i + 1).padStart(2, "0")}
            </span>
            <span className="min-w-0 flex-1">
              <span className="num block truncate text-xs text-ink-2" title={item.headline}>
                {item.headline}
              </span>
              <span className="num mt-1 block truncate text-2xs text-ink-4">{item.provenance}</span>
            </span>
            <ExternalIcon size={11} style={{ color: "var(--color-ink-4)" }} />
          </button>
        ))}

        {evidence.length === 0 && (
          <p className="num px-3 py-4 text-xs leading-relaxed text-ink-4">
            The alerts fired but nothing else in the window supports or contradicts them — no
            change, no metric movement, no error lines. That absence is itself worth knowing.
          </p>
        )}
      </motion.div>

      <div className="shrink-0 border-t border-line px-3 py-2.5">
        <button
          type="button"
          onClick={() => actions.onCorrelate(incident.started)}
          className="row-hover flex w-full items-center gap-2 rounded-sm border border-line px-2.5 py-1.5 text-left"
        >
          <span className="num text-2xs" style={{ color: "var(--color-accent)" }}>
            →
          </span>
          <span className="flex-1 text-xs text-ink-2">
            Correlate {clock(incident.started)} across the whole estate
          </span>
          <ChevronRight size={11} style={{ color: "var(--color-ink-4)" }} />
        </button>
      </div>
    </section>
  );
}

// ------------------------------------------------------------------ queue

/**
 * More than one incident, ranked. The list has a top — worst and oldest first,
 * acknowledged sunk to the bottom — because a triage list with no order is
 * just the alert list again with fewer rows.
 */
export function IncidentQueue({
  incidents,
  selected,
  context,
  onSelect,
}: {
  incidents: Incident[];
  selected?: string;
  context: IncidentContext;
  onSelect: (incident: Incident) => void;
}) {
  return (
    <motion.div variants={stagger(0.05)} {...enter} className="flex flex-col gap-1.5">
      {incidents.map((incident) => {
        const radius = blastRadius(incident, context.model);
        const active = incident.id === selected;
        return (
          <button
            key={incident.id}
            type="button"
            onClick={() => onSelect(incident)}
            className={cx(
              "row-hover flex w-full items-start gap-2.5 rounded-sm border px-2.5 py-2 text-left",
              active ? "border-line-strong bg-sunk" : "border-line",
              incident.acknowledged && "opacity-60",
            )}
            style={active ? { borderLeft: `2px solid ${severityColor(incident)}` } : undefined}
          >
            <span className="mt-1">
              <Dot health={incident.health} size={9} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-base text-ink" title={incident.title}>
                {incident.title}
              </span>
              <span className="num mt-1 flex flex-wrap items-center gap-2 text-2xs text-ink-4">
                <span>{formatRelative(incident.started)}</span>
                <span className="text-ink-4/50">·</span>
                <span>
                  {radius.connections} conn · {radius.components} comp
                </span>
                {incident.alertIds.length > 1 && (
                  <>
                    <span className="text-ink-4/50">·</span>
                    <span>{incident.alertIds.length} alerts</span>
                  </>
                )}
                {incident.confidence !== "high" && (
                  <Tag
                    color={
                      incident.confidence === "low"
                        ? "var(--color-serious)"
                        : "var(--color-warn)"
                    }
                  >
                    {incident.confidence.toUpperCase()} CONFIDENCE
                  </Tag>
                )}
              </span>
            </span>
          </button>
        );
      })}
    </motion.div>
  );
}

// ------------------------------------------------------------------ storm

/**
 * Many connections turning at once is one hypothesis, not many problems.
 *
 * The banner states the hypothesis and offers to reject it, because grouping
 * on timing alone is the weakest thing this system does and the screen should
 * not pretend otherwise. Under-merging costs a second glance; over-merging
 * costs a wrong diagnosis.
 */
export function StormBanner({
  incident,
  onSplit,
  onCorrelate,
}: {
  incident: Incident;
  onSplit: () => void;
  onCorrelate: (at: string) => void;
}) {
  const reason = incident.grouping.find((g) => g.kind === "simultaneousOnset");
  if (reason?.kind !== "simultaneousOnset") return null;

  return (
    <section
      className="panel flex shrink-0 flex-col items-start gap-3.5 px-4 py-3 @2xl:flex-row"
      style={{ borderLeft: "2px solid var(--color-serious)" }}
    >
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2.5">
          <span
            className="font-mono text-2xs tracking-[.16em] uppercase"
            style={{ color: "var(--color-serious)" }}
          >
            Possible single event
          </span>
          <Tag color="var(--color-serious)">LOW CONFIDENCE</Tag>
        </div>
        <p className="mt-2 max-w-[92ch] text-base leading-relaxed text-ink-2">
          {incident.profileIds.length} connections, in {reason.count} failures nothing declared
          connects, turned within {reason.windowSecs} seconds of each other. That is either one
          cause root cannot see — a network partition, a shared host, an expired certificate — or a
          coincidence. Nothing here proves which, so they are grouped provisionally and can be
          split.
        </p>
      </div>
      <div className="flex w-full shrink-0 flex-col gap-1.5 @2xl:w-[168px]">
        <Button variant="primary" onClick={() => onCorrelate(incident.started)}>
          Correlate the moment
        </Button>
        <Button onClick={onSplit}>Split them apart</Button>
      </div>
    </section>
  );
}

function clock(iso: string): string {
  return new Date(iso).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}
