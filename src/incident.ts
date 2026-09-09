/**
 * Reading an incident.
 *
 * The backend decides what is one incident and why (`incidents::correlate`).
 * This module decides how to read it: what the blast radius actually is, which
 * evidence supports the claim, what the app could not see, and what order the
 * failure spread in. All of it is derived from data the collectors already
 * returned — nothing here asks a model, and nothing here invents a fact.
 *
 * The division matters. Anything a model writes is a sentence about evidence;
 * everything in this file IS the evidence, and it must be able to stand on the
 * screen alone when no model is configured at all.
 */

import type {
  Alert,
  ChangeEvent,
  Correlation,
  GroupingReason,
  Health,
  HealthRun,
  Incident,
  LogEvent,
  MetricSeries,
  MetricUnit,
  TopologyView,
} from "./types";
import type { EstateModel } from "./model";

// ------------------------------------------------------------------- state

/**
 * Which of the four home states the estate is in.
 *
 * Derived, never configured: an operator should not be able to put the home
 * screen into a state the estate is not actually in.
 *
 * `storm` outranks `triage` because it is a different problem — many things
 * turning at once is one hypothesis worth testing, not many problems worth
 * queueing, and the screen has to say so before the operator starts working
 * down a list that has no top.
 */
export type HomeState = "quiet" | "incident" | "triage" | "storm";

export function homeState(incidents: Incident[]): HomeState {
  const live = incidents.filter((i) => !i.acknowledged);
  if (live.length === 0) return "quiet";
  if (live.some((i) => i.grouping.some((g) => g.kind === "simultaneousOnset"))) return "storm";
  return live.length === 1 ? "incident" : "triage";
}

/**
 * The one to lead with. Not simply the first: an incident the operator has
 * already acknowledged is someone's open tab, and leading with it would bury
 * the thing nobody has looked at yet.
 */
export function leadIncident(incidents: Incident[]): Incident | undefined {
  return incidents.find((i) => !i.acknowledged) ?? incidents[0];
}

// ------------------------------------------------------------ blast radius

export interface RadiusRow {
  profileId: string;
  profileName: string;
  health: Health;
  affected: number;
  total: number;
  /** True when this connection is where the incident started. */
  root: boolean;
}

export interface BlastRadius {
  connections: number;
  components: number;
  totalComponents: number;
  /** Components outside the incident that are fine. The reassuring number. */
  unaffected: number;
  rows: RadiusRow[];
}

/**
 * How far this reaches, in components rather than connections.
 *
 * A count of connections flatters the incident — "3 connections" sounds like a
 * third of a twelve-connection estate. Six of 364 components is the same
 * failure described honestly, and it is the number that decides whether to
 * wake anyone.
 */
export function blastRadius(incident: Incident, model: EstateModel): BlastRadius {
  const members = new Set(incident.profileIds);
  const roots = new Set(incident.roots);

  const rows: RadiusRow[] = incident.profileIds.map((profileId) => {
    const service = model.services.find((s) => s.snapshot.profileId === profileId);
    return {
      profileId,
      profileName: service?.snapshot.profileName ?? profileId,
      health: service?.health ?? "unknown",
      affected: service?.issues.length ?? 0,
      total: service?.entities.length ?? 0,
      root: roots.has(profileId),
    };
  });

  const components = rows.reduce((sum, r) => sum + r.affected, 0);
  const totalComponents = model.entities.length;

  return {
    connections: members.size,
    components,
    totalComponents,
    unaffected: Math.max(0, totalComponents - components),
    rows,
  };
}

// ---------------------------------------------------------------- evidence

export type EvidenceKind = "alert" | "change" | "metric" | "log";

export interface Evidence {
  id: string;
  kind: EvidenceKind;
  /** The claim itself, in the source's own words wherever possible. */
  headline: string;
  /** Where it came from and when — the part that makes it checkable. */
  provenance: string;
  at: string;
  /** What opening it should show. */
  target:
    | { to: "alerts" }
    | { to: "logs"; profileId: string }
    | { to: "service"; profileId: string }
    | { to: "correlate"; at: string };
}

/**
 * Every claim the screen makes, traced back to a line that can be opened.
 *
 * Ordered by what answers "why" first: the change that preceded it, then the
 * alerts that fired, then what moved, then the log lines. That is the order an
 * operator reconstructs a failure in, and it is deliberately not chronological
 * — the deploy four minutes before the first restart is the most useful line
 * on the screen even though it is the oldest.
 */
export function evidenceFor(
  incident: Incident,
  sources: {
    alerts: Alert[];
    changes: ChangeEvent[];
    logs: LogEvent[];
    correlation?: Correlation;
  },
): Evidence[] {
  const members = new Set(incident.profileIds);
  const started = Date.parse(incident.started);
  const out: Evidence[] = [];

  // A change inside the window before onset is the strongest thing here: it is
  // the only category that can explain rather than describe.
  for (const change of sources.changes) {
    if (!members.has(change.profileId)) continue;
    const at = Date.parse(change.at);
    if (at > started + 60_000) continue;
    out.push({
      id: `change:${change.id}`,
      kind: "change",
      headline: change.from ? `${change.component} ${change.from} → ${change.to}` : change.summary,
      provenance: `change · ${change.source} · ${minutesBefore(at, started)} the first alert`,
      at: change.at,
      target: { to: "service", profileId: change.profileId },
    });
  }

  for (const alert of sources.alerts) {
    if (!incident.alertIds.includes(alert.id)) continue;
    out.push({
      id: `alert:${alert.id}`,
      kind: "alert",
      headline: alert.detail || alert.title,
      provenance: `alert · ${alert.profileName}${alert.component ? ` · ${alert.component}` : ""}`,
      at: alert.firstSeen,
      target: { to: "alerts" },
    });
  }

  // What moved at onset, from the same correlation the Correlate screen uses.
  // Only movements on member connections: the rest of the estate moving is
  // exactly what this incident is NOT about.
  for (const lane of sources.correlation?.lanes ?? []) {
    if (!members.has(lane.profileId)) continue;
    for (const movement of lane.movements.slice(0, 2)) {
      out.push({
        id: `metric:${lane.profileId}:${movement.key}`,
        kind: "metric",
        headline: `${movement.label} ${movement.changePct >= 0 ? "+" : ""}${Math.round(movement.changePct)}%`,
        provenance: `metric · ${lane.profileName}/${movement.key} · around onset`,
        at: incident.started,
        target: { to: "correlate", at: incident.started },
      });
    }
  }

  const errors = sources.logs
    .filter((l) => members.has(l.profileId) && (l.level === "error" || l.level === "fatal"))
    .filter((l) => Date.parse(l.timestamp) >= started - 120_000);
  for (const [message, group] of collapse(errors)) {
    out.push({
      id: `log:${group[0].id}`,
      kind: "log",
      headline: message,
      provenance: `log · ${group[0].source || group[0].profileName} · ${group.length} occurrence${group.length === 1 ? "" : "s"}`,
      at: group[0].timestamp,
      target: { to: "logs", profileId: group[0].profileId },
    });
  }

  return out.slice(0, 8);
}

/**
 * Identical log lines are one piece of evidence that happened N times, not N
 * pieces. Keyed on the exact message rather than a pattern — this is evidence,
 * and a template that merges two genuinely different errors would be a lie in
 * the one place the screen promises not to tell them.
 */
function collapse(logs: LogEvent[]): [string, LogEvent[]][] {
  const groups = new Map<string, LogEvent[]>();
  for (const log of logs) {
    groups.set(log.message, [...(groups.get(log.message) ?? []), log]);
  }
  return [...groups.entries()].sort((a, b) => b[1].length - a[1].length).slice(0, 3);
}

function minutesBefore(at: number, reference: number): string {
  const mins = Math.round((reference - at) / 60_000);
  if (mins <= 0) return "at the same moment as";
  return `${mins} minute${mins === 1 ? "" : "s"} before`;
}

// -------------------------------------------------------------- blind spots

/**
 * What the app could not see.
 *
 * This is the difference between a finding and a guess, and it is the one
 * panel that gets more important as the rest of the screen gets more
 * confident. Every entry is a real absence checked against real state — never
 * a generic caveat, because a caveat that is always there is never read.
 */
export function blindSpots(
  incident: Incident,
  sources: {
    topology: TopologyView;
    changes: ChangeEvent[];
    history: Record<string, MetricSeries[]>;
  },
): string[] {
  const out: string[] = [];
  const members = incident.profileIds;

  const declared = members.filter((id) => (sources.topology.dependsOn[id] ?? []).length > 0);
  if (members.length > 1 && declared.length === 0) {
    out.push(
      "No dependencies are declared between these connections, so the order below is inferred from timing alone.",
    );
  } else if (members.length > 1 && declared.length < members.length) {
    const missing = members.filter((id) => (sources.topology.dependsOn[id] ?? []).length === 0);
    out.push(
      `${missing.join(", ")} ${missing.length === 1 ? "declares" : "declare"} no dependencies, so ${missing.length === 1 ? "its" : "their"} place in the order is inferred from timing.`,
    );
  }

  const started = Date.parse(incident.started);
  const changed = sources.changes.filter(
    (c) => members.includes(c.profileId) && Date.parse(c.at) <= started + 60_000,
  );
  if (changed.length === 0) {
    out.push(
      "No deploy or configuration change was observed before this started — either nothing changed, or the change was somewhere root does not watch.",
    );
  }

  const blind = members.filter((id) => (sources.history[id] ?? []).length === 0);
  if (blind.length > 0) {
    out.push(
      `No metric history for ${blind.join(", ")}, so nothing here can say what ${blind.length === 1 ? "it" : "they"} looked like before.`,
    );
  }

  return out;
}

// ------------------------------------------------------------- propagation

export interface PropagationRow {
  profileId: string;
  profileName: string;
  health: Health;
  runs: HealthRun[];
  /** When this connection turned, or undefined if it never did in the window. */
  turnedAt?: string;
  root: boolean;
}

/**
 * The order it spread in, which is the argument.
 *
 * Sorted by when each connection actually left healthy rather than by name or
 * severity, because reading down the column at the moment of onset is the
 * whole point: the thing at the top turned first.
 */
export function propagation(
  incident: Incident,
  healthRuns: Record<string, HealthRun[]>,
  model: EstateModel,
): PropagationRow[] {
  const roots = new Set(incident.roots);

  return incident.profileIds
    .map((profileId) => {
      const runs = healthRuns[profileId] ?? [];
      const turn = runs.find((r) => r.health === "down" || r.health === "degraded");
      const service = model.services.find((s) => s.snapshot.profileId === profileId);
      return {
        profileId,
        profileName: service?.snapshot.profileName ?? profileId,
        health: service?.health ?? "unknown",
        runs,
        turnedAt: turn?.from,
        root: roots.has(profileId),
      };
    })
    .sort((a, b) => {
      if (a.turnedAt && b.turnedAt) return Date.parse(a.turnedAt) - Date.parse(b.turnedAt);
      return a.turnedAt ? -1 : b.turnedAt ? 1 : 0;
    });
}

// --------------------------------------------------------------- grouping

/**
 * Why these alerts are being shown as one thing, in a sentence.
 *
 * The reason has to be legible for the grouping to be arguable — an operator
 * who cannot see why four alerts were merged cannot tell you the merge is
 * wrong, and a correlator nobody can correct is one nobody should trust.
 */
export function groupingSentence(incident: Incident): string {
  if (incident.grouping.length === 0) {
    return incident.profileIds.length === 1
      ? "One connection, one condition."
      : "Grouped by connection.";
  }

  const parts = incident.grouping.map(describe);
  return parts.join(" ");
}

function describe(reason: GroupingReason): string {
  switch (reason.kind) {
    case "sameConnection":
      return `Several conditions on ${reason.profileId} are one problem with it.`;
    case "dependencyFailedFirst":
      return `${reason.dependent} depends on ${reason.root}, and ${reason.root} failed first.`;
    case "dependentPressuredIt":
      return `${reason.origin} failed first and ${reason.pressured}, which it depends on, degraded shortly after — back-pressure rather than the cause.`;
    case "simultaneousOnset":
      return `${reason.count} independent failures began within ${reason.windowSecs} seconds of each other with nothing declared between them — this may be one event, or a coincidence worth splitting.`;
  }
}

/** What the confidence rating is actually claiming — about the grouping, not the diagnosis. */
export function confidenceMeaning(incident: Incident): string {
  switch (incident.confidence) {
    case "high":
      return incident.profileIds.length === 1
        ? "One connection. Nothing was merged, so there is nothing to get wrong."
        : "One connection explains the rest, and the dependency was declared.";
    case "medium":
      return incident.grouping.some((r) => r.kind === "dependentPressuredIt")
        ? "The edge here is being read backwards — pressure, not causation — so the grouping is sound but the mechanism is worth confirming."
        : "More than one connection could be the origin. The grouping is sound; which one started it is not settled.";
    case "low":
      return "Held together by timing alone. Worth splitting before acting on it as one thing.";
  }
}

// ------------------------------------------------------------ quiet estate

export interface Headroom {
  profileId: string;
  profileName: string;
  label: string;
  value: number;
  limit: number;
  unit: MetricUnit;
  /** 0 to 1 — how much of the declared limit is used. */
  used: number;
}

/**
 * What is closest to a limit, when nothing is wrong yet.
 *
 * The useful question in a quiet estate is not "is everything green" — the
 * header already said so — but "what will go first". Only metrics with a
 * declared threshold appear: a number with no limit has no headroom to
 * report, and inventing one would be the sort of guess this screen exists to
 * avoid.
 */
export function headroom(model: EstateModel, take = 6): Headroom[] {
  const out: Headroom[] = [];
  for (const service of model.services) {
    for (const metric of service.snapshot.metrics) {
      if (metric.warnAbove === undefined || metric.warnAbove === null) continue;
      if (metric.warnAbove <= 0) continue;
      out.push({
        profileId: service.snapshot.profileId,
        profileName: service.snapshot.profileName,
        label: metric.label,
        value: metric.value,
        limit: metric.warnAbove,
        unit: metric.unit,
        used: metric.value / metric.warnAbove,
      });
    }
  }
  return out.sort((a, b) => b.used - a.used).slice(0, take);
}

/**
 * Connections that left healthy at any point in the window, even briefly.
 *
 * A flap that healed is invisible on a screen that only shows current state,
 * and it is the single most useful thing a quiet estate can tell you — it is
 * usually the same connection that is about to stop healing itself.
 */
export function flapped(
  healthRuns: Record<string, HealthRun[]>,
  model: EstateModel,
): { profileId: string; profileName: string; runs: HealthRun[]; worst: Health }[] {
  const order: Health[] = ["unknown", "healthy", "degraded", "down"];
  return model.services
    .map((service) => {
      const runs = healthRuns[service.snapshot.profileId] ?? [];
      const bad = runs.filter((r) => r.health !== "healthy");
      return {
        profileId: service.snapshot.profileId,
        profileName: service.snapshot.profileName,
        runs,
        worst: bad.reduce<Health>(
          (a, r) => (order.indexOf(r.health) > order.indexOf(a) ? r.health : a),
          "healthy",
        ),
        flapped: bad.length > 0,
      };
    })
    .filter((row) => row.flapped)
    .sort((a, b) => order.indexOf(b.worst) - order.indexOf(a.worst));
}
