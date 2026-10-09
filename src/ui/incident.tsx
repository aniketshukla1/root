/**
 * The incident, told in the order an operator needs it.
 *
 * What is wrong, what changed, how far it reaches, how sure we are, what it
 * could not see, how it spread, what proves it. Every panel here is fed by
 * `incident.ts` from data the collectors returned — none of it waits on a
 * model, and the screen is complete with no model configured at all.
 */

import { useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { motion } from "motion/react";
import type { Variants } from "motion/react";

import { healthColor, healthLabel } from "../catalog";
import type { Catalog } from "../catalog";
import { formatMetricValue, formatRelative } from "../format";
import {
  blastRadius,
  blindSpots,
  confidenceMeaning,
  evidenceFor,
  groupingSentence,
  propagation,
} from "../incident";
import type { BlindSpot, Evidence, PropagationRow } from "../incident";
import type { EstateModel } from "../model";
import { SEVERITY } from "./AlertList";
import { levelTextColor } from "./charts";
import { EASE_OUT, enter, popIn, stagger } from "./motion";
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
import { Button, cx, Dot, PanelHeader, ServiceMark, Stat, Tag } from "./primitives";
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

/**
 * A connection's name, for any sentence an operator reads.
 *
 * Incidents, grouping reasons and the topology all carry profile ids. For a
 * real connection that id is a UUID, so every sentence built from them has
 * to come through here — one sentence that did not printed "depends on
 * 3f2a…" in the middle of an incident.
 */
const nameIn = (context: IncidentContext) => (id: string) =>
  context.model.services.find((s) => s.snapshot.profileId === id)?.snapshot.profileName ?? id;

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
  const nameOf = nameIn(context);
  const spots = blindSpots(incident, {
    topology: context.topology,
    changes: context.changes,
    history: context.history,
    nameOf,
  });

  const change = context.changes
    .filter((c) => incident.profileIds.includes(c.profileId))
    .filter((c) => Date.parse(c.at) <= Date.parse(incident.started) + 60_000)
    .sort((a, b) => Date.parse(b.at) - Date.parse(a.at))[0];

  const root = incident.leadProfileId || incident.profileIds[0];
  const rootService = context.model.services.find((s) => s.snapshot.profileId === root);

  return (
    <section
      className="panel hue flex shrink-0 flex-col overflow-hidden"
      style={
        {
          borderLeft: `2px solid ${severityColor(incident)}`,
          "--hue": severityColor(incident),
        } as React.CSSProperties
      }
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
                className="text-2xs font-semibold tracking-[.06em] uppercase"
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
            {groupingSentence(incident, nameOf)}
          </p>
        </div>

        {/* Wide enough for "Root Cause Analysis" in the UI face at 13px with
            its icon — 168px fitted the old 12px mono and wrapped this one. */}
        <div className="flex w-full shrink-0 flex-col gap-1.5 @2xl:w-[200px]">
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
            {/* `min-w-0` and nothing else. A flex child's default minimum is
                its content, so without it the strip refuses to shrink and
                pushes itself out of the card — but `flex-1` would make it
                *grow* too, stranding the FIRST tag at the far edge behind a
                gap. It needs permission to shrink, not to fill. */}
            <span className="min-w-0">
              <ComponentPips affected={row.affected} total={row.total} health={row.health} />
            </span>
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
    <span className="flex min-w-0 items-center gap-[3px]" aria-hidden>
      {Array.from({ length: shown }, (_, i) => (
        <span
          key={i}
          // 9px when there is room and narrower when there is not — a basis
          // that shrinks rather than a fixed width that overflows the card
          // the moment this sits in a third of a panel. Every cell is kept,
          // because the ratio is the message and a clipped tail would quietly
          // redraw it.
          className="h-[9px] w-[9px] min-w-[2px] shrink rounded-xs"
          style={
            i < bad
              ? {
                  background: `linear-gradient(to bottom, color-mix(in srgb, ${badColor} 80%, white), ${badColor})`,
                  boxShadow: `inset 0 0 0 1px ${badColor}, 0 0 7px 0 color-mix(in srgb, ${badColor} 55%, transparent)`,
                }
              : {
                  background: `color-mix(in srgb, ${okColor} 12%, var(--color-panel))`,
                  boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${okColor} 45%, transparent)`,
                }
          }
        />
      ))}
      {total > CAP && (
        <span className="num ml-1 shrink-0 self-center text-2xs text-ink-4">
          +{total - CAP}
        </span>
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

  const ordered = rows.filter((r) => r.turnedAt);
  const firstTurn = ordered[0]?.turnedAt;

  // The last change before the first thing broke — the candidate trigger.
  // This took the FIRST change in the window, so on an estate that deploys
  // often the chain opened on a deploy from forty minutes earlier.
  const turnMs = firstTurn ? Date.parse(firstTurn) + 60_000 : Infinity;
  const deploy = context.changes
    .filter((c) => incident.profileIds.includes(c.profileId))
    .filter((c) => Date.parse(c.at) >= start && Date.parse(c.at) <= turnMs)
    .sort((a, b) => Date.parse(b.at) - Date.parse(a.at))[0];
  const deployAt = deploy ? clamp((Date.parse(deploy.at) - start) / windowMs) : undefined;

  // The same run-up Evidence draws: from the change to the first failure,
  // deepening toward the moment it broke.
  const turnAt = firstTurn ? clamp((Date.parse(firstTurn) - start) / windowMs) : undefined;
  const runUp =
    deployAt !== undefined && turnAt !== undefined && turnAt > deployAt
      ? { from: deployAt, to: turnAt, color: severityColor(incident) }
      : undefined;

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
            runUp={runUp}
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

        {(ordered.length > 0 || deploy) && (
          <SpreadChain
            rows={rows}
            incident={incident}
            context={context}
            deploy={deploy}
            onOpenService={onOpenService}
          />
        )}
      </div>
    </section>
  );
}

interface ChainStep {
  key: string;
  at?: string;
  /** What links this step to an earlier one, said plainly. */
  link?: { text: string; declared: boolean };
  change?: ChangeEvent;
  row?: PropagationRow;
}

/**
 * The lanes say when; this says why each step is on the list.
 *
 * The same order — the change, what turned first, what followed — with the
 * reason each step is linked to the one before it: a declared dependency,
 * pressure read backwards along one, or nothing but timing. The last is said
 * as such and drawn dashed, rather than drawn like a link it is not.
 */
function SpreadChain({
  rows,
  incident,
  context,
  deploy,
  onOpenService,
}: {
  rows: PropagationRow[];
  incident: Incident;
  context: IncidentContext;
  deploy?: ChangeEvent;
  onOpenService: (profileId: string) => void;
}) {
  const kindOf = new Map(
    context.model.services.map((s) => [s.snapshot.profileId, s.snapshot.kind]),
  );

  const nameOf = nameIn(context);
  const linkFor = (row: PropagationRow, earlier: Set<string>) => {
    for (const reason of incident.grouping) {
      if (reason.kind === "dependencyFailedFirst" && reason.dependent === row.profileId && earlier.has(reason.root)) {
        return { text: `depends on ${nameOf(reason.root)}`, declared: true };
      }
      if (reason.kind === "dependentPressuredIt" && reason.pressured === row.profileId && earlier.has(reason.origin)) {
        return { text: `pressure from ${nameOf(reason.origin)}, which depends on it`, declared: true };
      }
    }
    const upstream = (context.topology.dependsOn[row.profileId] ?? []).find((id) => earlier.has(id));
    if (upstream) return { text: `depends on ${nameOf(upstream)}`, declared: true };
    const downstream = (context.topology.dependents[row.profileId] ?? []).find((id) => earlier.has(id));
    if (downstream) return { text: `${nameOf(downstream)} depends on it`, declared: true };
    return { text: "timing only — no declared link", declared: false };
  };

  // In time order, the change included: a deploy after onset is not a
  // trigger, and placing it where it happened says so without a sentence.
  const timed: ChainStep[] = [
    ...(deploy ? [{ key: `change:${deploy.id}`, at: deploy.at, change: deploy }] : []),
    ...rows.map((row) => ({ key: row.profileId, at: row.turnedAt, row })),
  ].sort((a, b) => {
    if (a.at && b.at) return Date.parse(a.at) - Date.parse(b.at);
    return a.at ? -1 : b.at ? 1 : 0;
  });

  const seen = new Set<string>();
  const steps = timed.map((step, i) => {
    if (step.change) {
      // Its connection counts as "already in the story" for what follows.
      seen.add(step.change.profileId);
      return step;
    }
    const row = step.row!;
    const previous = timed[i - 1];
    const link =
      i === 0
        ? undefined
        : previous?.change && previous.change.profileId === row.profileId
          ? { text: "after its own deploy", declared: true }
          : linkFor(row, seen);
    seen.add(row.profileId);
    return { ...step, link };
  });
  const firstTurned = steps.find((s) => s.row?.turnedAt)?.key;

  return (
    <ol className="mt-4 flex flex-col">
      {steps.map((step, i) => {
        const previous = steps[i - 1];
        const gap =
          i === 0
            ? undefined
            : step.at && previous?.at
              ? gapFrom(previous.at, step.at)
              : "in the incident, no change in this window";
        const color = step.change
          ? "var(--color-accent)"
          : healthColor(step.row!.health);
        // A change links to what follows only by time; between connections
        // the link is whatever the dependency map or the grouping says.
        const declared = step.link ? step.link.declared : !previous?.change;

        return (
          <li key={step.key} className="flex flex-col">
            {gap && (
              <div className="flex min-h-7 items-stretch gap-3">
                <span className="flex w-6 shrink-0 justify-center">
                  <span
                    className="w-px"
                    style={{
                      borderLeft: `1px ${declared ? "solid" : "dashed"} var(--color-line-strong)`,
                    }}
                  />
                </span>
                <span className="num self-center py-1 text-2xs text-ink-4">
                  {gap}
                  {step.link && (
                    <>
                      {" · "}
                      <span className={step.link.declared ? "text-ink-3" : "italic"}>
                        {step.link.text}
                      </span>
                    </>
                  )}
                </span>
              </div>
            )}

            {step.change ? (
              <div className="flex items-center gap-3">
                <span
                  className="grid h-6 w-6 shrink-0 place-items-center rounded-md border"
                  style={{ background: tint(color, 12), borderColor: tint(color, 34) }}
                >
                  <span className="block h-[7px] w-[7px] rotate-45 rounded-[1px]" style={{ background: color }} />
                </span>
                <span className="min-w-0 flex-1 truncate text-xs">
                  <span className="font-medium text-ink">{step.change.component}</span>{" "}
                  {step.change.from && (
                    <>
                      <span className="num text-ink-4">{step.change.from}</span>
                      <span className="text-ink-4"> → </span>
                    </>
                  )}
                  <span className="num text-ink">{step.change.to}</span>
                  <span className="text-ink-4"> on {step.change.profileName}</span>
                </span>
                <span className="num shrink-0 text-2xs text-ink-4">
                  {step.change.change === "deployed" ? "deployed" : "changed"} {clock(step.change.at)}
                </span>
              </div>
            ) : (
              <button
                type="button"
                onClick={() => onOpenService(step.row!.profileId)}
                className="group flex items-center gap-3 rounded-md text-left"
              >
                <span
                  className="grid h-6 w-6 shrink-0 place-items-center rounded-md border"
                  style={{ background: tint(color, 12), borderColor: tint(color, 40) }}
                >
                  {kindOf.get(step.row!.profileId) ? (
                    <ServiceMark descriptor={context.catalog.get(kindOf.get(step.row!.profileId)!)} size={14} />
                  ) : (
                    <Dot health={step.row!.health} size={7} />
                  )}
                </span>
                <span className="min-w-0 flex-1 truncate text-xs font-medium text-ink transition-colors group-hover:text-accent">
                  {step.row!.profileName}
                </span>
                {step.key === firstTurned && (
                  <Tag color="var(--color-crit)">turned first</Tag>
                )}
                <span className="num shrink-0 text-2xs" style={{ color }}>
                  {healthLabel(step.row!.health)}
                </span>
                <span className="num w-10 shrink-0 text-right text-2xs text-ink-4">
                  {step.row!.turnedAt ? clock(step.row!.turnedAt) : "—"}
                </span>
              </button>
            )}
          </li>
        );
      })}
    </ol>
  );
}

function gapFrom(previous: string, current: string): string {
  const mins = Math.round((Date.parse(current) - Date.parse(previous)) / 60_000);
  if (mins <= 0) return "at the same moment";
  return `${mins} minute${mins === 1 ? "" : "s"} later`;
}

// --------------------------------------------------------------- evidence

/**
 * The colour a piece of evidence already has everywhere else in root, so the
 * panel needs no legend of its own: a change is the accent, as the deploy
 * markers on every chart are; an alert is its severity; a metric is data
 * ink; a log line is its level.
 */
function evidenceTone(item: Evidence): string {
  switch (item.kind) {
    case "change":
      return "var(--color-accent)";
    case "alert":
      return SEVERITY[item.severity ?? "warning"].color;
    case "metric":
      return "var(--color-data)";
    case "log":
      return levelTextColor(item.level ?? "error");
  }
}

/** A tone at a strength, for the ground and rule behind it — as `Tag` mixes. */
const tint = (color: string, pct: number) => `color-mix(in srgb, ${color} ${pct}%, transparent)`;

/**
 * One shape per kind, and they are the shapes the charts already draw: a
 * diamond for a change, a dot for an alert. A metric points the way it
 * moved; a log line is a block, as in the Logs activity strip.
 */
function EvidenceGlyph({ item, size = 8 }: { item: Evidence; size?: number }) {
  const color = evidenceTone(item);
  if (item.kind === "metric") {
    const up = (item.movement?.changePct ?? 0) >= 0;
    return (
      <svg width={size + 2} height={size + 2} viewBox="0 0 10 10" aria-hidden className="block">
        <path d={up ? "M5 0.8 9.4 8.8H0.6Z" : "M5 9.2 9.4 1.2H0.6Z"} style={{ fill: color }} />
      </svg>
    );
  }
  const side = item.kind === "change" ? size * 0.8 : item.kind === "log" ? size * 0.85 : size;
  return (
    <span
      aria-hidden
      className={cx(
        "block",
        item.kind === "change" && "rotate-45 rounded-[1px]",
        item.kind === "alert" && "rounded-full",
        item.kind === "log" && "rounded-[2px]",
      )}
      style={{ width: side, height: side, background: color }}
    />
  );
}

/** "4m before", "1h 18m before", "at onset", "2m after". */
function fromOnset(at: string, onset: string): string {
  const mins = Math.round((Date.parse(at) - Date.parse(onset)) / 60_000);
  if (mins === 0) return "at onset";
  return `${span(Math.abs(mins))} ${mins < 0 ? "before" : "after"}`;
}

function span(mins: number): string {
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours >= 48) return `${Math.round(hours / 24)}d`;
  return mins % 60 ? `${hours}h ${mins % 60}m` : `${hours}h`;
}

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
  // One hover, shared by the strip and the list: point at either and its
  // twin lights up, which is what ties a mark on the line to its claim.
  const [hovered, setHovered] = useState<string>();

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

  // The most recent change before the first alert: the line `evidenceFor`
  // ranks first for a reason, marked so it is not just the first of eight.
  const onset = Date.parse(incident.started);
  const lastChange = evidence
    .filter((item) => item.kind === "change" && Date.parse(item.at) <= onset)
    .sort((a, b) => Date.parse(b.at) - Date.parse(a.at))[0]?.id;

  return (
    <section className="panel flex min-h-0 flex-col overflow-hidden">
      <PanelHeader
        title="Evidence"
        subtitle={`${evidence.length} source${evidence.length === 1 ? "" : "s"} · each opens where it came from`}
      />

      {evidence.length > 0 && (
        <LeadUp
          evidence={evidence}
          onset={incident.started}
          hovered={hovered}
          onHover={setHovered}
          onOpen={open}
        />
      )}

      <motion.div variants={stagger(0.04)} {...enter} className="min-h-0 flex-1 overflow-y-auto">
        {evidence.map((item) => (
          <EvidenceRow
            key={item.id}
            item={item}
            onset={incident.started}
            catalog={context.catalog}
            last={item.id === lastChange}
            hovered={hovered === item.id}
            onHover={setHovered}
            onOpen={open}
          />
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

/** Tick candidates, in minutes from onset; whichever fall in range and fit are drawn. */
const LEAD_TICKS = [-2880, -1440, -720, -360, -180, -120, -60, -30, -15, -5, -1, 1, 5, 15, 30, 60];
/** Where the line runs, from the top of the strip. */
const LEAD_BASE = 50;
/** Rise per stacked mark, when marks land on the same moment. */
const LEAD_STEP = 11;

/**
 * The lead-up, drawn: every piece of evidence on one line against the moment
 * the first alert fired.
 *
 * Minutes run on a log scale either side of onset. On a linear one, a change
 * an hour earlier pinned everything else into the last few pixels — and the
 * last few minutes are what this strip exists to show. The uneven ticks say
 * so without a caption.
 */
function LeadUp({
  evidence,
  onset,
  hovered,
  onHover,
  onOpen,
}: {
  evidence: Evidence[];
  onset: string;
  hovered?: string;
  onHover: (id?: string) => void;
  onOpen: (item: Evidence) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setWidth(el.clientWidth);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const t0 = Date.parse(onset);
  const minutes = evidence.map((item) => (Date.parse(item.at) - t0) / 60_000);
  const lo = Math.min(0, ...minutes);
  const hi = Math.max(0, ...minutes);

  const f = (m: number) => Math.sign(m) * Math.log1p(Math.abs(m) / 2);
  const inverse = (v: number) => Math.sign(v) * 2 * Math.expm1(Math.abs(v));
  // Room past each end, and always some after onset, so no mark sits on the
  // edge and onset never reads as where the line stops.
  const a = f(lo) - 0.3;
  const b = Math.max(f(hi), 0.9) + 0.3;
  const PAD = 14;
  const x = (m: number) => PAD + ((f(m) - a) / (b - a)) * Math.max(0, width - 2 * PAD);
  const zero = x(0);

  // Marks on the same moment stack upward rather than hide each other.
  const occupied: number[][] = [];
  const marks = evidence
    .map((item, i) => ({ item, m: minutes[i], px: x(minutes[i]) }))
    .sort((p, q) => p.m - q.m)
    .map((mark) => {
      let level = 0;
      while (level < 3 && (occupied[level] ?? []).some((px) => Math.abs(px - mark.px) < 11)) {
        level++;
      }
      (occupied[level] ??= []).push(mark.px);
      return { ...mark, y: LEAD_BASE - 8 - level * LEAD_STEP };
    });

  // The onset label is the one that must always show; ticks give way to it.
  const onsetLabel = `first alert ${clock(onset)}`;
  const onsetWidth = onsetLabel.length * 5.6;
  const onsetAlign =
    zero > width - onsetWidth / 2 - 4 ? "end" : zero < onsetWidth / 2 + 4 ? "start" : "middle";
  const onsetLeft =
    onsetAlign === "end"
      ? zero - onsetWidth
      : onsetAlign === "start"
        ? zero
        : zero - onsetWidth / 2;
  const kept: number[] = [];
  const ticks = LEAD_TICKS.filter((m) => {
    if (m <= inverse(a) || m >= inverse(b)) return false;
    const px = x(m);
    if (px > onsetLeft - 22 && px < onsetLeft + onsetWidth + 22) return false;
    if (kept.some((other) => Math.abs(other - px) < 40)) return false;
    kept.push(px);
    return true;
  });

  return (
    <div ref={ref} aria-hidden className="relative h-[70px] shrink-0 border-b border-line-soft">
      {width > 0 && (
        <>
          {/* The run-up: a wash that deepens toward onset. */}
          <div
            className="absolute top-2"
            style={{
              left: x(lo) - 6,
              width: Math.max(0, zero - x(lo) + 6),
              height: LEAD_BASE - 8,
              background: `linear-gradient(to right, transparent, ${tint("var(--color-crit)", 16)})`,
              // Rising off the line rather than a box laid on it.
              maskImage: "linear-gradient(to top, black, transparent)",
              WebkitMaskImage: "linear-gradient(to top, black, transparent)",
            }}
          />
          <div className="absolute right-3 left-3 h-px bg-line-strong" style={{ top: LEAD_BASE }} />

          {ticks.map((m) => (
            <span key={m}>
              <span
                className="absolute h-[4px] w-px bg-line-strong"
                style={{ left: x(m), top: LEAD_BASE }}
              />
              <span
                className="num absolute -translate-x-1/2 text-[10px] whitespace-nowrap text-ink-4"
                style={{ left: x(m), top: LEAD_BASE + 6 }}
              >
                {m < 0 ? "−" : "+"}
                {span(Math.abs(m))}
              </span>
            </span>
          ))}

          <motion.div
            className="absolute w-px origin-bottom"
            style={{
              left: zero,
              top: 4,
              height: LEAD_BASE - 4 + 5,
              background: "var(--color-crit)",
            }}
            initial={{ scaleY: 0 }}
            animate={{ scaleY: 1 }}
            transition={EASE_OUT}
          />
          <span
            className="num absolute text-[10px] font-medium whitespace-nowrap"
            style={{
              left: onsetLeft,
              width: onsetWidth,
              top: LEAD_BASE + 6,
              color: "var(--color-crit)",
              textAlign:
                onsetAlign === "middle" ? "center" : onsetAlign === "end" ? "right" : "left",
            }}
          >
            {onsetLabel}
          </span>

          <motion.div variants={stagger(0.06, 0.2)} {...enter}>
            {marks.map(({ item, px, y }) => {
              const tone = evidenceTone(item);
              const on = hovered === item.id;
              return (
                <div key={item.id}>
                  {/* The stem says which moment a stacked mark belongs to. */}
                  <span
                    className="absolute w-px"
                    style={{
                      left: px,
                      top: y,
                      height: LEAD_BASE - y,
                      background: tint(tone, on ? 70 : 38),
                    }}
                  />
                  <button
                    type="button"
                    tabIndex={-1}
                    onMouseEnter={() => onHover(item.id)}
                    onMouseLeave={() => onHover(undefined)}
                    onClick={() => onOpen(item)}
                    title={`${item.headline} — ${fromOnset(item.at, onset)}`}
                    className="absolute grid h-4 w-4 -translate-x-1/2 -translate-y-1/2 place-items-center"
                    style={{ left: px + 0.5, top: y }}
                  >
                    {/* Arrival and hover on separate elements: both are a
                        scale, and one would overwrite the other. */}
                    <motion.span variants={popIn} className="grid place-items-center">
                      <span
                        className={cx(
                          "grid place-items-center rounded-full transition-[scale,box-shadow] duration-150",
                          on && "scale-[1.45]",
                        )}
                        style={{
                          boxShadow: on ? `0 0 0 3px ${tint(tone, 28)}` : undefined,
                        }}
                      >
                        <EvidenceGlyph item={item} size={8} />
                      </span>
                    </motion.span>
                  </button>
                </div>
              );
            })}
          </motion.div>
        </>
      )}
    </div>
  );
}

const rowIn: Variants = {
  hidden: { opacity: 0, y: 3 },
  shown: { opacity: 1, y: 0, transition: EASE_OUT },
};

function EvidenceRow({
  item,
  onset,
  catalog,
  last,
  hovered,
  onHover,
  onOpen,
}: {
  item: Evidence;
  onset: string;
  catalog: Catalog;
  /** The most recent change before the first alert. */
  last: boolean;
  hovered: boolean;
  onHover: (id?: string) => void;
  onOpen: (item: Evidence) => void;
}) {
  const tone = evidenceTone(item);
  const qualifier =
    item.kind === "change"
      ? item.change?.source
      : item.kind === "alert"
        ? item.severity
        : item.kind === "log"
          ? `${item.level}${item.count && item.count > 1 ? ` ×${item.count}` : ""}`
          : undefined;

  return (
    <motion.button
      type="button"
      variants={rowIn}
      onClick={() => onOpen(item)}
      onMouseEnter={() => onHover(item.id)}
      onMouseLeave={() => onHover(undefined)}
      onFocus={() => onHover(item.id)}
      onBlur={() => onHover(undefined)}
      title={`${item.headline}\n${item.provenance}`}
      className={cx(
        "group/row flex w-full items-center gap-3 border-b border-line-soft px-3 py-2 text-left transition-colors last:border-0",
        hovered && "bg-wash",
      )}
    >
      <span
        className="grid h-7 w-7 shrink-0 place-items-center rounded-md border transition-colors"
        style={{
          background: tint(tone, hovered ? 18 : 11),
          borderColor: tint(tone, hovered ? 45 : 28),
        }}
      >
        <EvidenceGlyph item={item} size={9} />
      </span>

      <span className="flex min-w-0 flex-1 flex-col gap-1">
        <span className="flex min-w-0 items-center gap-2 text-xs">
          <EvidenceHeadline item={item} tone={tone} />
          {last && (
            <Tag color="var(--color-accent)" title="The most recent change before the first alert">
              last change before onset
            </Tag>
          )}
        </span>
        <span className="flex min-w-0 items-center gap-1.5 text-2xs text-ink-4">
          <span
            className="shrink-0 font-semibold tracking-[.06em] uppercase"
            style={{ color: tone }}
          >
            {item.kind}
          </span>
          {qualifier && <span className="shrink-0">{qualifier}</span>}
          <span aria-hidden className="shrink-0 text-ink-4/60">
            ·
          </span>
          <ServiceMark descriptor={catalog.get(item.connection.kind)} size={12} />
          <span className="shrink-0 text-ink-3">{item.connection.name}</span>
          {item.detail && (
            <>
              <span aria-hidden className="shrink-0 text-ink-4/60">
                ·
              </span>
              <span className="num min-w-0 truncate">{item.detail}</span>
            </>
          )}
        </span>
      </span>

      <span className="flex shrink-0 items-center gap-2 self-start pt-0.5">
        <span className="num text-2xs whitespace-nowrap text-ink-3">
          {fromOnset(item.at, onset)}
        </span>
        <ExternalIcon
          size={11}
          className="text-ink-4 opacity-0 transition-opacity group-hover/row:opacity-100 group-focus-visible/row:opacity-100"
        />
      </span>
    </motion.button>
  );
}

/** The claim itself, drawn in its parts where it has them. */
function EvidenceHeadline({ item, tone }: { item: Evidence; tone: string }) {
  if (item.kind === "change" && item.change?.from) {
    return (
      <span className="min-w-0 truncate">
        <span className="font-medium text-ink">{item.change.component}</span>{" "}
        <span className="num text-ink-4">{item.change.from}</span>
        <span className="text-ink-4"> → </span>
        <span className="num font-medium text-ink">{item.change.to}</span>
      </span>
    );
  }

  if (item.kind === "metric" && item.movement) {
    const { label, changePct, from, to, unit } = item.movement;
    return (
      <span className="flex min-w-0 items-center gap-2">
        <span className="shrink-0 font-medium text-ink">{label}</span>
        <span
          className="num inline-flex h-[18px] shrink-0 items-center gap-1 rounded-sm px-1.5 text-2xs font-semibold"
          style={{ color: tone, background: tint(tone, 14) }}
        >
          <span className="text-[8px]">{changePct >= 0 ? "▲" : "▼"}</span>
          {Math.abs(Math.round(changePct))}%
        </span>
        <span className="num min-w-0 truncate text-2xs text-ink-4">
          {formatMetricValue(from, unit)} → {formatMetricValue(to, unit)}
        </span>
      </span>
    );
  }

  return (
    <span className={cx("min-w-0 truncate", item.kind === "log" ? "num text-ink-2" : "text-ink-2")}>
      {item.headline}
    </span>
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
              active ? "hue border-line-strong bg-sunk" : "border-line",
              incident.acknowledged && "opacity-60",
            )}
            style={
              {
                borderLeft: `2px solid ${
                  active
                    ? severityColor(incident)
                    : `color-mix(in srgb, ${severityColor(incident)} 45%, transparent)`
                }`,
                "--hue": severityColor(incident),
              } as React.CSSProperties
            }
          >
            <span className="mt-1">
              <Dot health={incident.health} size={9} />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-base text-ink" title={incident.title}>
                {incident.title}
              </span>
              <span className="num mt-1 flex flex-wrap items-center gap-2 text-2xs text-ink-4">
                {/* Whose incident it is, by mark: the queue is read for "is
                    this the payments thing or the search thing" before any
                    number on it is. */}
                <span className="flex items-center -space-x-1">
                  {incident.profileIds.slice(0, 4).map((id) => {
                    const kind = context.model.services.find((s) => s.snapshot.profileId === id)
                      ?.snapshot.kind;
                    return kind ? (
                      <span key={id} title={id} className="rounded-sm ring-2 ring-panel">
                        <ServiceMark descriptor={context.catalog.get(kind)} size={14} />
                      </span>
                    ) : null;
                  })}
                </span>
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
            className="text-2xs font-semibold tracking-[.06em] uppercase"
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
