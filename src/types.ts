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
  /**
   * This column holds a distributed-trace id. The table renders such a cell as
   * an opener rather than as text, for any collector that declares one — the
   * frontend never learns which service types have traces.
   */
  traceRef?: boolean;
  /** This column holds a profile selector; the cell opens a flame graph. */
  profileRef?: boolean;
}

export type SpanStatus = "unset" | "ok" | "error";

export interface SpanEvent {
  at: string;
  name: string;
}

export interface Span {
  spanId: string;
  parentSpanId?: string;
  name: string;
  service: string;
  startedAt: string;
  durationMs: number;
  status: SpanStatus;
  statusMessage?: string;
  attributes?: Record<string, string>;
  events?: SpanEvent[];
}

export interface TraceNode {
  span: Span;
  /** Milliseconds from the start of the trace; the bar's left edge. */
  offsetMs: number;
  /** Duration minus what the children accounted for — where time went. */
  selfMs: number;
  depth: number;
  /** Parent missing from the trace, so it hangs at the root. */
  orphaned?: boolean;
  children: TraceNode[];
}

export interface ServiceTime {
  service: string;
  selfMs: number;
  spanCount: number;
  errorCount: number;
}

export interface Trace {
  traceId: string;
  rootService: string;
  rootName: string;
  startedAt: string;
  durationMs: number;
  spanCount: number;
  errorCount: number;
  /** Nonzero means the trace is partial: some parents were never received. */
  orphanCount: number;
  /** Descending by self time. */
  services: ServiceTime[];
  roots: TraceNode[];
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

// ------------------------------------------------------------------ history
//
// What the database kept, as opposed to what the ring buffer is holding. These
// are records rather than working objects: they are read to be displayed and
// counted, never acted on, so their enums arrive as the strings they were
// stored as and there is no acknowledging or resolving them from here.

/** One past firing of a condition, including when it ended. */
export interface PastAlert {
  fingerprint: string;
  profileId: string;
  profileName: string;
  severity: string;
  title: string;
  detail: string;
  component?: string;
  /** Always false in this list — forecasts are kept apart on disk. */
  predicted: boolean;
  firstSeen: string;
  lastSeen: string;
  /** Absent while the condition is still firing. */
  resolvedAt?: string;
}

/** One recorded change, with every column it was written with. */
export interface PastChange {
  id: string;
  at: string;
  profileId: string;
  profileName: string;
  kind: string;
  change: string;
  source: string;
  section?: string;
  component?: string;
  from?: string;
  to?: string;
  summary: string;
}

/**
 * A condition and how often it fired over the window.
 *
 * One alert that fired forty times and forty that fired once look identical in
 * a list ordered by recency, and they are completely different problems.
 */
export interface FlappingCondition {
  profileName: string;
  title: string;
  times: number;
}

/** What the history store is holding, for the Settings panel. */
export interface HistoryStats {
  /**
   * Whether history is kept at all. True by default now: without a database
   * root keeps a month in a local file. False only when neither would open.
   */
  configured: boolean;
  /** Whether the rollup, compression and expiry are the database's job. */
  timescale: boolean;
  /** `local` (the SQLite file beside the config) or `postgres`. */
  backend?: "local" | "postgres";
  /** The local file's size on disk. */
  bytes?: number;
  rows: Record<string, number>;
}

export interface DashboardState {
  snapshots: HealthSnapshot[];
  /** Conditions that are true now. */
  alerts: Alert[];
  /**
   * What the prediction engine expects to become true.
   *
   * A separate field rather than a flag on the alerts, so a surface that has
   * not been taught about predictions cannot show one by accident.
   */
  predictions: Alert[];
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
export type RuntimeKind = "undeclared" | "container" | "pod" | "host" | "managed";

/**
 * Where a connection actually runs, as declared.
 *
 * root infers what a container is from its image and its published port. That
 * is reading rather than guessing, but nothing checks it — a declaration gives
 * the inference something to be wrong against, and the disagreements are the
 * point.
 */
export interface Runtime {
  kind: RuntimeKind;
  /** The Docker host or Kubernetes cluster it runs on. */
  hostProfileId?: string;
  /** The container or pod name, as the host reports it. */
  name?: string;
}

export type Discrepancy =
  | { kind: "hostNotConfigured"; hostProfileId: string }
  | { kind: "notFound"; host: string; name: string }
  | { kind: "disagrees"; host: string; declared: string; inferred: string }
  | { kind: "notServing"; host: string; name: string }
  | { kind: "incomplete" };

export interface RuntimeCheck {
  profileId: string;
  profileName: string;
  declared: Runtime;
  discrepancies: Discrepancy[];
}

/** A component that looks like a service with nothing watching it. */
export interface Uncovered {
  hostProfileId: string;
  hostLabel: string;
  name: string;
  kind: ServiceKind;
}

export interface RuntimeReport {
  /** Only connections with something to say — agreement is not news. */
  checks: RuntimeCheck[];
  uncovered: Uncovered[];
  declared: number;
  total: number;
}

/** Where a connection's configuration lives. Declared, never discovered. */
export interface ConfigSource {
  id: string;
  label?: string;
  /** Absolute path to a file or a directory. */
  path: string;
  /** A glob applied under `path` when it is a directory. */
  include?: string;
}

export type ConfigFormat = "ini" | "yaml" | "json" | "toml" | "xml" | "properties" | "text";

export type RedactionReason =
  | "keyName"
  | "privateKey"
  | "jwt"
  | "urlCredentials"
  | "providerToken"
  | "authorizationHeader";

export interface Redaction {
  line: number;
  key?: string;
  reason: RedactionReason;
}

/**
 * Why a document has no body.
 *
 * Withholding is a result, not a failure. "There is a .env here and it was not
 * opened" is more useful to render than either its contents or its absence.
 */
export type Withheld =
  | { kind: "secret" }
  | { kind: "tooLarge"; bytes: number }
  | { kind: "binary" }
  | { kind: "unreadable"; message: string };

/**
 * Where a document came from.
 *
 * A file is what is on disk, which may not be what the process loaded. Live is
 * what the target is actually running with. The two disagreeing is the
 * commonest config bug there is.
 */
export type ConfigOrigin = { kind: "file"; path: string } | { kind: "live"; via: string };

export interface ConfigDocument {
  id: string;
  profileId: string;
  label: string;
  origin: ConfigOrigin;
  format: ConfigFormat;
  bytes: number;
  modified?: string;
  /** Redacted before it ever left the backend. Absent when `withheld` says why. */
  body?: string;
  withheld?: Withheld;
  redactions?: Redaction[];
  readAt: string;
}

export type Severity = "advisory" | "soon" | "now";

// ------------------------------------------------------- the judgement record

/**
 * Was it a real problem?
 *
 * Four answers rather than a thumb, because "bad" collapses three unrelated
 * failures into one number and each is fixed somewhere different.
 */
export type ProblemVerdict = "real" | "notAProblem" | "alreadyKnew" | "unsure";

/**
 * Was the suggested fix the right one?
 *
 * Separate from the problem verdict: a single rating cannot say which half
 * failed, and a wrong detection and a wrong remedy are fixed in different
 * places.
 */
export type FixVerdict = "right" | "wrong" | "partlyRight" | "unsure";

/**
 * Where a finding came from.
 *
 * Rules never invent a setting but only find what someone encoded; the model
 * reaches further and can be confidently wrong. A score that mixes them
 * describes neither.
 */
export type Provenance =
  | { rule: { id: string } }
  | { model: { model: string } }
  | { modelCitingRule: { model: string; rule: string } };

export interface Verdict {
  findingId: string;
  profileId: string;
  profileName: string;
  provenance: Provenance;
  /** The finding as judged, stored whole so the score stays interpretable. */
  snapshot: unknown;
  problem: ProblemVerdict;
  fix?: FixVerdict;
  note?: string;
}

export interface RecordedVerdict extends Omit<Verdict, "provenance"> {
  id: number;
  at: string;
  provenance: string;
  family: string;
  fingerprint: string;
  rulesVersion: string;
}

/** Did root see this coming? `no` is the miss — the measurement thumbs can't produce. */
export type Called = "yes" | "no" | "partly";

export interface Recall {
  incidentId: string;
  headline: string;
  profileIds: string[];
  startedAt: string;
  called: Called;
  findingId?: string;
  note?: string;
}

export interface FamilyScore {
  family: string;
  judged: number;
  real: number;
  notAProblem: number;
  alreadyKnew: number;
  unsure: number;
  fixRight: number;
  fixWrong: number;
  fixPartly: number;
}

export interface RuleScore {
  rule: string;
  judged: number;
  real: number;
  notAProblem: number;
}

export interface RecallScore {
  incidents: number;
  called: number;
  missed: number;
  partly: number;
}

export interface Scorecard {
  byFamily: FamilyScore[];
  byRule: RuleScore[];
  recall: RecallScore;
  rulesVersion: string;
}

// --------------------------------------------------------- scenario library

/**
 * Why a scenario did not pass.
 *
 * `missed` and `spurious` are counted apart for the same reason precision and
 * recall are: a suite drifting blinder and one drifting noisier need opposite
 * responses.
 */
export type Failure =
  | { kind: "missed"; rule: string }
  | { kind: "spurious"; rule: string; title: string }
  | { kind: "wrongSeverity"; rule: string; expected: Severity; actual: Severity };

export interface ScenarioOutcome {
  id: string;
  title: string;
  /** Why this case matters — so a failure explains itself. */
  why: string;
  kind: string;
  engine: "rules";
  passed: boolean;
  failures?: Failure[];
  fired: string[];
}

export interface SuiteResult {
  total: number;
  passed: number;
  missed: number;
  spurious: number;
  wrongSeverity: number;
  outcomes: ScenarioOutcome[];
}

// ------------------------------------------------------------------ backtest

/**
 * Something the reader must know before believing a backtest number.
 *
 * `configIsCurrent` is always present: config has no history, so a replay
 * measures yesterday's metrics against today's settings.
 */
export type Caveat =
  | { kind: "configIsCurrent" }
  | { kind: "thinHistory"; samples: number }
  | { kind: "noHistorySource" };

/** One stretch during which a rule would have been saying something. */
export interface Episode {
  rule: string;
  title: string;
  severity: Severity;
  from: string;
  to: string;
  evaluations: number;
}

export interface RuleBehaviour {
  rule: string;
  episodes: number;
  firingSecs: number;
  /** Above roughly a third, a rule has stopped being a warning. */
  dutyCycle: number;
  worst: Severity;
}

export interface Coverage {
  incidentId: string;
  headline: string;
  startedAt: string;
  /** What a human recorded at the time — the label this is scored against. */
  recorded: Called;
  rule?: string;
  leadSecs?: number;
  /** A recorded miss the current rules would now catch. */
  newlyCaught: boolean;
}

export interface Replay {
  profileId: string;
  profileName: string;
  from: string;
  to: string;
  evaluations: number;
  samples: number;
  episodes: Episode[];
  behaviour: RuleBehaviour[];
  coverage: Coverage[];
  caveats: Caveat[];
}

// -------------------------------------------------------- long-range history

/** One metric key, and the PromQL that produces it. */
export interface MetricQuery {
  metricKey: string;
  expr: string;
}

/**
 * Where a connection's long-range history lives.
 *
 * root keeps hours; a forecast that matters needs weeks. Declared, because
 * `memory` and `redis_memory_used_bytes` cannot be matched up by inference.
 */
export interface HistorySource {
  prometheusProfileId: string;
  queries: MetricQuery[];
}

/**
 * Where a connection's snapshots are written, when anywhere.
 *
 * Absent for almost every connection. root reads; this is the one place it
 * writes to somebody else's store, so it is turned on one connection at a
 * time and never inferred.
 */
export interface SnapshotSink {
  warehouseProfileId: string;
  /** Measurement name. Empty means the connection's own name is used. */
  measurement: string;
}

export type Evidence =
  | { kind: "setting"; key: string; value: string; source: string }
  | { kind: "metric"; key: string; label: string; value: number; unit: MetricUnit; limit?: number }
  | { kind: "projection"; key: string; secondsToLimit: number; fit: number; windowSecs: number };

export interface Suggestion {
  setting: string;
  current: string;
  proposed: string;
  /** Text for a human to run. Nothing in the app executes it. */
  apply?: string;
}

/**
 * One thing wrong, or about to be — found by reading config against telemetry.
 *
 * Deterministic rules, not the assistant. Every finding carries the evidence
 * it rests on so the reasoning can be checked rather than trusted.
 */
export interface Finding {
  id: string;
  /**
   * Where this came from — a rule, the assistant, or the assistant restating a
   * rule. Carried on the finding so a verdict cannot be filed against the
   * wrong engine.
   */
  provenance: Provenance;
  /** Which rule produced this, so a verdict attributes to one rule not "the rules". */
  rule: string;
  profileId: string;
  profileName: string;
  severity: Severity;
  title: string;
  detail: string;
  evidence: Evidence[];
  suggestion?: Suggestion;
}

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

/**
 * What the assistant makes of one failing component, for the matrix tooltip.
 *
 * `grounds` is every fact the model was given, in the order it was given them.
 * It travels with the text because this is the one place a model speaks without
 * being asked a question, and an unverifiable sentence in a monitoring tool is
 * worse than none.
 */
export interface ComponentReading {
  text: string;
  model: string;
  grounds: string[];
  readAt: string;
}

/**
 * What the model made of one incident.
 *
 * `grounds` is every fact it was given, in order, and `disconfirm` is what
 * would show the account to be wrong. Both travel with the text because an
 * unverifiable explanation in a monitoring tool is worse than none — and an
 * account nothing could disprove is not an account.
 */
export interface RootCause {
  incidentId: string;
  /** One sentence naming the cause, not the symptom. */
  summary: string;
  /** The model's own confidence, which is not root's. */
  confidence: Confidence;
  /** The causal sequence, earliest first. */
  chain: string[];
  /** Evidence lines it said it relied on. */
  because: string[];
  /** What to check to confirm it, most decisive first. */
  confirm: string[];
  /** What would show this is wrong. Never empty — the backend refuses it. */
  disconfirm: string;
  /** Whether it agrees with root's own grouping about which member is the root. */
  agrees: boolean;
  /** Why not, when it does not. */
  dissent?: string;
  model: string;
  grounds: string[];
  at: string;
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
  /** Traces that spent time in this connection during the window. */
  traces?: LaneTrace[];
}

/**
 * One trace's presence on one lane.
 *
 * A trace is not one service's, which is why it explains anything: the spans of
 * one request land on the lane of whichever connection each service maps to.
 * `selfMs` is the lane's own time, excluding what it waited on downstream —
 * the number that separates a cause from a bystander.
 */
export interface LaneTrace {
  traceId: string;
  rootService: string;
  rootName: string;
  startedAt: string;
  durationMs: number;
  selfMs: number;
  spanCount: number;
  errorCount: number;
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
  /**
   * Where this connection's configuration lives on disk.
   *
   * Declared like `dependsOn` and for the same reason. Live config read back
   * from the target itself needs no entry here — the collector already knows
   * how to ask.
   */
  configSources: ConfigSource[];
  historySource?: HistorySource;
  snapshotSink?: SnapshotSink;
  runtime: Runtime;
  /**
   * Part of the live demo rather than anything real. Set only by starting the
   * demo; such a connection never notifies and never leaves the process.
   */
  simulated?: boolean;
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
  mcp: { enabled: boolean; port: number };
  prediction: { enabled: boolean; intervalMinutes: number };
  team: { serverUrl?: string; account?: Account };
  /** The live demo, while one is running. */
  demo?: DemoSettings;
}

export interface DemoSettings {
  startedAt?: string;
  seed: number;
  /** Real connections switched off for the demo, switched back on when it ends. */
  paused?: string[];
}

/** What the live demo is doing right now. */
export interface DemoStatus {
  running: boolean;
  startedAt?: string;
  /** Simulated connections in the config. */
  connections: number;
  /** Real connections switched off while it runs. */
  paused: number;
  /** The story playing now, if one is. */
  playing?: {
    story: string;
    title: string;
    /** Neutral on purpose — it must not give away what the incident view works out. */
    phase: string;
    startedAt: string;
    endsAt: string;
    manual: boolean;
  };
  next?: { story: string; title: string; at: string };
  stories: { id: string; title: string }[];
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

/**
 * The in-app MCP server. Same shape as {@link IngestStatus}, plus the tool
 * names it would serve — the honest answer to "what does enabling this give
 * away" is a list rather than a sentence.
 */
export interface McpStatus {
  enabled: boolean;
  port: number;
  hasToken: boolean;
  url: string;
  keychainAvailable: boolean;
  tools: string[];
}

/** Forward-looking risk, read by the model. See src-tauri/src/predict.rs. */
export interface PredictionStatus {
  enabled: boolean;
  intervalMinutes: number;
  /** Without a configured assistant this cannot run at all. */
  assistantReady: boolean;
}

export interface PredictionReport {
  findings: Finding[];
  /** Per prediction, in the model's own words, why it said so. */
  because: string[][];
  /** Everything the model was shown this run. */
  grounds: string[];
  model: string;
  at?: string;
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

/**
 * One rectangle in a flame graph.
 *
 * Positions arrive absolute — the wire format delta-encodes them and the
 * backend decodes once, in `profiles.rs`, so nothing here does arithmetic on
 * an offset it did not receive.
 */
export interface Frame {
  name: string;
  /** Row in the graph. Zero is the root spanning everything. */
  depth: number;
  /** Samples from the left edge. */
  offset: number;
  total: number;
  /** Samples in this frame alone. */
  own: number;
  /** Index into `frames` of the frame this one sits inside. */
  parent?: number;
}

/** One symbol, with every call site of it summed. */
export interface HotSymbol {
  name: string;
  own: number;
  /** Share of the whole profile, 0–1. */
  share: number;
  /** How many separate places in the graph carried this name. */
  sites: number;
}

/** A decoded profile, ready to draw. */
export interface Profile {
  frames: Frame[];
  /** Ranked by self samples, worst first. */
  hot: HotSymbol[];
  total: number;
  maxSelf: number;
  /** `samples`, `bytes`, `objects`, `nanoseconds` — or empty when unstated. */
  unit: string;
  sampleRate: number;
  /** What was profiled, as the store names it. */
  name: string;
  spy?: string;
}

// ------------------------------------------------------------ notifications

export type ChannelKind = "desktop" | "slack" | "teams" | "discord" | "pagerduty" | "webhook";

/** One place root can tell, and the rules for what it hears. See `notify.rs`. */
export interface NotifyChannel {
  /** Empty for a channel being created. */
  id: string;
  name: string;
  kind: ChannelKind;
  enabled: boolean;
  /** The least severe alert it hears about. */
  minSeverity: AlertSeverity;
  /** Empty means every environment. */
  environments: Environment[];
  /** Empty means every team. */
  teams: string[];
  /** Minutes an alert must stay unacknowledged first. 0 is at once. */
  delayMinutes: number;
  sendResolved: boolean;
  /** Host and last four characters of the stored URL or key. */
  targetHint?: string | null;
}

export type DeliveryEvent = "firing" | "resolved" | "acknowledged" | "test";

export interface Delivery {
  id: string;
  channelId: string;
  channelName: string;
  kind: ChannelKind;
  event: DeliveryEvent;
  alerts: number;
  summary: string;
  ok: boolean;
  status: number | null;
  error: string | null;
  attempts: number;
  latencyMs: number;
  at: string;
}

export interface NotificationsView {
  channels: NotifyChannel[];
  deliveries: Delivery[];
  keychainAvailable: boolean;
  /** Teams named in ownership metadata. */
  teams: string[];
}

export type NoticeStatus = "sent" | "due" | "retrying" | "held";

/** One alert's standing with one channel. Keyed by alert id in `getAlertNotices`. */
export interface Notice {
  channelId: string;
  channelName: string;
  kind: ChannelKind;
  status: NoticeStatus;
  /** When it was told, when it will be, or when the next attempt is. */
  at: string;
  delayMinutes: number;
}
