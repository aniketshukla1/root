// Mirrors the serde representations in src-tauri/src/. Everything crosses the
// boundary as camelCase.
//
// NOTE ON EXTENSIBILITY: `ServiceKind` is deliberately an open `string`, not a
// union of the kinds that exist today. The frontend must never enumerate
// service types — it renders from the ServiceDescriptor catalog the backend
// publishes, so adding a collector in Rust makes it appear here with no
// frontend change. Do not narrow this type.

export type ServiceKind = string;

export type Health = "unknown" | "healthy" | "degraded" | "down";
export type LogLevel = "trace" | "debug" | "info" | "warn" | "error" | "fatal";
export type AlertSeverity = "info" | "warning" | "critical";
export type MetricUnit = "count" | "percent" | "bytes" | "millis" | "seconds" | "per_second";

// ------------------------------------------------------------------ registry

export type FieldKind = "text" | "password" | "number" | "boolean" | "select" | "url";

export interface SelectOption {
  value: string;
  label: string;
}

export interface FieldSpec {
  key: string;
  label: string;
  kind: FieldKind;
  placeholder?: string;
  help?: string;
  required: boolean;
  default?: unknown;
  options?: SelectOption[];
  min?: number;
  max?: number;
  advanced: boolean;
}

export interface ActionSpec {
  tool: string;
  label: string;
}

export interface Capabilities {
  logs: boolean;
  actions: ActionSpec[];
  diagnostics: string[];
}

/** Everything the UI needs to present one kind of infrastructure. */
export interface ServiceDescriptor {
  kind: ServiceKind;
  label: string;
  monogram: string;
  /** Index into the categorical chart palette. */
  accent: number;
  category: string;
  blurb: string;
  endpoint: FieldSpec;
  username?: FieldSpec;
  credential?: FieldSpec;
  fields: FieldSpec[];
  defaultPollSecs: number;
  capabilities: Capabilities;
}

// -------------------------------------------------------------------- domain

export interface Metric {
  key: string;
  label: string;
  value: number;
  unit: MetricUnit;
  warnAbove?: number;
}

// ------------------------------------------------------------------ sections
//
// A service's inventory is a set of sections, each a table of typed rows. This
// replaced a flat `ComponentStatus[]` in which a container, a Jenkins job, an ES
// shard and a running query all shared one shape — so the UI could only ever
// render them identically.

export type CellKind =
  | { type: "text" }
  | { type: "code" }
  | { type: "status" }
  | { type: "metric"; unit: MetricUnit }
  | { type: "duration" }
  | { type: "timestamp" }
  | { type: "tags" }
  | { type: "gauge"; unit: MetricUnit };

export type Align = "start" | "end";

export interface ColumnSpec {
  key: string;
  label: string;
  kind: CellKind;
  align: Align;
  /** The identifying column, rendered with the row's health dot. */
  primary: boolean;
  sortable: boolean;
  /** Hidden on narrow layouts; the row stays readable without it. */
  secondary: boolean;
  /**
   * This column identifies which version of the component is running. The
   * backend diffs it between polls to derive deploy markers; the table renders
   * it like any other column.
   */
  revision: boolean;
}

export type CellValue =
  | { type: "null" }
  | { type: "text"; value: string }
  | { type: "number"; value: number }
  | { type: "duration"; value: number }
  | { type: "timestamp"; value: string }
  | { type: "tags"; value: string[] }
  | { type: "status"; value: Health }
  | { type: "gauge"; value: number; max?: number; warnAbove?: number };

export interface RowAction {
  tool: string;
  label: string;
  args: Record<string, unknown>;
  destructive: boolean;
}

export interface Row {
  id: string;
  name: string;
  health: Health;
  summary: string;
  cells: Record<string, CellValue>;
  actions: RowAction[];
  /** Domain payload only a purpose-built view reads; the generic table ignores it. */
  extra?: unknown;
}

export interface Section {
  id: string;
  label: string;
  columns: ColumnSpec[];
  rows: Row[];
  priority: number;
  emptyText: string;
  /**
   * The backend keeps per-row history for this section's numeric columns, so
   * its components can be charted over time. False for high-cardinality or
   * short-lived sections — those still get a component page, showing live
   * values rather than a series.
   */
  tracked: boolean;
}

export interface HealthSnapshot {
  profileId: string;
  profileName: string;
  kind: ServiceKind;
  health: Health;
  summary: string;
  metrics: Metric[];
  sections: Section[];
  checkedAt: string;
  latencyMs: number;
  error?: string;
}

/**
 * What kind of record a log line is.
 *
 * `runtime` and `event` make up the shared log view; `build` (Jenkins console)
 * lives with its build, and `diagnostic` is derived rather than emitted.
 */
export type LogStream = "runtime" | "event" | "build" | "diagnostic";

export const GLOBAL_STREAMS: LogStream[] = ["runtime", "event"];

export interface LogEvent {
  id: string;
  profileId: string;
  profileName: string;
  kind: ServiceKind;
  source: string;
  level: LogLevel;
  message: string;
  timestamp: string;
  stream: LogStream;
}

export interface Alert {
  id: string;
  fingerprint: string;
  profileId: string;
  profileName: string;
  kind: ServiceKind;
  severity: AlertSeverity;
  title: string;
  detail: string;
  component?: string;
  firstSeen: string;
  lastSeen: string;
  active: boolean;
  acknowledged: boolean;
}

// ------------------------------------------------------------------ changes

export type ChangeKind = "deployed" | "appeared";

/**
 * Where a change came from. `observed` means the app watched a revision move
 * and is certain it happened; `reported` means something external asserted it —
 * a pipeline that posts a deploy and then fails still leaves a marker, so the
 * two must not read identically.
 */
export type ChangeSource = "observed" | "reported";

/**
 * A moment when something about the infrastructure changed, derived by the
 * backend from consecutive snapshots.
 *
 * Not an alert: nothing is wrong, so there is no severity and nothing to
 * acknowledge. These exist so a spike in a chart can be read next to the thing
 * that caused it.
 */
export interface ChangeEvent {
  id: string;
  profileId: string;
  profileName: string;
  kind: ServiceKind;
  change: ChangeKind;
  source: ChangeSource;
  section: string;
  component: string;
  /** Absent when the component is newly present. */
  from?: string;
  to: string;
  at: string;
  summary: string;
}

export interface DashboardState {
  snapshots: HealthSnapshot[];
  alerts: Alert[];
  overallHealth: Health;
  servicesTotal: number;
  servicesHealthy: number;
  servicesDown: number;
  generatedAt: string;
}

// ------------------------------------------------------------------- history

export interface Point {
  t: string;
  v: number;
}

/**
 * Where a metric is heading, and when it reaches its declared limit.
 *
 * Absent whenever the history is too short, too brief, or too noisy to
 * extrapolate from. That absence is the point: a projection nobody can trust
 * is worse than none, because the first wrong one teaches people to ignore the
 * rest. `fit` travels with it so the screen can say how much to believe it.
 */
export interface Trend {
  /** Change per second, in the metric's own unit. */
  perSecond: number;
  /** 0–1. How much of the movement a straight line explains. */
  fit: number;
  windowSecs: number;
  samples: number;
  /** Seconds until it is at or above `warnAbove`. Absent if not heading there. */
  secondsToLimit?: number;
}

/**
 * One measure heading toward a limit, anywhere in the estate.
 *
 * Ranked by the backend because ranking needs every series at once, and the
 * component ones — an NFS export, a container's volume, a single hot node —
 * are one row deep in a place no connection-level view reaches.
 */
export interface CapacityRisk {
  profileId: string;
  /** Absent for connection metrics; identifies the component otherwise. */
  component?: { section: string; row: string };
  key: string;
  label: string;
  unit: MetricUnit;
  value: number;
  limit: number;
  /** Fraction of the limit in use. May exceed 1 — over is a state, not an error. */
  used: number;
  trend?: Trend;
}

export interface MetricSeries {
  key: string;
  label: string;
  unit: MetricUnit;
  warnAbove?: number;
  points: Point[];
  /** Percent change across the window; absent when there is too little history. */
  changePct?: number;
  min: number;
  max: number;
  latest: number;
  trend?: Trend;
}

/** The declared dependency graph, resolved both ways. */
export interface TopologyView {
  dependsOn: Record<string, string[]>;
  dependents: Record<string, string[]>;
  /** Failing connections not explained by another failing connection. */
  roots: string[];
}

/**
 * Why a set of alerts is being shown as one thing.
 *
 * Carried on the incident so a wrong grouping is arguable rather than
 * mysterious: the operator can see the reason and overrule it.
 */
export type GroupingReason =
  | { kind: "sameConnection"; profileId: string }
  | { kind: "dependencyFailedFirst"; root: string; dependent: string }
  /** The edge read backwards: a failing service pressuring what it depends on. */
  | { kind: "dependentPressuredIt"; origin: string; pressured: string }
  /** `count` is independent failures that coincided, not connections. */
  | { kind: "simultaneousOnset"; count: number; windowSecs: number };

/**
 * `low` is not a hedge — it marks a grouping held together by timing alone,
 * which is the one most worth a second look before it is believed.
 */
export type Confidence = "high" | "medium" | "low";

/** The alert stream read as the small number of things actually happening. */
export interface Incident {
  /** Stable while the originating condition persists. */
  id: string;
  title: string;
  severity: AlertSeverity;
  health: Health;
  started: string;
  lastSeen: string;
  /** Every connection involved, worst and earliest first. */
  profileIds: string[];
  alertIds: string[];
  /** Members not explained by another member failing. */
  roots: string[];
  /** The connection the title is about. Never re-derive this in the UI. */
  leadProfileId: string;
  /** True only when every alert in it has been acknowledged. */
  acknowledged: boolean;
  confidence: Confidence;
  grouping: GroupingReason[];
}

/** A period a connection spent in one state, as observed by its collector. */
export interface HealthRun {
  health: Health;
  from: string;
  to: string;
}

export interface HistogramBucket {
  t: string;
  trace: number;
  debug: number;
  info: number;
  warn: number;
  error: number;
  fatal: number;
  total: number;
  /** Counts per connection, so the same buckets stack by severity or by source. */
  byProfile: Record<string, number>;
}

// --------------------------------------------------------------- correlation

export interface MetricMovement {
  key: string;
  label: string;
  unit: MetricUnit;
  from: number;
  to: number;
  changePct: number;
}

export interface CorrelationLane {
  profileId: string;
  profileName: string;
  kind: ServiceKind;
  total: number;
  errorCount: number;
  events: LogEvent[];
  movements: MetricMovement[];
}

export interface Correlation {
  center: string;
  from: string;
  to: string;
  lanes: CorrelationLane[];
  alerts: Alert[];
}

// -------------------------------------------------------------------- config

/** Open map — keys come from the service's declared FieldSpecs. */
export type ProfileOptions = Record<string, unknown>;

/**
 * Human and operational context that travels with a connection.
 *
 * This lives inside an open metadata envelope rather than the collector's
 * options. Collectors stay concerned with how to observe a system; the
 * platform layer owns who is accountable for it and how it should be treated.
 */
export type Environment = "production" | "staging" | "development" | "shared";
export type Criticality = "tier_0" | "tier_1" | "tier_2" | "tier_3";

export interface OperationalMetadata {
  team?: string;
  owner?: string;
  contact?: string;
  environment?: Environment;
  criticality?: Criticality;
  runbookUrl?: string;
}

/** Open by design: integrations can add namespaced metadata without a migration. */
export interface ProfileMetadata extends Record<string, unknown> {
  operations?: OperationalMetadata;
}

export interface ConnectionProfile {
  id: string;
  /**
   * What this connection needs in order to work, as profile ids.
   *
   * Declared, never discovered — root watches connections, not the network
   * between them. Only this direction is stored; dependents are derived,
   * because "what does this need?" is answerable and "what needs this?"
   * usually is not.
   */
  dependsOn: string[];
  name: string;
  kind: ServiceKind;
  endpoint: string;
  username?: string | null;
  hasSecret: boolean;
  enabled: boolean;
  pollIntervalSecs: number;
  options: ProfileOptions;
  metadata: ProfileMetadata;
}

export type LlmProviderKind = "anthropic" | "open_ai_compatible" | "ollama";
export type AgentMode = "read_only" | "confirm_actions";

export interface LlmSettings {
  provider: LlmProviderKind;
  model: string;
  baseUrl?: string | null;
  maxTokens: number;
  temperature: number;
  agentMode: AgentMode;
  hasApiKey: boolean;
}

/**
 * Ascending authority. Viewer is the floor because it is what an SSO domain
 * match grants, and joining a workspace should never be an escalation.
 */
export type Role = "viewer" | "responder" | "admin" | "owner";

export interface Account {
  email: string;
  name: string;
  role: Role;
}

/**
 * Local mode is a value, not an error.
 *
 * `serverUrl` absent means root is doing what it does by default: reading a
 * config file on this machine and answering to nobody. The role here decides
 * which controls are worth drawing; the server decides what they are allowed
 * to do. A client that enforced its own permissions would be enforcing
 * nothing.
 */
export interface TeamState {
  serverUrl?: string;
  account?: Account;
  /** A token exists locally. Not proof the server still honours it. */
  hasSession: boolean;
}

export interface Member {
  email: string;
  name: string;
  role: Role;
  /** Absent for someone invited who has not signed in yet. */
  lastSeen?: string;
  viaSso: boolean;
}

export interface SsoSettings {
  domain?: string;
  defaultRole: Role;
  enforced: boolean;
}

export interface Workspace {
  members: Member[];
  sso: SsoSettings;
}

export interface AppConfig {
  profiles: ConnectionProfile[];
  llm: LlmSettings;
  retention: { logCapacity: number };
  ingest: { enabled: boolean; port: number };
  team: { serverUrl?: string; account?: Account };
}

/**
 * The local deploy-ingest listener. The token is never returned here — only a
 * rotate returns it, and only once.
 */
export interface IngestStatus {
  enabled: boolean;
  port: number;
  hasToken: boolean;
  url: string;
  keychainAvailable: boolean;
}

export interface LogFilter {
  profileIds?: string[];
  kinds?: ServiceKind[];
  minLevel?: LogLevel;
  streams?: LogStream[];
  search?: string;
  since?: string;
  limit?: number;
}

// --------------------------------------------------------------------- agent

export interface AgentStep {
  tool: string;
  arguments: Record<string, unknown>;
  ok: boolean;
  mutating: boolean;
  approved: boolean | null;
  result: string;
  durationMs: number;
}

export interface AgentTurn {
  conversationId: string;
  text: string;
  steps: AgentStep[];
  usage: { inputTokens: number; outputTokens: number };
  truncated: boolean;
}

export interface PendingConfirmation {
  callId: string;
  conversationId: string;
  tool: string;
  description: string;
  equivalentCommand: string;
  profileName: string;
  arguments: Record<string, unknown>;
  requestedAt: string;
}

export type AgentEvent =
  | { type: "thinking"; conversationId: string; text: string }
  | {
      type: "toolStart";
      conversationId: string;
      callId: string;
      tool: string;
      arguments: Record<string, unknown>;
      mutating: boolean;
    }
  | {
      type: "toolEnd";
      conversationId: string;
      callId: string;
      tool: string;
      ok: boolean;
      summary: string;
      durationMs: number;
    }
  | { type: "done"; conversationId: string };

export interface ConnectionStatusEvent {
  profileId: string;
  profileName: string;
  state: "error";
  message: string;
}

export interface ChatEntry {
  id: string;
  role: "user" | "assistant";
  text: string;
  steps?: AgentStep[];
  pending?: boolean;
  error?: boolean;
  usage?: { inputTokens: number; outputTokens: number };
}

export const LOG_LEVELS: LogLevel[] = ["trace", "debug", "info", "warn", "error", "fatal"];

export const HEALTH_ORDER: Health[] = ["down", "degraded", "unknown", "healthy"];
