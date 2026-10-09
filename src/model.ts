// The read model: a pure projection from what the backend reports into what the
// screens need. No React, no Tauri — so the severity ranking, the alert/row
// merge, and the grouping can all be reasoned about (and tested) on their own.

import type { Catalog } from "./catalog";
import { isProblem } from "./catalog";
import type {
  Alert,
  AlertSeverity,
  AppConfig,
  CellValue,
  ColumnSpec,
  ConnectionProfile,
  Criticality,
  HealthRun,
  DashboardState,
  Environment,
  Health,
  HealthSnapshot,
  OperationalMetadata,
  ServiceKind,
} from "./types";
import { HEALTH_ORDER } from "./types";

// ---------------------------------------------------------------- ownership

export interface Ownership {
  team: string;
  owner: string;
  contact: string;
  environment: Environment;
  criticality: Criticality;
  runbookUrl?: string;
  /** False when nobody has been named. Never guessed — it lowers coverage instead. */
  assigned: boolean;
}

const UNASSIGNED: Ownership = {
  team: "Unassigned",
  owner: "No responder",
  contact: "",
  environment: "shared",
  criticality: "tier_2",
  assigned: false,
};

function clean(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function asEnvironment(value: unknown): Environment {
  return value === "production" || value === "staging" || value === "development" || value === "shared"
    ? value
    : "shared";
}

function asCriticality(value: unknown): Criticality {
  return value === "tier_0" || value === "tier_1" || value === "tier_2" || value === "tier_3"
    ? value
    : "tier_2";
}

/** Resolved without ever making config availability a rendering dependency. */
export function ownershipFor(profileId: string, config: AppConfig | null): Ownership {
  const raw = config?.profiles.find((p) => p.id === profileId)?.metadata?.operations as
    | OperationalMetadata
    | undefined;
  const team = clean(raw?.team);
  const owner = clean(raw?.owner);
  if (!team && !owner) return UNASSIGNED;

  return {
    team: team ?? "Independent owner",
    owner: owner ?? "Team rotation",
    contact: clean(raw?.contact) ?? "",
    environment: asEnvironment(raw?.environment),
    criticality: asCriticality(raw?.criticality),
    runbookUrl: clean(raw?.runbookUrl),
    assigned: true,
  };
}

export function criticalityRank(value: Criticality): number {
  return ["tier_0", "tier_1", "tier_2", "tier_3"].indexOf(value);
}

export function criticalityLabel(value: Criticality): string {
  return value.replace("tier_", "T");
}

export function environmentLabel(value: Environment): string {
  return value === "development" ? "Dev" : value.charAt(0).toUpperCase() + value.slice(1);
}

export function initials(value: string): string {
  const parts = value.split(/[\s@._-]+/).filter(Boolean);
  return (parts.length > 1 ? `${parts[0][0]}${parts[1][0]}` : parts[0]?.slice(0, 2) ?? "?").toUpperCase();
}

// ------------------------------------------------------------------ entities

/** One monitored thing: a container, a job, a shard, an index, a query, a node. */
export interface Entity {
  id: string;
  profileId: string;
  /** Route parts, carried rather than parsed back out of `id` at the call site. */
  sectionId: string;
  rowId: string;
  profileName: string;
  kind: ServiceKind;
  category: string;
  name: string;
  section: string;
  summary: string;
  health: Health;
  ownership: Ownership;
  /**
   * What the collector actually reported about this one thing, carried by
   * reference rather than copied — the same objects the section table renders.
   *
   * Without these an entity is a name, a health and one terse summary line,
   * which is all the component matrix had to show for a cell: fine for a
   * failing pod whose summary says "exited 137 · OOMKilled", useless for a
   * healthy one, whose summary is usually empty. The numbers are the answer to
   * "what is this thing doing", and they exist either way.
   */
  cells: Record<string, CellValue>;
  columns: ColumnSpec[];
}

export interface Issue extends Entity {
  /** 1 = down, 2 = degraded. Alert severity can promote a degraded row to 1. */
  priority: 1 | 2;
  alert?: Alert;
  firstSeen: string;
  acknowledged: boolean;
}

export interface ServiceView {
  snapshot: HealthSnapshot;
  profile?: ConnectionProfile;
  category: string;
  ownership: Ownership;
  entities: Entity[];
  issues: Issue[];
  alerts: Alert[];
  /** Worst of the snapshot and its rows — a healthy snapshot cannot hide a down row. */
  health: Health;
}

/**
 * Alert severities, most urgent first.
 *
 * The prediction engine speaks in horizons — `now`, `soon`, `advisory` — which
 * map onto these, so sorting a prediction list by severity sorts it by how
 * long there is to act.
 */
const SEVERITY_ORDER: AlertSeverity[] = ["critical", "warning", "info"];

export interface EstateModel {
  services: ServiceView[];
  entities: Entity[];
  issues: Issue[];
  /** Active, and real — what is true now. Never includes a forecast. */
  alerts: Alert[];
  /**
   * Active predictions, soonest first.
   *
   * Kept out of `alerts` and out of every count derived from it: a forecast
   * must not make the estate read as on fire, and an alert count that a
   * forecast can inflate is a count nobody can act on.
   */
  predictions: Alert[];
  counts: {
    connections: number;
    healthy: number;
    degraded: number;
    down: number;
    unknown: number;
    entities: number;
    alerts: number;
    unacknowledged: number;
    critical: number;
    assigned: number;
    production: number;
  };
  health: Health;
}

function worst(healths: Health[]): Health {
  return healths.reduce<Health>(
    (acc, h) => (HEALTH_ORDER.indexOf(h) < HEALTH_ORDER.indexOf(acc) ? h : acc),
    "healthy",
  );
}

function matchesAlert(alert: Alert, name: string): boolean {
  const needle = name.toLowerCase();
  return alert.component?.toLowerCase() === needle || alert.title.toLowerCase().includes(needle);
}

function entitiesOf(snapshot: HealthSnapshot, category: string, ownership: Ownership): Entity[] {
  // A connection that failed to collect, or one whose collector reports no
  // inventory, is still one monitored thing — otherwise it would vanish from
  // every count at exactly the moment it broke.
  if (snapshot.error || snapshot.sections.every((s) => s.rows.length === 0)) {
    return [
      {
        id: snapshot.profileId,
        profileId: snapshot.profileId,
        sectionId: "",
        rowId: "",
        profileName: snapshot.profileName,
        kind: snapshot.kind,
        category,
        name: snapshot.profileName,
        section: "Connection",
        summary: snapshot.error ?? snapshot.summary,
        health: snapshot.health,
        ownership,
        cells: {},
        columns: [],
      },
    ];
  }

  return snapshot.sections.flatMap((section) =>
    section.rows.map((row) => ({
      id: `${snapshot.profileId}:${section.id}:${row.id}`,
      profileId: snapshot.profileId,
      sectionId: section.id,
      rowId: row.id,
      profileName: snapshot.profileName,
      kind: snapshot.kind,
      category,
      name: row.name,
      section: section.label,
      summary: row.summary,
      health: row.health,
      ownership,
      cells: row.cells,
      columns: section.columns,
    })),
  );
}

/** Severity, then unacknowledged, then tier, then age. */
export function compareIssues(a: Issue, b: Issue): number {
  return (
    a.priority - b.priority ||
    Number(a.acknowledged) - Number(b.acknowledged) ||
    criticalityRank(a.ownership.criticality) - criticalityRank(b.ownership.criticality) ||
    new Date(a.firstSeen).getTime() - new Date(b.firstSeen).getTime()
  );
}

export function buildModel(
  dashboard: DashboardState | null,
  config: AppConfig | null,
  catalog: Catalog,
): EstateModel {
  const empty: EstateModel = {
    services: [],
    entities: [],
    issues: [],
    alerts: [],
    predictions: [],
    counts: {
      connections: 0, healthy: 0, degraded: 0, down: 0, unknown: 0,
      entities: 0, alerts: 0, unacknowledged: 0, critical: 0, assigned: 0, production: 0,
    },
    health: "unknown",
  };
  if (!dashboard) return empty;

  const active = dashboard.alerts.filter((a) => a.active);

  // Soonest first. Severity is the horizon the prediction engine speaks in —
  // `now`, `soon`, `advisory` — so ordering by it is ordering by how long
  // there is to act, which is the only ordering this list has a use for.
  const predictions = (dashboard.predictions ?? [])
    .filter((a) => a.active)
    .sort((a, b) => SEVERITY_ORDER.indexOf(a.severity) - SEVERITY_ORDER.indexOf(b.severity));

  const allEntities: Entity[] = [];

  const services = dashboard.snapshots.map<ServiceView>((snapshot) => {
    const category = catalog.category(snapshot.kind);
    const ownership = ownershipFor(snapshot.profileId, config);
    const alerts = active.filter((a) => a.profileId === snapshot.profileId);
    const entities = entitiesOf(snapshot, category, ownership);
    allEntities.push(...entities);

    const issues: Issue[] = entities
      .filter((e) => isProblem(e.health))
      .map((entity) => {
        // A section whose rows come and go (running queries, pods, events)
        // raises one alert for the whole section, fingerprinted
        // `profile:section` by the backend; its rows belong to that alert.
        const alert =
          alerts.find((a) => matchesAlert(a, entity.name)) ??
          alerts.find((a) => a.fingerprint === `${snapshot.profileId}:${entity.sectionId}`);
        return {
          ...entity,
          priority: entity.health === "down" ? 1 : 2,
          alert,
          firstSeen: alert?.firstSeen ?? snapshot.checkedAt,
          acknowledged: alert?.acknowledged ?? false,
        };
      });

    // A collector can raise a service-level threshold alert with no failing
    // inventory row behind it. That signal has to stay visible, so it becomes
    // an issue in its own right rather than being dropped.
    for (const alert of alerts) {
      if (issues.some((i) => i.alert?.id === alert.id)) continue;
      issues.push({
        id: `alert:${alert.id}`,
        profileId: snapshot.profileId,
        sectionId: "",
        rowId: "",
        profileName: snapshot.profileName,
        kind: snapshot.kind,
        category,
        name: alert.component ?? alert.title,
        section: "Alert condition",
        summary: alert.detail,
        cells: {},
        columns: [],
        health: alert.severity === "critical" ? "down" : "degraded",
        ownership,
        priority: alert.severity === "critical" ? 1 : 2,
        alert,
        firstSeen: alert.firstSeen,
        acknowledged: alert.acknowledged,
      });
    }

    issues.sort(compareIssues);

    return {
      snapshot,
      profile: config?.profiles.find((p) => p.id === snapshot.profileId),
      category,
      ownership,
      entities,
      issues,
      alerts,
      // A row the collector calls unknown has no opinion — a finished one-shot
      // container, a job never built, a trace in a list — and every collector
      // leaves those out of its own rollup. Counting them here made a healthy
      // Docker host read "unknown" for one completed job. The snapshot's own
      // unknown still counts: that one means the connection itself is in doubt.
      health: worst([
        snapshot.health,
        ...entities.map((e) => e.health).filter((h) => h !== "unknown"),
      ]),
    };
  });

  const issues = services.flatMap((s) => s.issues).sort(compareIssues);

  return {
    services,
    entities: allEntities,
    issues,
    alerts: active,
    predictions,
    counts: {
      connections: services.length,
      healthy: services.filter((s) => s.health === "healthy").length,
      degraded: services.filter((s) => s.health === "degraded").length,
      down: services.filter((s) => s.health === "down").length,
      unknown: services.filter((s) => s.health === "unknown").length,
      entities: allEntities.length,
      alerts: active.length,
      unacknowledged: active.filter((a) => !a.acknowledged).length,
      critical: active.filter((a) => a.severity === "critical").length,
      assigned: services.filter((s) => s.ownership.assigned).length,
      production: services.filter((s) => s.ownership.environment === "production").length,
    },
    health: services.length === 0 ? "unknown" : worst(services.map((s) => s.health)),
  };
}

// ------------------------------------------------------------------ grouping

export type GroupBy = "category" | "team" | "environment" | "health" | "kind";

export const GROUP_LABELS: Record<GroupBy, string> = {
  category: "Category",
  team: "Team",
  environment: "Environment",
  health: "Status",
  kind: "Type",
};

export interface Group {
  key: string;
  label: string;
  services: ServiceView[];
  health: Health;
  issues: number;
}

export function groupServices(
  services: ServiceView[],
  by: GroupBy,
  catalog: Catalog,
): Group[] {
  const keyOf = (s: ServiceView): string => {
    switch (by) {
      case "category": return s.category;
      case "team": return s.ownership.team;
      case "environment": return environmentLabel(s.ownership.environment);
      case "health": return s.health;
      case "kind": return catalog.label(s.snapshot.kind);
    }
  };

  const groups = new Map<string, ServiceView[]>();
  for (const service of services) {
    const key = keyOf(service);
    groups.set(key, [...(groups.get(key) ?? []), service]);
  }

  return [...groups.entries()]
    .map<Group>(([key, list]) => ({
      key,
      label: by === "health" ? key.charAt(0).toUpperCase() + key.slice(1) : key,
      services: list,
      health: worst(list.map((s) => s.health)),
      issues: list.reduce((sum, s) => sum + s.issues.length, 0),
    }))
    // Worst first, then most issues — the group that needs attention is on top.
    .sort(
      (a, b) =>
        HEALTH_ORDER.indexOf(a.health) - HEALTH_ORDER.indexOf(b.health) ||
        b.issues - a.issues ||
        a.label.localeCompare(b.label),
    );
}

// -------------------------------------------------------------------- lanes

/**
 * State per connection across a window, as continuous runs.
 *
 * These are OBSERVED, not inferred: the collector records one health sample
 * per poll and the backend collapses them into runs, so a gap wider than a
 * connection's own cadence comes back as `unknown` rather than silently
 * extending the last known state. A lane therefore says what was seen, which
 * is the claim the chart needs to be able to make.
 *
 * This replaced a derivation from alert windows, which could only ever say
 * "this connection was alerting" — blind to any flap nobody had thresholded.
 */
export interface LaneRun {
  health: Health;
  /** Fractions of the window; 0 is its left edge and 1 is now. */
  from: number;
  to: number;
}

export interface LaneView {
  id: string;
  name: string;
  health: Health;
  runs: LaneRun[];
}

export function lanesFor(
  model: EstateModel,
  runsByProfile: Record<string, HealthRun[]>,
  windowMs: number,
  now = Date.now(),
): LaneView[] {
  const start = now - windowMs;
  const clamp = (v: number) => Math.max(0, Math.min(1, v));

  return [...model.services]
    .sort(
      (a, b) =>
        HEALTH_ORDER.indexOf(a.health) - HEALTH_ORDER.indexOf(b.health) ||
        b.alerts.length - a.alerts.length ||
        a.snapshot.profileName.localeCompare(b.snapshot.profileName),
    )
    .map((service) => {
      const observed = runsByProfile[service.snapshot.profileId] ?? [];
      const runs: LaneRun[] = observed
        .map((run) => ({
          health: run.health,
          from: clamp((Date.parse(run.from) - start) / windowMs),
          to: clamp((Date.parse(run.to) - start) / windowMs),
        }))
        .filter((run) => run.to > run.from);

      return {
        id: service.snapshot.profileId,
        name: service.snapshot.profileName,
        health: service.health,
        // A connection the ring has never sampled is unknown for the window,
        // never healthy by default.
        runs: runs.length > 0 ? runs : [{ health: "unknown" as Health, from: 0, to: 1 }],
      };
    });
}
