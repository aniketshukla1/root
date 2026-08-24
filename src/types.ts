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

export interface AppConfig {
  profiles: ConnectionProfile[];
  llm: LlmSettings;
  retention: { logCapacity: number };
  ingest: { enabled: boolean; port: number };
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
