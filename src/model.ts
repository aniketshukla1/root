// The read model: a pure projection from what the backend reports into what the
// screens need. No React, no Tauri — so the severity ranking, the alert/row
// merge, and the grouping can all be reasoned about (and tested) on their own.

import type { Catalog } from "./catalog";
import { isProblem } from "./catalog";
import type {
  Alert,
  AppConfig,
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
  profileName: string;
  kind: ServiceKind;
  category: string;
  name: string;
  section: string;
  summary: string;
  health: Health;
  ownership: Ownership;
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

export interface EstateModel {
  services: ServiceView[];
  entities: Entity[];
  issues: Issue[];
  alerts: Alert[];
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
        profileName: snapshot.profileName,
        kind: snapshot.kind,
        category,
        name: snapshot.profileName,
        section: "Connection",
        summary: snapshot.error ?? snapshot.summary,
        health: snapshot.health,
        ownership,
      },
    ];
  }

  return snapshot.sections.flatMap((section) =>
    section.rows.map((row) => ({
      id: `${snapshot.profileId}:${section.id}:${row.id}`,
      profileId: snapshot.profileId,
      profileName: snapshot.profileName,
      kind: snapshot.kind,
      category,
      name: row.name,
      section: section.label,
      summary: row.summary,
      health: row.health,
      ownership,
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
    counts: {
      connections: 0, healthy: 0, degraded: 0, down: 0, unknown: 0,
      entities: 0, alerts: 0, unacknowledged: 0, critical: 0, assigned: 0, production: 0,
    },
    health: "unknown",
  };
  if (!dashboard) return empty;

  const active = dashboard.alerts.filter((a) => a.active);
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
        const alert = alerts.find((a) => matchesAlert(a, entity.name));
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
        profileName: snapshot.profileName,
        kind: snapshot.kind,
        category,
        name: alert.component ?? alert.title,
        section: "Alert condition",
        summary: alert.detail,
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
      health: worst([snapshot.health, ...entities.map((e) => e.health)]),
    };
  });

  const issues = services.flatMap((s) => s.issues).sort(compareIssues);

  return {
    services,
    entities: allEntities,
    issues,
    alerts: active,
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
