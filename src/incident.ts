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
  AlertSeverity,
  ChangeEvent,
  Correlation,
  GroupingReason,
  Health,
  HealthRun,
  Incident,
  LogEvent,
  LogLevel,
  MetricMovement,
  MetricSeries,
  ServiceKind,
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

  // The same claim as parts, so the panel can draw it rather than print it:
  // a connection's mark, a severity's colour, a version's before and after.
  // `headline` and `provenance` stay the sentence a screen reader hears.

  /** The connection it came from. */
  connection: { profileId: string; name: string; kind: ServiceKind };
  /** An alert's severity. */
  severity?: AlertSeverity;
  /** A log line's level — the worst of the identical lines folded into it. */
  level?: LogLevel;
  /** A change's before and after. */
  change?: { component: string; from?: string; to: string; source: string };
  /** A metric's movement around onset. */
  movement?: MetricMovement;
  /** How many identical log lines this one stands for. */
  count?: number;
  /** What follows the connection: an alert's component, a log line's source. */
  detail?: string;
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
      connection: {
        profileId: change.profileId,
        name: change.profileName,
        kind: change.kind,
      },
      change: {
        component: change.component,
        from: change.from,
        to: change.to,
        source: change.source,
      },
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
      connection: {
        profileId: alert.profileId,
        name: alert.profileName,
        kind: alert.kind,
      },
      severity: alert.severity,
      detail: alert.component,
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
        connection: {
          profileId: lane.profileId,
          name: lane.profileName,
          kind: lane.kind,
        },
        movement,
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
      connection: {
        profileId: group[0].profileId,
        name: group[0].profileName,
        kind: group[0].kind,
      },
      level: group.some((l) => l.level === "fatal") ? "fatal" : "error",
      count: group.length,
      detail: group[0].source || undefined,
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
export interface BlindSpot {
  text: string;
  /**
   * Where to go to remove this blind spot, when it is one the operator can
   * actually close. A disclosure the reader cannot act on is a shrug; one that
   * hands them the screen that fixes it is a to-do.
   */
  fix?: { label: string; profileId: string };
}

export function blindSpots(
  incident: Incident,
  sources: {
    topology: TopologyView;
    changes: ChangeEvent[];
    history: Record<string, MetricSeries[]>;
  },
): BlindSpot[] {
  const out: BlindSpot[] = [];
  const members = incident.profileIds;

  const missing = members.filter((id) => (sources.topology.dependsOn[id] ?? []).length === 0);
  if (members.length > 1 && missing.length === members.length) {
    out.push({
      text: "No dependencies are declared between these connections, so the order below is inferred from timing alone.",
      fix: { label: `Declare them on ${missing[0]}`, profileId: missing[0] },
    });
  } else if (members.length > 1 && missing.length > 0) {
    out.push({
      text: `${missing.join(", ")} ${missing.length === 1 ? "declares" : "declare"} no dependencies, so ${missing.length === 1 ? "its" : "their"} place in the order is inferred from timing.`,
      fix: { label: `Declare them on ${missing[0]}`, profileId: missing[0] },
    });
  }

  const started = Date.parse(incident.started);
  const changed = sources.changes.filter(
    (c) => members.includes(c.profileId) && Date.parse(c.at) <= started + 60_000,
  );
  if (changed.length === 0) {
    // No `fix` here on purpose: this one may be entirely correct. Nothing
    // changed is a finding, and offering to "fix" it would imply otherwise.
    out.push({
      text: "No deploy or configuration change was observed before this started — either nothing changed, or the change was somewhere root does not watch.",
    });
  }

  const blind = members.filter((id) => (sources.history[id] ?? []).length === 0);
  if (blind.length > 0) {
    out.push({
      text: `No metric history for ${blind.join(", ")}, so nothing here can say what ${blind.length === 1 ? "it" : "they"} looked like before.`,
      fix: { label: `Check ${blind[0]}`, profileId: blind[0] },
    });
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

  return mergeReasons(incident.grouping).map(describe).join(" ");
}

/**
 * One sentence per mechanism, not per edge.
 *
 * The backend emits a reason for every edge it read, so an origin that
 * pressured two dependencies arrived as two reasons — and was written out as
 * the same sentence twice with one name swapped. Edges that share a mechanism
 * and an anchor (the same origin, the same root) are folded into one reason
 * naming every service on the other end.
 */
type MergedReason =
  | Exclude<GroupingReason, { kind: "dependencyFailedFirst" | "dependentPressuredIt" }>
  | { kind: "dependencyFailedFirst"; root: string; dependents: string[] }
  | { kind: "dependentPressuredIt"; origin: string; pressured: string[] };

function mergeReasons(reasons: GroupingReason[]): MergedReason[] {
  const merged: MergedReason[] = [];
  for (const reason of reasons) {
    if (reason.kind === "dependencyFailedFirst") {
      const same = merged.find(
        (m) => m.kind === "dependencyFailedFirst" && m.root === reason.root,
      );
      if (same?.kind === "dependencyFailedFirst") {
        if (!same.dependents.includes(reason.dependent)) same.dependents.push(reason.dependent);
      } else {
        merged.push({ kind: reason.kind, root: reason.root, dependents: [reason.dependent] });
      }
    } else if (reason.kind === "dependentPressuredIt") {
      const same = merged.find(
        (m) => m.kind === "dependentPressuredIt" && m.origin === reason.origin,
      );
      if (same?.kind === "dependentPressuredIt") {
        if (!same.pressured.includes(reason.pressured)) same.pressured.push(reason.pressured);
      } else {
        merged.push({ kind: reason.kind, origin: reason.origin, pressured: [reason.pressured] });
      }
    } else {
      merged.push(reason);
    }
  }
  return merged;
}

/** "a", "a and b", "a, b and c". */
function listOf(names: string[]): string {
  if (names.length <= 1) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

function describe(reason: MergedReason): string {
  switch (reason.kind) {
    case "sameConnection":
      return `Several conditions on ${reason.profileId} are one problem with it.`;
    case "dependencyFailedFirst":
      return `${listOf(reason.dependents)} depend${reason.dependents.length === 1 ? "s" : ""} on ${reason.root}, and ${reason.root} failed first.`;
    case "dependentPressuredIt": {
      const n = reason.pressured.length;
      const which = n === 1 ? "which" : n === 2 ? "both of which" : "all of which";
      return `${reason.origin} failed first; ${listOf(reason.pressured)}, ${which} it depends on, degraded shortly after — back-pressure rather than the cause.`;
    }
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

// ------------------------------------------------------------- suggestions

/**
 * What is worth asking, given what is actually happening.
 *
 * A generic starter list — "what is failing right now?" — asks the model to
 * rediscover something the screen already knows, and spends a tool call doing
 * it. When there is a live incident the useful questions are about THAT: its
 * mechanism, its blast radius, and the one thing the correlator cannot judge,
 * which is whether its own grouping is right.
 */
/** A question worth asking, with what kind of question it is and what answering it reads. */
export interface Starter {
  text: string;
  /** Picks its mark: the same shapes and colours the rest of root uses for each. */
  kind: "why" | "change" | "dependency" | "evidence" | "capacity" | "flap" | "trend";
  /** What the assistant will look at — said up front, so the answer can be checked. */
  reads: string;
}

export function suggestions(incidents: Incident[]): Starter[] {
  const lead = leadIncident(incidents.filter((i) => !i.acknowledged));
  if (!lead) {
    return [
      { text: "What is closest to a limit across the estate?", kind: "capacity", reads: "metric trends against thresholds" },
      { text: "What changed in the last hour?", kind: "change", reads: "observed and reported changes" },
      { text: "Which connections have flapped today?", kind: "flap", reads: "health runs and alert history" },
      { text: "Is anything trending toward a threshold?", kind: "trend", reads: "metric history" },
    ];
  }

  const root = lead.leadProfileId || lead.profileIds[0];
  const out: Starter[] = [
    {
      text: `Why is ${root} failing, and what should I check first?`,
      kind: "why",
      reads: `alerts, metrics and error lines on ${root}`,
    },
    {
      text: `What changed on ${root} before this started?`,
      kind: "change",
      reads: "changes and deploys before onset",
    },
  ];

  if (lead.profileIds.length > 1) {
    const others = lead.profileIds.filter((id) => id !== root);
    out.push({
      text: `Is ${others[0]} failing because of ${root}, or is that a separate problem?`,
      kind: "dependency",
      reads: "declared dependencies and timing",
    });
  }
  if (lead.confidence !== "high") {
    out.push({
      text: "Does the evidence actually support grouping these as one incident?",
      kind: "evidence",
      reads: "the incident's grouping reasons",
    });
  } else {
    out.push({
      text: `What else depends on ${root} that has not failed yet?`,
      kind: "dependency",
      reads: "the topology around it",
    });
  }

  return out;
}
