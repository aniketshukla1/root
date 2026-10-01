/**
 * Development fixtures.
 *
 * The real app talks to the Rust side over Tauri's `invoke`, which only exists
 * inside the Tauri webview. Opening the Vite server in a plain browser
 * therefore gets you an app that renders nothing but its empty states — which
 * makes iterating on the frontend, or reviewing a screen, need a full Rust
 * build every time.
 *
 * So when `invoke` is absent, `api` falls back to this estate instead. It is
 * the same estate the design mockups use, so a screen can be compared against
 * its design directly. Nothing here ships: the check is `window.__TAURI_INTERNALS__`,
 * which is always present in the packaged app.
 */

import type {
  Alert,
  AlertSeverity,
  ComponentReading,
  FlappingCondition,
  HistoryStats,
  PastAlert,
  PastChange,
  AppConfig,
  Account,
  ChangeEvent,
  Confidence,
  ConnectionProfile,
  Correlation,
  DashboardState,
  GroupingReason,
  Health,
  HealthSnapshot,
  HistogramBucket,
  Incident,
  LogEvent,
  LogLevel,
  MetricSeries,
  Point,
  CapacityRisk,
  ConfigDocument,
  FamilyScore,
  Finding,
  MetricUnit,
  Recall,
  RecordedVerdict,
  Replay,
  RuleScore,
  RuntimeReport,
  SuiteResult,
  Scorecard,
  Verdict,
  ServiceDescriptor,
  TeamState,
  Trend,
  Workspace,
} from "./types";

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

const NOW = Date.now();
const iso = (msAgo: number) => new Date(NOW - msAgo).toISOString();

/** Deterministic wander, so a reload does not reshuffle every chart. */
function seeded(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Poll interval the fixtures pretend to have been collected at. */
const STEP_MS = 15_000;

/**
 * How long a series runs.
 *
 * An hour, because that is the metrics screen's default window and a fixture
 * that fills a quarter of its own chart teaches the wrong thing about what
 * the product looks like with data in it. It also clears the forecaster's
 * fifteen-minute floor — the previous 60 points spanned 885 seconds, fifteen
 * short of it, so every projection on the demo estate was correctly refused
 * and the capacity columns all read "not knowable yet".
 */
const SAMPLES = 240;

/** The step figures below were tuned at this many points. See `series`. */
const SAMPLES_TUNED_AT = 60;

function series(
  n: number,
  seed: number,
  base: number,
  shape: "flat" | "rise" | "fall" | "spike",
): Point[] {
  const r = seeded(seed);
  const out: Point[] = [];
  let v = base;

  // Shapes are defined over the whole window, not per sample, so the drawing
  // is the same at any resolution. The per-step figures were tuned at 60
  // points; scaling drift by 60/n and noise by sqrt(60/n) — a random walk's
  // spread grows with the root of its length — keeps a longer series looking
  // like the same metric rather than a wilder one.
  const drift = SAMPLES_TUNED_AT / n;
  const noise = Math.sqrt(drift);

  for (let i = 0; i < n; i += 1) {
    v += (r() - 0.5) * base * 0.13 * noise;
    if (shape === "rise") v += base * 0.012 * drift;
    if (shape === "fall") v -= base * 0.01 * drift;
    if (shape === "spike" && i > n * 0.72) v += base * 0.06 * drift;
    v = Math.max(base * 0.1, v);
    out.push({ t: iso((n - i) * STEP_MS), v: Number(v.toFixed(2)) });
  }

  // The walk starts at `base` and drifts, which left the LAST point — the one
  // labelled "now" — well away from the value the snapshot reports for now.
  // Memory read 7.4 GB on the card and 14.8 GB in the same metric's series.
  // A history is a history OF the current value, so it is rescaled to land on
  // it; the shape survives, the endpoint stops disagreeing with the estate.
  const last = out[out.length - 1]?.v ?? base;
  if (last > 0 && Math.abs(last - base) > base * 0.001) {
    const factor = base / last;
    for (const point of out) point.v = Number((point.v * factor).toFixed(2));
  }
  return out;
}

/**
 * Every kind the Rust registry publishes.
 *
 * The real catalog comes from the backend, which is the whole point of the
 * descriptor model — but the browser preview has no backend, and an onboarding
 * screen that offered twelve types while the app ships seventeen would be
 * designing against a smaller product than the one that exists.
 */
const METRIC_KINDS = ["docker", "podman", "kubernetes", "linux_host", "nfs",
  "postgres", "mysql", "mongodb", "clickhouse",
  "redis", "memcached",
  "etcd", "consul", "zookeeper",
  "kafka", "rabbitmq", "nats",
  "nginx", "apache", "tomcat", "haproxy", "traefik", "jenkins",
  "elasticsearch", "prometheus", "http_endpoint"];

/**
 * The registry's own labels, for the kinds where capitalising the id gets it
 * wrong. "Mysql" and "Clickhouse" are not what those products are called, and
 * the onboarding screen is the first thing anyone sees.
 */
const KIND_LABELS: Record<string, string> = {
  mysql: "MySQL",
  postgres: "PostgreSQL",
  mongodb: "MongoDB",
  clickhouse: "ClickHouse",
  etcd: "etcd",
  rabbitmq: "RabbitMQ",
  nats: "NATS",
  zookeeper: "ZooKeeper",
  apache: "Apache httpd",
  tomcat: "Apache Tomcat",
  haproxy: "HAProxy",
  http_endpoint: "HTTP endpoint",
  linux_host: "Linux host",
  nfs: "NFS exports",
};

export const DESCRIPTORS: ServiceDescriptor[] = METRIC_KINDS.map((kind, i) => ({
  kind,
  label:
    KIND_LABELS[kind] ??
    kind.replace(/[_-]+/g, " ").replace(/^./, (c) => c.toUpperCase()),
  monogram: kind.slice(0, 2).toUpperCase(),
  accent: i,
  category:
    ["docker", "podman"].includes(kind) ? "Containers"
    : kind === "kubernetes" ? "Orchestration"
    : kind === "linux_host" ? "Hosts"
    : kind === "nfs" ? "Storage"
    : ["postgres", "mysql", "mongodb", "clickhouse"].includes(kind) ? "Databases"
    : ["redis", "memcached"].includes(kind) ? "Caching"
    : ["etcd", "consul", "zookeeper"].includes(kind) ? "Coordination"
    : ["kafka", "rabbitmq", "nats"].includes(kind) ? "Messaging"
    : kind === "jenkins" ? "CI/CD"
    : kind === "elasticsearch" ? "Search & analytics"
    : kind === "prometheus" ? "Observability"
    : "Web & endpoints",
  blurb: "",
  endpoint: { key: "endpoint", label: "Endpoint", kind: "text", required: true, advanced: false },
  fields: [],
  defaultPollSecs: 15,
  capabilities: { logs: true, actions: [], diagnostics: [] },
}));

type Spec = [id: string, name: string, kind: string, health: HealthSnapshot["health"],
  summary: string, team: string, parts: number];

/**
 * Which estate the dev fixtures serve, from `?estate=` in the URL.
 *
 * The home screen has four states and only one of them is reachable from a
 * single hard-coded estate, which means three of them could never be looked at
 * while being designed. This exists so all four can be. It lives entirely in
 * the fixture layer: the packaged app has `__TAURI_INTERNALS__` and never
 * reaches any of this code, so there is no way to put the real product into a
 * state the real estate is not in.
 *
 *   ?estate=empty     nothing configured — the first run
 *   ?estate=quiet     nothing alerting
 *   ?estate=incident  one story, several connections
 *   ?estate=triage    several unrelated incidents (the default)
 *   ?estate=storm     many connections turning at once
 */
export type Scenario = "empty" | "quiet" | "incident" | "triage" | "storm";

export const SCENARIO: Scenario = (() => {
  if (typeof window === "undefined") return "triage";
  const asked = new URLSearchParams(window.location.search).get("estate");
  return asked === "empty" ||
    asked === "quiet" ||
    asked === "incident" ||
    asked === "storm"
    ? asked
    : "triage";
})();

const BASE_ESTATE: Spec[] = [
  ["orders-api", "orders-api", "docker", "down", "3 of 8 containers exited", "payments", 8],
  ["payments-db", "payments-db", "postgres", "degraded", "replication lag 42s on replica-2", "payments", 6],
  ["search-cluster", "search-cluster", "elasticsearch", "degraded", "yellow · 2 unassigned shards", "search", 18],
  ["ci-pipeline", "ci-pipeline", "jenkins", "degraded", "build #4821 failed on main", "platform", 16],
  ["queue-broker", "queue-broker", "rabbitmq", "unknown", "collector unreachable — last seen 4m ago", "platform", 9],
  ["edge-proxy", "edge-proxy", "nginx", "healthy", "12 upstreams healthy · 4.2k req/s", "platform", 12],
  ["checkout-web", "checkout-web", "kubernetes", "healthy", "24 pods ready · 3 nodes", "checkout", 24],
  ["events-bus", "events-bus", "kafka", "healthy", "18 topics · consumer lag 1.2k", "data", 21],
  ["session-cache", "session-cache", "redis", "healthy", "hit rate 99.1% · 2 replicas in sync", "platform", 3],
  ["billing-mongo", "billing-mongo", "mongodb", "healthy", "primary healthy · 2 secondaries", "billing", 3],
  ["metrics-store", "metrics-store", "prometheus", "healthy", "42 targets up · 0 down", "platform", 42],
  ["status-page", "status-page", "http_endpoint", "healthy", "200 in 42ms · body matched", "platform", 1],
];

/** Health overrides per scenario; anything unnamed keeps its base health. */
const SCENARIO_HEALTH: Record<Scenario, Record<string, HealthSnapshot["health"]>> = {
  empty: {},
  quiet: {
    "orders-api": "healthy",
    "payments-db": "healthy",
    "search-cluster": "healthy",
    "ci-pipeline": "healthy",
    "queue-broker": "healthy",
  },
  incident: { "search-cluster": "healthy", "ci-pipeline": "healthy" },
  triage: {},
  storm: {
    "edge-proxy": "down",
    "checkout-web": "down",
    "events-bus": "degraded",
    "session-cache": "down",
    "billing-mongo": "degraded",
  },
};

const SCENARIO_SUMMARY: Record<string, string> = {
  healthy: "reporting normally on its last poll",
};

/**
 * The ids this scenario moved. Everything downstream — health runs, metric
 * values — follows from this one set rather than from a per-scenario table
 * per fixture, so a scenario cannot say "nothing is wrong" in the header while
 * a metric two panels down sits at four times its declared limit.
 */
const OVERRIDDEN = new Set(Object.keys(SCENARIO_HEALTH[SCENARIO]));

const ESTATE: Spec[] = SCENARIO === "empty" ? [] : BASE_ESTATE.map((spec) => {
  const override = SCENARIO_HEALTH[SCENARIO][spec[0]];
  if (!override || override === spec[3]) return spec;
  const next: Spec = [...spec];
  next[3] = override;
  next[4] = SCENARIO_SUMMARY[override] ?? spec[4];
  return next;
});

const METRICS: Record<string, [string, string, number, string, number?][]> = {
  "orders-api": [["cpu", "CPU", 62, "percent", 75], ["memory", "Memory", 7945689498, "bytes", 8589934592],
    ["running", "Running", 5, "count"], ["error_rate", "Error rate", 8.1, "percent", 2]],
  "payments-db": [["lag", "Lag", 42, "seconds", 30], ["conns", "Connections", 318, "count"]],
  "search-cluster": [["heap", "Heap", 81, "percent", 85], ["nodes", "Nodes", 6, "count"]],
  "ci-pipeline": [["duration", "Duration", 14, "seconds"], ["queue", "Queue", 7, "count"]],
  "queue-broker": [],
  "edge-proxy": [["rps", "Req/s", 4200, "per_second"], ["p99", "p99", 38, "millis"]],
  "checkout-web": [["pods", "Pods ready", 24, "count"], ["cpu", "CPU", 34, "percent", 75]],
  "events-bus": [["lag", "Lag", 1200, "count"], ["brokers", "Brokers", 3, "count"]],
  "session-cache": [["hit", "Hit rate", 99.1, "percent"], ["memory", "Memory", 1503238554, "bytes"]],
  "billing-mongo": [["conns", "Connections", 88, "count"]],
  "metrics-store": [["targets", "Targets up", 42, "count"]],
  "status-page": [["latency", "Latency", 42, "millis", 800]],
};

/**
 * What each kind of connection is actually made of. The heatmap groups by
 * connection AND section, so a collector that reports both nodes and indices
 * shows as two blocks — which is the point: "two of six nodes" and "two of
 * ninety indices" are very different sentences.
 */
const INVENTORY: Record<string, [section: string, label: string, count: number][]> = {
  docker: [["containers", "Containers", 8]],
  postgres: [["replicas", "Replicas", 3], ["queries", "Active queries", 6]],
  elasticsearch: [["nodes", "Nodes", 6], ["indices", "Indices", 42], ["shards", "Shards", 96]],
  jenkins: [["workers", "Workers", 16], ["jobs", "Jobs", 34]],
  rabbitmq: [["queues", "Queues", 12]],
  nginx: [["upstreams", "Upstreams", 12]],
  kubernetes: [["pods", "Pods", 64], ["nodes", "Nodes", 6]],
  kafka: [["brokers", "Brokers", 3], ["topics", "Topics", 18]],
  redis: [["replicas", "Replicas", 3]],
  mongodb: [["members", "Members", 3]],
  prometheus: [["targets", "Targets", 42]],
  http_endpoint: [["endpoint", "Endpoint", 1]],
};

function rowsFor(spec: Spec): HealthSnapshot["sections"] {
  const [id, , kind, health] = spec;
  const sections = INVENTORY[kind] ?? [["components", "Components", 6]];
  const r = seeded(id.length * 977 + kind.length);

  return sections.map(([sectionId, label, count], si) => {
    // Only the first section carries the failures, so a degraded connection
    // reads as "its nodes are unhappy" rather than everything at once.
    const failing =
      si > 0 || health === "healthy" || health === "unknown"
        ? 0
        : health === "down"
          ? Math.max(2, Math.round(count * 0.35))
          : Math.max(1, Math.round(count * 0.12));

    const rows = Array.from({ length: count }, (_, i) => {
      const rowHealth: HealthSnapshot["health"] =
        i < failing ? (health === "down" && i < failing / 2 ? "down" : "degraded") : "healthy";
      return {
        id: `${sectionId}-${i + 1}`,
        name: `${label.toLowerCase().replace(/\s+/g, "-")}-${i + 1}`,
        health: rowHealth,
        summary:
          rowHealth === "down"
            ? "exited 137 · OOMKilled"
            : rowHealth === "degraded"
              ? "responding slowly"
              : "running",
        cells: {
          // The primary column reads cells.name, not row.name — without this
          // every component listed as "—" and there was nothing to click.
          name: {
            type: "text" as const,
            value: `${label.toLowerCase().replace(/\s+/g, "-")}-${i + 1}`,
          },
          image: { type: "text" as const, value: `${id}:2.14.3` },
          cpu: { type: "number" as const, value: Math.round(r() * 90) },
          memory: { type: "number" as const, value: Math.round(r() * 8 * 1024 ** 3) },
          restarts: {
            type: "number" as const,
            value: rowHealth === "healthy" ? 0 : Math.ceil(r() * 9),
          },
        },
        actions: [],
      };
    });

    return {
      id: sectionId,
      label,
      priority: si + 1,
      tracked: si === 0,
      emptyText: `No ${label.toLowerCase()} reported`,
      columns: [
        { key: "name", label: "Name", kind: { type: "text" as const }, align: "start" as const, primary: true, sortable: true, secondary: false, revision: false },
        { key: "image", label: "Image", kind: { type: "code" as const }, align: "start" as const, primary: false, sortable: true, secondary: true, revision: true },
        { key: "cpu", label: "CPU", kind: { type: "metric" as const, unit: "percent" as const }, align: "end" as const, primary: false, sortable: true, secondary: false, revision: false },
        { key: "memory", label: "Memory", kind: { type: "metric" as const, unit: "bytes" as const }, align: "end" as const, primary: false, sortable: true, secondary: false, revision: false },
        { key: "restarts", label: "Restarts", kind: { type: "text" as const }, align: "end" as const, primary: false, sortable: true, secondary: true, revision: false },
      ],
      rows,
    };
  });
}

/**
 * A connection the scenario made healthy must not still be reporting a metric
 * over its own threshold — that is the header and the panel disagreeing, which
 * is exactly the kind of quiet lie a monitoring tool cannot afford. Brought to
 * sixty per cent of the limit: comfortably inside, not suspiciously zero.
 */
function scenarioMetrics(id: string): [string, string, number, string, number?][] {
  const base = METRICS[id] ?? [];
  if (SCENARIO_HEALTH[SCENARIO][id] !== "healthy") return base;
  return base.map((metric) => {
    const [key, label, value, unit, warnAbove] = metric;
    if (warnAbove === undefined || value <= warnAbove) return metric;
    return [key, label, Number((warnAbove * 0.6).toFixed(2)), unit, warnAbove];
  });
}

export const SNAPSHOTS: HealthSnapshot[] = ESTATE.map((spec) => {
  const [id, name, kind, health, summary] = spec;
  return {
    profileId: id,
    profileName: name,
    kind,
    health,
    summary,
    metrics: scenarioMetrics(id).map(([key, label, value, unit, warnAbove]) => ({
      key, label, value, unit: unit as never, warnAbove,
    })),
    sections: rowsFor(spec),
    checkedAt: iso(6_000 + id.length * 900),
    latencyMs: 20 + id.length * 3,
    error: health === "unknown" ? "collector unreachable" : undefined,
  };
});

const BASE_ALERTS: Alert[] = [
  { id: "a1", fingerprint: "orders-api/containers", profileId: "orders-api", profileName: "orders-api",
    kind: "docker", severity: "critical", title: "3 containers exited with code 137",
    detail: "orders-worker-3, orders-worker-7, orders-worker-8 · reason=OOMKilled · memory limit 8G",
    component: "Containers", firstSeen: iso(18 * 60_000), lastSeen: iso(4 * 60_000),
    active: true, acknowledged: false },
  { id: "a2", fingerprint: "payments-db/lag", profileId: "payments-db", profileName: "payments-db",
    kind: "postgres", severity: "critical", title: "Replication lag 42s exceeds 30s threshold",
    detail: "streaming replication · primary db-01 · lag rising 1.4s per minute",
    component: "replica-2", firstSeen: iso(16 * 60_000), lastSeen: iso(11 * 60_000),
    active: true, acknowledged: true },
  { id: "a3", fingerprint: "search-cluster/shards", profileId: "search-cluster", profileName: "search-cluster",
    kind: "elasticsearch", severity: "warning", title: "2 shards unassigned for 26 minutes",
    detail: "orders-2026.08 [3] · orders-2026.08 [7] · cluster status yellow",
    component: "Indices", firstSeen: iso(26 * 60_000), lastSeen: iso(26 * 60_000),
    active: true, acknowledged: true },
  { id: "a4", fingerprint: "ci-pipeline/build", profileId: "ci-pipeline", profileName: "ci-pipeline",
    kind: "jenkins", severity: "warning", title: "Build #4821 failed on branch main",
    detail: "orders-api-release · FAILURE in 14m 02s",
    component: "orders-api-release", firstSeen: iso(38 * 60_000), lastSeen: iso(38 * 60_000),
    active: true, acknowledged: false },
  /**
   * A component name long enough to be a layout problem, and not an invented
   * one: Redis trims every `SLOWLOG GET` argument to 128 characters and adds
   * its own `... (N more bytes)`, so one padded write comes back as a
   * ~170-character token with nowhere to break.
   *
   * It lives here because the alerts table used to paint exactly this string
   * straight across the four columns to its right, and nothing in the rest of
   * the fixtures was long enough to show it. A layout that only holds for
   * short values is one that has not been tested.
   */
  { id: "a5", fingerprint: "session-cache/slowlog", profileId: "session-cache", profileName: "session-cache",
    kind: "redis", severity: "warning", title: "Slow command took 118ms",
    detail: "slowlog entry 11607 · above the 50ms threshold",
    component: `SET pad:11607 ${"x".repeat(128)}... (16256 more bytes)`,
    firstSeen: iso(9 * 60_000), lastSeen: iso(2 * 60_000),
    active: true, acknowledged: false },
];

/**
 * A storm: five connections turning inside ninety seconds with nothing
 * declared between them. This is the shape the correlator is most likely to
 * get wrong, so it is the one most worth being able to look at.
 */
const STORM_ALERTS: Alert[] = [
  ["edge-proxy", "nginx", "12 upstreams unreachable", "connect() failed · all backends refused"],
  ["checkout-web", "kubernetes", "18 pods not ready", "readiness probe timeout on 3 nodes"],
  ["session-cache", "redis", "connection refused on 6379", "no route to host"],
  ["events-bus", "kafka", "consumer group rebalancing", "3 brokers unreachable"],
  ["billing-mongo", "mongodb", "replica set has no primary", "election in progress · 2 members visible"],
].map(([profileId, kind, title, detail], i) => ({
  id: `s${i + 1}`,
  fingerprint: `${profileId}/reachability`,
  profileId: profileId as string,
  profileName: profileId as string,
  kind: kind as string,
  severity: (i < 3 ? "critical" : "warning") as Alert["severity"],
  title: title as string,
  detail: detail as string,
  component: undefined,
  // Inside STORM_WINDOW of each other, which is what makes them a storm
  // rather than five things that happened this afternoon.
  firstSeen: iso(180_000 + i * 12_000),
  lastSeen: iso(20_000),
  active: true,
  acknowledged: false,
}));

export const ALERTS: Alert[] = (() => {
  switch (SCENARIO) {
    case "empty":
    case "quiet":
      return [];
    case "incident":
      // Only the orders-api story: its OOM, and the lag it pressures into the
      // database four minutes later.
      return BASE_ALERTS.filter((a) => ["a1", "a2"].includes(a.id));
    case "storm":
      return STORM_ALERTS;
    case "triage":
      return BASE_ALERTS;
  }
})();

export const DASHBOARD: DashboardState = {
  snapshots: SNAPSHOTS,
  alerts: ALERTS,
  // Forecasts travel apart from alerts, so a surface that has not been
  // taught about them cannot show one by accident.
  predictions: [],
  overallHealth: "degraded",
  servicesTotal: SNAPSHOTS.length,
  servicesHealthy: SNAPSHOTS.filter((s) => s.health === "healthy").length,
  servicesDown: SNAPSHOTS.filter((s) => s.health === "down").length,
  generatedAt: iso(0),
};

export function seriesFor(profileId: string): MetricSeries[] {
  // scenarioMetrics, not METRICS: a connection the scenario made healthy must
  // not still be plotting a series that breaches its own threshold.
  return scenarioMetrics(profileId).map(([key, label, value, unit, warnAbove], i) => {
    const shape = key === "memory" || key === "lag" || key === "heap" ? "rise"
      : key === "error_rate" ? "spike" : key === "running" ? "fall" : "flat";
    const points = series(SAMPLES, profileId.length * 31 + i * 7, value, shape);
    const values = points.map((p) => p.v);
    return {
      key, label, unit: unit as never, warnAbove, points,
      changePct: Number((((values[values.length - 1] - values[0]) / (values[0] || 1)) * 100).toFixed(1)),
      min: Math.min(...values), max: Math.max(...values), latest: values[values.length - 1],
      trend: fitTrend(points, warnAbove),
    };
  });
}

const LINES: [string, string, LogLevel, string][] = [
  ["orders-api", "worker-3", "error", "container exited: code=137 reason=OOMKilled restart_count=9"],
  ["orders-api", "worker-1", "error", "POST /v2/orders 502 in 30012ms upstream=orders-worker-3"],
  ["payments-db", "replica-2", "warn", "streaming replication lag 42.3s exceeds 30s"],
  ["orders-api", "worker-3", "error", "connection refused: dial tcp 10.4.2.19:5432"],
  ["orders-api", "daemon", "info", "restarting orders-worker-3 (policy: on-failure)"],
  ["orders-api", "worker-7", "error", "connection pool exhausted after 30s, 0 of 20 free"],
  ["search-cluster", "es-node-4", "warn", "shard [orders-2026.08][3] unassigned, retry 4"],
  ["edge-proxy", "access", "info", "GET /health 200 in 2ms upstream=checkout-web"],
  ["orders-api", "worker-5", "warn", "heap 7.1G of 8G limit, gc pause 412ms"],
  ["session-cache", "replica-1", "debug", "sync full completed, 1.4G in 802ms"],
  ["ci-pipeline", "orders-release", "info", "build #4821 finished: FAILURE in 14m 02s"],
  ["events-bus", "broker-2", "debug", "group orders-consumer rebalanced, 12 partitions"],
];

/**
 * Lines belonging to connections this scenario actually has.
 *
 * The `empty` scenario has no estate, and the non-null assertion here used to
 * throw on the first line rather than yield no logs — a fixture cannot invent
 * a connection to attribute a log to.
 */
export const LOGS: LogEvent[] = Array.from({ length: 240 }, (_, i) => i).flatMap<LogEvent>((i) => {
  const [profileId, source, level, message] = LINES[i % LINES.length];
  const spec = ESTATE.find((e) => e[0] === profileId);
  if (!spec) return [];
  return [
    {
      id: `log-${i}`, profileId, profileName: profileId, kind: spec[2], source, level, message,
      timestamp: iso(i * 4_300), stream: level === "info" ? "event" : "runtime",
    },
  ];
});

export function histogram(buckets: number): HistogramBucket[] {
  const r = seeded(23);
  return Array.from({ length: buckets }, (_, i) => {
    const t = i / (buckets - 1);
    const burst = t > 0.62 && t < 0.8;
    const scale = (0.28 + r() * 0.34) * (burst ? 1.6 : 1) * 60;
    const error = Math.round(scale * (burst ? 0.26 : 0.1));
    const warn = Math.round(scale * (burst ? 0.42 : 0.16));
    const info = Math.round(scale * 0.44);
    const debug = Math.round(scale * 0.3);
    return {
      t: iso((buckets - i) * 15_000), trace: 0, debug, info, warn, error, fatal: 0,
      total: debug + info + warn + error,
      byProfile: { "orders-api": error + warn, "edge-proxy": info },
    };
  });
}

const DEPENDS_ON: Record<string, string[]> = {
  "orders-api": ["payments-db", "session-cache", "events-bus"],
  "checkout-web": ["orders-api", "edge-proxy"],
  "payments-db": ["edge-proxy"],
  "search-cluster": ["events-bus"],
  "ci-pipeline": [],
  "billing-mongo": [],
  "status-page": ["edge-proxy"],
};

export const CONFIG: AppConfig = {
  profiles: ESTATE.map(([id, name, kind, , , team]) => ({
    id, name, kind, endpoint: `${kind}://${id}.internal`,
    hasSecret: true, enabled: true, pollIntervalSecs: 15, options: {},
    dependsOn: DEPENDS_ON[id] ?? [],
    metadata: {
      operations: {
        team,
        owner: team === "platform" ? "Platform rotation" : `${team} on-call`,
        contact: `#${team}-alerts`,
        environment: id === "queue-broker" ? "staging" : "production",
        criticality: ["orders-api", "payments-db", "edge-proxy"].includes(id) ? "tier1" : "tier2",
      },
    },
  })) as unknown as AppConfig["profiles"],
  llm: { provider: "anthropic", model: "claude-opus-5", maxTokens: 4096, temperature: 0.2,
    agentMode: "read_only", hasApiKey: true },
  retention: { logCapacity: 5000 },
  ingest: { enabled: false, port: 7717 },
  mcp: { enabled: false, port: 7718 },
  prediction: { enabled: false, intervalMinutes: 30 },
  // Local by default, as the packaged app is until someone points it at a
  // server. `?team=` overrides it; see TEAM_MODE.
  team: {},
};

/** One observed deploy in the window, so charts have a marker to render. */
/**
 * Typed, so it cannot drift from `ChangeEvent` again — the previous shape here
 * carried `reported: false` where the type wants `source`, and the UI rendered
 * an empty tag rather than failing to compile.
 */
export const CHANGES: ChangeEvent[] = [
  {
    id: "c1",
    profileId: "orders-api",
    profileName: "orders-api",
    kind: "docker",
    change: "deployed",
    source: "observed",
    section: "containers",
    component: "worker",
    from: "2.14.2",
    to: "2.14.3",
    at: iso(22 * 60_000),
    summary: "orders-api worker 2.14.2 → 2.14.3",
  },
  {
    id: "c2",
    profileId: "checkout-web",
    profileName: "checkout-web",
    kind: "kubernetes",
    change: "deployed",
    source: "reported",
    section: "deployments",
    component: "checkout",
    from: "1.9.0",
    to: "1.9.1",
    at: iso(74 * 60_000),
    summary: "checkout-web checkout 1.9.0 → 1.9.1",
  },
  {
    id: "c3",
    profileId: "session-cache",
    profileName: "session-cache",
    kind: "redis",
    change: "appeared",
    source: "observed",
    section: "replicas",
    component: "replica-3",
    to: "present",
    at: iso(96 * 60_000),
    summary: "session-cache replica-3 appeared",
  },
];

/**
 * Health runs matching what the collector ring now produces: observed periods,
 * with a genuine silence on queue-broker rather than a tidy ending.
 */
export function healthRuns(): Record<string, import("./types").HealthRun[]> {
  const windowMs = 60 * 60_000;
  const at = (frac: number) => iso(windowMs - frac * windowMs);
  const runs = (
    spec: [health: string, from: number, to: number][],
  ): import("./types").HealthRun[] =>
    spec.map(([health, from, to]) => ({
      health: health as never,
      from: at(from),
      to: at(to),
    }));

  const scripted: Record<string, import("./types").HealthRun[]> = {
    "orders-api": runs([["healthy", 0, 0.7], ["degraded", 0.7, 0.78], ["down", 0.78, 1]]),
    "payments-db": runs([["healthy", 0, 0.77], ["degraded", 0.77, 1]]),
    "search-cluster": runs([["healthy", 0, 0.57], ["degraded", 0.57, 1]]),
    "ci-pipeline": runs([
      ["healthy", 0, 0.37], ["degraded", 0.37, 0.45],
      ["healthy", 0.45, 0.87], ["degraded", 0.87, 1],
    ]),
    // Reported normally, then stopped. The ring shows the silence.
    "queue-broker": runs([["healthy", 0, 0.93], ["unknown", 0.93, 1]]),
    "edge-proxy": runs([["healthy", 0, 1]]),
    "checkout-web": runs([["healthy", 0, 0.75], ["degraded", 0.75, 0.79], ["healthy", 0.79, 1]]),
    "events-bus": runs([["healthy", 0, 1]]),
    "session-cache": runs([["healthy", 0, 1]]),
    "billing-mongo": runs([["healthy", 0, 1]]),
    "metrics-store": runs([["healthy", 0, 1]]),
    "status-page": runs([["healthy", 0, 1]]),
  };

  // A connection the scenario moved gets runs that agree with where it moved
  // to: healthy for the window, then the new state from about the moment the
  // scenario's alerts begin. Connections the scenario left alone keep their
  // scripted history, including checkout-web's deliberate flap-and-recover.
  for (const id of OVERRIDDEN) {
    const health = SCENARIO_HEALTH[SCENARIO][id];
    scripted[id] =
      health === "healthy"
        ? runs([["healthy", 0, 1]])
        : runs([["healthy", 0, 0.94], [health, 0.94, 1]]);
  }
  return scripted;
}

/** A grounded answer, so the dock can be exercised without a model configured. */
/**
 * A canned reading for the browser build.
 *
 * The packaged app calls a model; this returns what that call would plausibly
 * return for the demo estate, so the tooltip can be designed and reviewed
 * without a key. It is deliberately specific — a lorem-ipsum sentence here
 * would hide exactly the thing worth looking at, which is whether two sentences
 * of generated text earn their place beside the numbers.
 */
export function componentReading(profileId: string, sectionId: string, rowId: string) {
  const snapshot = SNAPSHOTS.find((s) => s.profileId === profileId);
  const section = snapshot?.sections.find((s) => s.id === sectionId);
  const row = section?.rows.find((r) => r.id === rowId);
  const name = row?.name ?? rowId;

  const oom = /137|OOMKilled/i.test(row?.summary ?? "");
  const lag = /lag|replica/i.test(`${section?.label ?? ""} ${row?.summary ?? ""}`);

  const text = oom
    ? "Memory reached the 8G limit four minutes after 2.14.3 rolled out, and the kernel killed it — the restart count says this is the third time. Check whether the new build raised steady-state memory before raising the limit."
    : lag
      ? "Replication has been falling behind since the primary's write rate climbed, so this is back-pressure rather than a fault here. The primary is the thing to look at."
      : `Nothing in the last hour explains ${name} on its own — no deploy, no threshold crossed, and the log lines naming it are routine. Open it and compare against a sibling before treating it as the cause.`;

  return {
    text,
    model: "claude-opus-5 (fixture)",
    grounds: [
      `connection: ${snapshot?.profileName ?? profileId}`,
      `component: ${name} · ${section?.label ?? sectionId}`,
      `health: ${row?.health ?? "unknown"}`,
      ...(row?.summary ? [`collector says: ${row.summary}`] : []),
    ],
    readAt: new Date().toISOString(),
  };
}

export function agentTurn(conversationId: string, message: string) {
  const about137 = /137|OOMKilled/i.test(message);
  return {
    conversationId,
    steps: [
      { tool: "get_snapshot", arguments: {}, ok: true, mutating: false, approved: null,
        result: "orders-api → down · 5 of 8 containers running", durationMs: 180 },
      { tool: "search_logs", arguments: {}, ok: true, mutating: false, approved: null,
        result: "code=137 → 9 lines in 14m, all orders-worker-*", durationMs: 240 },
      { tool: "get_metric_history", arguments: {}, ok: true, mutating: false, approved: null,
        result: "memory peaked 7.98G against an 8G limit", durationMs: 130 },
      { tool: "list_changes", arguments: {}, ok: true, mutating: false, approved: null,
        result: "orders-api 2.14.2 → 2.14.3 at 14:00, observed", durationMs: 90 },
    ],
    usage: { inputTokens: 2140, outputTokens: 380 },
    truncated: false,
    text: about137
      ? [
          "The container did not crash — the kernel killed it. Exit code 137 is 128 + 9, meaning it received SIGKILL, and OOMKilled says why: it asked for more memory than its limit allowed.",
          "",
          "This worker is capped at 8G and peaked at 7.98G before it was killed — the 9th time in 18 minutes. Memory does not fall back between restarts.",
          "",
          "It is not load. Request volume is down 12% over the same window, so this reads as a memory regression in 2.14.3, deployed four minutes before the first restart.",
          "",
          "What to check next:",
          "  1. Compare memory against 2.14.2",
          "  2. Inspect orders-worker-3",
          "  3. Correlate 14:04 across the estate",
        ].join("\n")
      : [
          "One connection is down: orders-api, owned by payments. Three of its eight containers are exiting on code 137 — the kernel OOM killer, not the application.",
          "",
          "Restarts begin at 14:04, four minutes after 2.14.3 was deployed. payments-db is also degraded with 42s replication lag, starting after the first restart storm — most likely downstream of this rather than a second fault.",
        ].join("\n"),
  };
}

/**
 * The graph as the backend resolves it: declared dependencies, derived
 * dependents, and the failing connections that nothing else explains.
 */
export function topology(): import("./types").TopologyView {
  const dependsOn: Record<string, string[]> = {};
  const dependents: Record<string, string[]> = {};
  const ids = new Set(ESTATE.map(([id]) => id));

  for (const [id] of ESTATE) {
    dependsOn[id] = (DEPENDS_ON[id] ?? []).filter((d) => d !== id && ids.has(d));
    dependents[id] ??= [];
  }
  for (const [id, deps] of Object.entries(dependsOn)) {
    for (const dep of deps) (dependents[dep] ??= []).push(id);
  }

  // Onset from the oldest still-firing alert, as the backend does — a
  // dependency only explains a dependent when it failed FIRST. orders-api
  // turns at 14:04 and payments-db at 14:06, so the database does not get
  // credited with a failure that predates it.
  const failing = ESTATE.filter(([, , , health]) => health !== "healthy").map(([id]) => id);
  const onset = new Map<string, number>();
  for (const id of failing) {
    const times = ALERTS.filter((a) => a.active && a.profileId === id).map((a) =>
      Date.parse(a.firstSeen),
    );
    onset.set(id, times.length > 0 ? Math.min(...times) : NOW);
  }
  const TOLERANCE = 30_000;
  const roots = failing.filter(
    (id) =>
      !dependsOn[id].some((dep) => {
        const depAt = onset.get(dep);
        return depAt !== undefined && depAt <= (onset.get(id) ?? NOW) + TOLERANCE;
      }),
  );

  return { dependsOn, dependents, roots: roots.length > 0 ? roots : failing };
}

/**
 * The fixture estate's alerts, correlated the way the backend correlates them.
 *
 * This ports `incidents::correlate` rather than hard-coding an answer, for the
 * same reason `topology()` ports onset arbitration: a hand-written incident
 * list would keep saying whatever it said on the day it was written, however
 * the fixture alerts changed underneath it. The rules are the same three —
 * several conditions on one connection, a declared dependency that failed
 * first, and a simultaneous storm — with the same 30s tolerance and 90s
 * window.
 *
 * On this estate the interesting outcome is a NON-merge: orders-api depends on
 * payments-db, but payments-db turned two minutes later, so the database is
 * not credited with a failure that predates it and the two stay apart.
 */
export function incidents(): Incident[] {
  const ONSET_TOLERANCE = 30_000;
  const PRESSURE_WINDOW = 15 * 60_000;
  const STORM_WINDOW = 90_000;
  const STORM_MIN = 4;

  const active = ALERTS.filter((a) => a.active);
  if (active.length === 0) return [];

  const onset = new Map<string, number>();
  for (const a of active) {
    const at = Date.parse(a.firstSeen);
    onset.set(a.profileId, Math.min(onset.get(a.profileId) ?? at, at));
  }

  const parent = new Map<string, string>([...onset.keys()].map((id) => [id, id]));
  const find = (id: string): string => {
    let cur = id;
    while (parent.get(cur) !== cur) cur = parent.get(cur)!;
    return cur;
  };
  const union = (a: string, b: string) => {
    const [ra, rb] = [find(a), find(b)];
    if (ra !== rb) parent.set(ra, rb);
  };

  const reasons: GroupingReason[] = [];

  for (const id of onset.keys()) {
    if (active.filter((a) => a.profileId === id).length > 1) {
      reasons.push({ kind: "sameConnection", profileId: id });
    }
  }

  for (const [dependent, started] of onset) {
    for (const dep of DEPENDS_ON[dependent] ?? []) {
      const depStarted = onset.get(dep);
      if (depStarted === undefined) continue;
      if (depStarted <= started + ONSET_TOLERANCE) {
        union(dependent, dep);
        reasons.push({ kind: "dependencyFailedFirst", root: dep, dependent });
      } else if (depStarted <= started + PRESSURE_WINDOW) {
        union(dependent, dep);
        reasons.push({ kind: "dependentPressuredIt", origin: dependent, pressured: dep });
      }
    }
  }

  // Storms are counted in GROUPS, not connections: a declared chain that
  // failed for a known reason is one participant, not four coincidences.
  const groupOnset = new Map<string, [string, number]>();
  for (const [id, started] of onset) {
    const root = find(id);
    const slot = groupOnset.get(root);
    if (!slot || started < slot[1]) groupOnset.set(root, [id, started]);
  }

  const byTime = [...groupOnset.values()].sort((a, b) => a[1] - b[1]);
  let storm: string[] = [];
  for (let i = 0; i < byTime.length; i++) {
    const window: string[] = [];
    for (let j = i; j < byTime.length && byTime[j][1] - byTime[i][1] <= STORM_WINDOW; j++) {
      window.push(byTime[j][0]);
    }
    if (window.length > storm.length) storm = window;
  }
  const stormed = storm.length >= STORM_MIN;
  if (stormed) {
    for (const member of storm.slice(1)) union(storm[0], member);
    reasons.push({ kind: "simultaneousOnset", count: storm.length, windowSecs: 90 });
  }

  const groups = new Map<string, string[]>();
  for (const id of onset.keys()) {
    const root = find(id);
    groups.set(root, [...(groups.get(root) ?? []), id].sort());
  }

  const rank = (s: AlertSeverity) => (s === "critical" ? 0 : s === "warning" ? 1 : 2);
  const worst = (a: Health, b: Health) =>
    (["unknown", "healthy", "degraded", "down"].indexOf(a) >
    ["unknown", "healthy", "degraded", "down"].indexOf(b)
      ? a
      : b);

  const out: Incident[] = [...groups.values()].map((members) => {
    const mine = active.filter((a) => members.includes(a.profileId));
    const severity = mine.map((a) => a.severity).sort((a, b) => rank(a) - rank(b))[0] ?? "warning";
    const health = members
      .map((id) => SNAPSHOTS.find((s) => s.profileId === id)?.health ?? "degraded")
      .reduce(worst, "unknown" as Health);
    const started = Math.min(...mine.map((a) => Date.parse(a.firstSeen)));

    let roots = members.filter(
      (id) =>
        !(DEPENDS_ON[id] ?? []).some((dep) => {
          const depAt = onset.get(dep);
          return (
            members.includes(dep) && depAt !== undefined && depAt <= (onset.get(id) ?? started) + ONSET_TOLERANCE
          );
        }),
    );

    const grouping = reasons.filter((r) =>
      r.kind === "sameConnection"
        ? members.includes(r.profileId)
        : r.kind === "dependencyFailedFirst"
          ? members.includes(r.root) && members.includes(r.dependent)
          : r.kind === "dependentPressuredIt"
            ? members.includes(r.origin) && members.includes(r.pressured)
            : stormed && members.length > 1,
    );

    // Reading an edge backwards leaves both ends looking like origins, so when
    // declared edges are what bind the incident the clock settles it.
    const boundByEdges = grouping.some(
      (r) => r.kind === "dependencyFailedFirst" || r.kind === "dependentPressuredIt",
    );
    if (boundByEdges && roots.length > 1) {
      const first = Math.min(...roots.map((id) => onset.get(id) ?? started));
      roots = roots.filter((id) => (onset.get(id) ?? started) <= first + ONSET_TOLERANCE);
    }
    const onlyPressure =
      boundByEdges && !grouping.some((r) => r.kind === "dependencyFailedFirst");

    // Partly, not only: a chain swept into a storm has members nothing
    // explains, so the whole grouping is what needs checking.
    const heldPartlyByTiming =
      members.length > 1 && grouping.some((r) => r.kind === "simultaneousOnset");
    const confidence: Confidence = heldPartlyByTiming
      ? "low"
      : members.length === 1
        ? "high"
        : onlyPressure
          ? "medium"
          : roots.length === 1
            ? "high"
            : "medium";

    const lead =
      mine
        .filter((a) => roots.includes(a.profileId))
        .sort((a, b) => rank(a.severity) - rank(b.severity) || Date.parse(a.firstSeen) - Date.parse(b.firstSeen))[0] ??
      mine.sort((a, b) => Date.parse(a.firstSeen) - Date.parse(b.firstSeen))[0];

    return {
      id: `inc:${lead.fingerprint}`,
      leadProfileId: lead.profileId,
      title: members.length > 1 ? `${lead.profileName} — ${lead.title}` : lead.title,
      severity,
      health,
      started: new Date(started).toISOString(),
      lastSeen: new Date(Math.max(...mine.map((a) => Date.parse(a.lastSeen)))).toISOString(),
      profileIds: [...members].sort(
        (a, b) => (onset.get(a) ?? started) - (onset.get(b) ?? started),
      ),
      alertIds: mine.map((a) => a.id),
      roots,
      acknowledged: mine.every((a) => a.acknowledged),
      confidence,
      grouping,
    };
  });

  return out.sort(
    (a, b) =>
      Number(a.acknowledged) - Number(b.acknowledged) ||
      rank(a.severity) - rank(b.severity) ||
      b.profileIds.length - a.profileIds.length ||
      Date.parse(a.started) - Date.parse(b.started),
  );
}

/**
 * What every connection was doing around one moment.
 *
 * The previous fixture returned `{ at, lanes: [] }` — not a `Correlation` at
 * all, and untyped, so the Correlate screen read `data.alerts.length` on
 * undefined and the app went white. It is typed now, for the same reason
 * `CHANGES` is: a fixture that lies about its shape does not fail loudly, it
 * fails on whichever screen happens to read the missing field.
 *
 * Lanes carry only connections with something to say in the window. A
 * connection that was quiet is not a lane with zeroes in it — it is absent,
 * and the screen reports how many were absent rather than padding the list.
 */
export function correlation(at: string, windowSecs = 120): Correlation {
  const center = Date.parse(at);
  const half = (windowSecs * 1000) / 2;
  const from = center - half;
  const to = center + half;

  const lanes = SNAPSHOTS.map((snapshot) => {
    const events = LOGS.filter(
      (log) =>
        log.profileId === snapshot.profileId &&
        Date.parse(log.timestamp) >= from &&
        Date.parse(log.timestamp) <= to,
    );

    // Only metrics that actually moved. A movement of nought per cent is not
    // evidence, and listing it would bury the ones that are.
    const movements = seriesFor(snapshot.profileId)
      .filter((s) => s.changePct !== undefined && Math.abs(s.changePct) >= 5)
      .map((s) => ({
        key: s.key,
        label: s.label,
        unit: s.unit,
        from: s.points[0]?.v ?? s.latest,
        to: s.latest,
        changePct: s.changePct ?? 0,
      }))
      .sort((a, b) => Math.abs(b.changePct) - Math.abs(a.changePct));

    return {
      profileId: snapshot.profileId,
      profileName: snapshot.profileName,
      kind: snapshot.kind,
      total: events.length,
      errorCount: events.filter((e) => e.level === "error" || e.level === "fatal").length,
      events: events.slice(0, 12),
      movements: movements.slice(0, 4),
    };
  }).filter((lane) => lane.total > 0 || lane.movements.length > 0);

  return {
    center: new Date(center).toISOString(),
    from: new Date(from).toISOString(),
    to: new Date(to).toISOString(),
    lanes,
    alerts: ALERTS.filter((a) => Date.parse(a.firstSeen) <= to),
  };
}

/**
 * Saving and deleting, in the browser.
 *
 * The fixture layer is otherwise a pure read model, and this is the one place
 * it has to hold state: without it the first-run flow ends at a Save button
 * that rejects, which is the one step of onboarding most worth being able to
 * try. Session-scoped and thrown away on reload — nothing here reaches disk,
 * and the packaged app never takes this path.
 */
const SAVED: ConnectionProfile[] = [...CONFIG.profiles];

export function saveProfile(profile: ConnectionProfile): AppConfig {
  const at = SAVED.findIndex((p) => p.id === profile.id);
  if (at >= 0) SAVED[at] = profile;
  else SAVED.push(profile);
  return { ...CONFIG, profiles: [...SAVED] };
}

export function deleteProfile(profileId: string): AppConfig {
  const at = SAVED.findIndex((p) => p.id === profileId);
  if (at >= 0) SAVED.splice(at, 1);
  return { ...CONFIG, profiles: [...SAVED] };
}

export function currentConfig(): AppConfig {
  return { ...CONFIG, profiles: [...SAVED] };
}

// ------------------------------------------------------------------- team

/**
 * A workspace, in the browser.
 *
 * `?team=1` starts signed in so the Team screen can be looked at; `?team=out`
 * starts with a server configured but no session, which is the only way to
 * reach the sign-in screen without a server to sign in to. Without either,
 * root is in local mode, which is what it is by default and what the packaged
 * app does until someone points it at a server.
 */
const TEAM_MODE = (() => {
  if (typeof window === "undefined") return "local";
  const asked = new URLSearchParams(window.location.search).get("team");
  return asked === "1" ? "in" : asked === "out" ? "out" : "local";
})();

const DEMO_SERVER = "https://root.acme.internal";

const DEMO_ACCOUNT: Account = {
  email: "aniket@acme.io",
  name: "Aniket Shukla",
  role: "owner",
};

let teamState: TeamState =
  TEAM_MODE === "in"
    ? { serverUrl: DEMO_SERVER, account: DEMO_ACCOUNT, hasSession: true }
    : TEAM_MODE === "out"
      ? { serverUrl: DEMO_SERVER, hasSession: false }
      : { hasSession: false };

export function team(): TeamState {
  return { ...teamState };
}

export function saveTeamServer(serverUrl: string | null): TeamState {
  const trimmed = serverUrl?.trim();
  teamState = trimmed
    ? { ...teamState, serverUrl: trimmed.includes("://") ? trimmed : `https://${trimmed}` }
    : { hasSession: false };
  return team();
}

export function signIn(email: string): TeamState {
  teamState = {
    ...teamState,
    hasSession: true,
    account: { ...DEMO_ACCOUNT, email, name: email.split("@")[0] },
  };
  return team();
}

export function signOut(): TeamState {
  teamState = { serverUrl: teamState.serverUrl, hasSession: false };
  return team();
}

export function workspace(): Workspace {
  return {
    sso: { domain: "acme.io", defaultRole: "viewer", enforced: true },
    members: [
      { email: "aniket@acme.io", name: "Aniket Shukla", role: "owner", lastSeen: iso(2 * 60_000), viaSso: false },
      { email: "priya@acme.io", name: "Priya Raman", role: "admin", lastSeen: iso(26 * 60_000), viaSso: true },
      { email: "tom@acme.io", name: "Tom Byrne", role: "responder", lastSeen: iso(3 * 60 * 60_000), viaSso: true },
      { email: "dana@acme.io", name: "Dana Okoro", role: "responder", lastSeen: iso(9 * 60 * 60_000), viaSso: true },
      { email: "sam@acme.io", name: "Sam Idris", role: "viewer", lastSeen: iso(2 * 24 * 60 * 60_000), viaSso: true },
      { email: "new.hire@acme.io", name: "Invited", role: "viewer", viaSso: false },
    ],
  };
}

/**
 * The same least-squares fit the backend does, over the fixture's own points.
 *
 * Ported rather than hard-coded, for the reason `incidents()` is: a fabricated
 * "fills in 6 days" would keep saying six days however the generated series
 * changed underneath it, and the capacity view would be designed against a
 * number that means nothing. The gates match forecast.rs — eight samples,
 * fifteen minutes, R² of 0.25 — so the browser refuses to project in exactly
 * the cases the product does.
 */
function fitTrend(points: Point[], warnAbove?: number): Trend | undefined {
  if (points.length < 8) return undefined;
  const t0 = Date.parse(points[0].t);
  const windowSecs = (Date.parse(points[points.length - 1].t) - t0) / 1000;
  if (windowSecs < 900) return undefined;

  const xs = points.map((p) => (Date.parse(p.t) - t0) / 1000);
  const ys = points.map((p) => p.v);
  const n = xs.length;
  const meanX = xs.reduce((a, b) => a + b, 0) / n;
  const meanY = ys.reduce((a, b) => a + b, 0) / n;

  let sxy = 0;
  let sxx = 0;
  for (let i = 0; i < n; i += 1) {
    sxy += (xs[i] - meanX) * (ys[i] - meanY);
    sxx += (xs[i] - meanX) ** 2;
  }
  if (sxx <= Number.EPSILON) return undefined;

  const perSecond = sxy / sxx;
  const intercept = meanY - perSecond * meanX;
  let ssRes = 0;
  let ssTot = 0;
  for (let i = 0; i < n; i += 1) {
    ssRes += (ys[i] - (intercept + perSecond * xs[i])) ** 2;
    ssTot += (ys[i] - meanY) ** 2;
  }
  const fit = ssTot <= Number.EPSILON ? 1 : Math.min(1, Math.max(0, 1 - ssRes / ssTot));

  const current = ys[ys.length - 1];
  let secondsToLimit: number | undefined;
  if (warnAbove !== undefined) {
    // Already past it is a reading of the present, not a projection, so it
    // does not wait on the fit — same ordering as seconds_to_ceiling.
    if (current >= warnAbove) secondsToLimit = 0;
    else if (fit >= 0.25 && perSecond > 0) secondsToLimit = (warnAbove - current) / perSecond;
  }

  return { perSecond, fit, windowSecs, samples: n, secondsToLimit };
}

/**
 * The estate's capacity ranking, over the fixture's own series.
 *
 * Mirrors capacity_risks() in history.rs: everything with a declared limit,
 * soonest to reach it first, fill only as the tiebreak for rows with no
 * projection. Component-level rows are absent here because the fixture has no
 * per-entity history to fit — that gap is the fixture's, not the product's.
 */
export function capacityRisks(take = 8): CapacityRisk[] {
  const out: CapacityRisk[] = [];
  for (const snapshot of SNAPSHOTS) {
    const series = seriesFor(snapshot.profileId);
    for (const metric of snapshot.metrics) {
      if (metric.warnAbove === undefined || metric.warnAbove === null) continue;
      if (metric.warnAbove <= 0) continue;
      out.push({
        profileId: snapshot.profileId,
        key: metric.key,
        label: metric.label,
        unit: metric.unit,
        value: metric.value,
        limit: metric.warnAbove,
        used: metric.value / metric.warnAbove,
        trend: series.find((s) => s.key === metric.key)?.trend,
      });
    }
  }

  return out
    .sort((a, b) => {
      const at = a.trend?.secondsToLimit;
      const bt = b.trend?.secondsToLimit;
      if (at !== undefined && bt !== undefined) return at - bt;
      if (at !== undefined) return -1;
      if (bt !== undefined) return 1;
      return b.used - a.used;
    })
    .slice(0, take);
}

/**
 * History for one component, the way a tracked section produces it.
 *
 * The real store records the numeric cells of any section a collector marked
 * `tracked` — that is how an NFS export's fill, a container's memory or one
 * node's heap get a series of their own, and it is where the capacity answer
 * usually lives, because "which export fills first" is not a question the
 * connection-level roll-up can answer.
 *
 * Limits come from the column's gauge threshold in the product. The fixture
 * declares them here for the same two columns the estate declares them on, so
 * the component screen shows what it will show with real data behind it.
 */
export function componentSeriesFor(
  profileId: string,
  sectionId: string,
  rowId: string,
): MetricSeries[] {
  // Read from SNAPSHOTS, which is what the screen is rendering. Rebuilding
  // the rows from the estate spec looked equivalent and was not: the row
  // generator draws restarts from a seeded stream only for unhealthy rows, so
  // a scenario that heals one connection shifts every value after it. The
  // component page showed 63% in its current-state panel and a series topping
  // out at 54% — a history of a number the component never held.
  const section = SNAPSHOTS.find((s) => s.profileId === profileId)?.sections.find(
    (s) => s.id === sectionId,
  );
  const row = section?.rows.find((r) => r.id === rowId);
  if (!section?.tracked || !row) return [];

  const columns: [string, string, MetricUnit, number | undefined][] = [
    ["cpu", "CPU", "percent", 85],
    ["memory", "Memory", "bytes", 8 * 1024 ** 3],
    ["restarts", "Restarts", "count", undefined],
  ];

  const seed = rowId.length * 613 + profileId.length * 17;
  return columns.flatMap(([key, label, unit, warnAbove], i) => {
    const cell = row.cells[key];
    if (!cell || cell.type !== "number") return [];
    const shape = key === "memory" ? "rise" : key === "restarts" ? "flat" : "spike";
    const points = series(SAMPLES, seed + i * 29, cell.value, shape);
    const values = points.map((p) => p.v);
    return [
      {
        key,
        label,
        unit,
        warnAbove,
        points,
        changePct: Number(
          (((values[values.length - 1] - values[0]) / (values[0] || 1)) * 100).toFixed(1),
        ),
        min: Math.min(...values),
        max: Math.max(...values),
        latest: values[values.length - 1],
        trend: fitTrend(points, warnAbove),
      },
    ];
  });
}

// ------------------------------------------------------------------- config

/**
 * What the config layer returns, including the parts it refuses to return.
 *
 * The withheld cases are here on purpose. A demo estate where every document
 * opens cleanly teaches the wrong thing about a tool whose most important
 * behaviour is declining to read something.
 */
const TARGET_CONFIG: Record<string, ConfigDocument[]> = {
  "session-cache": [
    {
      id: "cfg-redis-live",
      profileId: "session-cache",
      label: "Running configuration",
      origin: { kind: "live", via: "CONFIG GET *" },
      format: "json",
      bytes: 412,
      body: JSON.stringify(
        {
          maxmemory: "2147483648",
          "maxmemory-policy": "noeviction",
          appendonly: "yes",
          save: "3600 1 300 100 60 10000",
          timeout: "0",
          "tcp-keepalive": "300",
          requirepass: "«redacted»",
        },
        null,
        2,
      ),
      redactions: [{ line: 0, key: "requirepass", reason: "keyName" }],
      readAt: iso(0),
    },
    {
      id: "cfg-redis-file",
      profileId: "session-cache",
      label: "redis.conf",
      origin: { kind: "file", path: "/etc/redis/redis.conf" },
      format: "ini",
      bytes: 1184,
      modified: iso(86_400_000 * 9),
      // Deliberately disagrees with the live value above: the file says 4gb,
      // the running process says 2gb. Somebody edited and never reloaded.
      body: `# Managed by ops. Reload after editing.
maxmemory 4gb
maxmemory-policy allkeys-lru
appendonly yes
requirepass «redacted»
bind 0.0.0.0
protected-mode yes`,
      redactions: [{ line: 5, key: "requirepass", reason: "keyName" }],
      readAt: iso(0),
    },
    {
      id: "cfg-redis-env",
      profileId: "session-cache",
      label: ".env",
      origin: { kind: "file", path: "/etc/redis/.env" },
      format: "text",
      bytes: 208,
      modified: iso(86_400_000 * 30),
      withheld: { kind: "secret" },
      readAt: iso(0),
    },
  ],
  "orders-api": [
    {
      id: "cfg-docker-live",
      profileId: "orders-api",
      label: "Container limits",
      origin: { kind: "live", via: "inspect (HostConfig)" },
      format: "json",
      bytes: 620,
      body: JSON.stringify(
        {
          "orders-worker-1": { memoryLimitBytes: 2147483648, restartPolicy: "UnlessStopped" },
          "orders-worker-3": { memoryLimitBytes: 0, restartPolicy: "UnlessStopped" },
          "orders-api": { memoryLimitBytes: 0, restartPolicy: "UnlessStopped" },
        },
        null,
        2,
      ),
      readAt: iso(0),
    },
    {
      id: "cfg-compose",
      profileId: "orders-api",
      label: "docker-compose.yml",
      origin: { kind: "file", path: "/srv/orders/docker-compose.yml" },
      format: "yaml",
      bytes: 964,
      modified: iso(86_400_000 * 2),
      body: `services:
  api:
    image: orders-api:2.14.3
    environment:
      DATABASE_URL: postgres://orders:«redacted credentials»@payments-db:5432/orders
      REDIS_URL: redis://session-cache:6379
      API_TOKEN: \${ORDERS_API_TOKEN}
    restart: unless-stopped`,
      redactions: [{ line: 5, reason: "urlCredentials" }],
      readAt: iso(0),
    },
  ],
  "payments-db": [
    {
      id: "cfg-pg-live",
      profileId: "payments-db",
      label: "Running parameters",
      origin: { kind: "live", via: "pg_settings (non-default)" },
      format: "json",
      bytes: 380,
      body: JSON.stringify(
        {
          max_connections: { value: "100", source: "configuration file" },
          shared_buffers: { value: "2097152kB", source: "configuration file" },
          work_mem: { value: "4096kB", source: "configuration file" },
          password_encryption: { value: "scram-sha-256", source: "configuration file" },
        },
        null,
        2,
      ),
      readAt: iso(0),
    },
  ],
};

export function configDocuments(profileId: string): ConfigDocument[] {
  return TARGET_CONFIG[profileId] ?? [];
}

/**
 * The findings the Rust rules produce for the estate above.
 *
 * Written out rather than reimplemented: the rules live in advisor.rs and
 * porting them here would give two versions to keep honest. These match what
 * that module returns for this config and these metrics.
 */
const FINDINGS: Finding[] = [
  {
    id: "session-cache:config-drift",
    provenance: { rule: { id: "caching.config-drift" } },
    rule: "caching.config-drift",
    profileId: "session-cache",
    profileName: "session-cache",
    severity: "soon",
    title: "2 settings on disk are not what is running",
    detail:
      "maxmemory, maxmemory-policy differ between the file and the running process. Usually " +
      "this means an edit that nothing reloaded — the change is correct and has never taken " +
      "effect, and anyone reading the file is reasoning about a service that does not match " +
      "it. It also means a restart will adopt the file's values without anyone deciding to, " +
      "so this is a change already scheduled for the next time this process comes up.",
    evidence: [
      { kind: "setting", key: "maxmemory", value: "4gb", source: "on disk" },
      { kind: "setting", key: "maxmemory", value: "2147483648", source: "running" },
      { kind: "setting", key: "maxmemory-policy", value: "allkeys-lru", source: "on disk" },
      { kind: "setting", key: "maxmemory-policy", value: "noeviction", source: "running" },
    ],
    suggestion: {
      setting: "reload",
      current: "file and process disagree",
      proposed: "reload to adopt the file, or correct the file to match the process",
    },
  },
  {
    id: "session-cache:maxmemory-noeviction",
    provenance: { rule: { id: "caching.maxmemory-noeviction" } },
    rule: "caching.maxmemory-noeviction",
    profileId: "session-cache",
    profileName: "session-cache",
    severity: "now",
    title: "Writes will start failing in about 3 hours",
    detail:
      "Memory is at 1.8 GB of a 2.0 GB ceiling and rising. `maxmemory-policy` is " +
      "`noeviction`, so when it arrives Redis does not evict anything — it returns OOM on " +
      "every write and keeps serving reads. Applications that treat Redis as a cache " +
      "usually survive that badly, because a failed write looks like a bug rather than a " +
      "full cache.",
    evidence: [
      { kind: "metric", key: "memory", label: "Memory", value: 1_932_735_283, unit: "bytes", limit: 2_147_483_648 },
      { kind: "setting", key: "maxmemory", value: "2147483648", source: "CONFIG GET *" },
      { kind: "setting", key: "maxmemory-policy", value: "noeviction", source: "CONFIG GET *" },
      { kind: "projection", key: "memory", secondsToLimit: 10_800, fit: 0.91, windowSecs: 3585 },
    ],
    suggestion: {
      setting: "maxmemory-policy",
      current: "noeviction",
      proposed: "allkeys-lru, if this instance is a cache",
      apply:
        "redis-cli CONFIG SET maxmemory-policy allkeys-lru   # cache semantics: evict rather than refuse\n" +
        "redis-cli CONFIG SET maxmemory <larger>            # or raise the ceiling, if the data is not disposable",
    },
  },
  {
    // The category the whole measurement exists to grow: something no rule
    // covers, reported by the assistant, waiting to be confirmed or dismissed.
    // Confirmed, it becomes the next rule; dismissed, it is why the model's
    // column reads differently from the rules'.
    id: "payments-db:model:replication-lag-is-climbing-on",
    provenance: { model: { model: "claude-opus-5" } },
    rule: "model",
    profileId: "payments-db",
    profileName: "payments-db",
    severity: "soon",
    title: "Replication lag is climbing on replica-2 after the 03:00 batch",
    detail:
      "Lag on replica-2 has risen at the same time on each of the last four nights and has " +
      "not fully recovered before the next run, so the floor is drifting up. No rule covers " +
      "this — there is no declared threshold on replication lag for this connection — but the " +
      "pattern is consistent enough to be worth a look before it becomes a failover that " +
      "cannot complete.",
    evidence: [
      {
        kind: "setting",
        key: "observed",
        value: "lag peaked at 42s, 47s, 51s and 58s on the last four nights, each within 5 minutes of 03:00",
        source: "assistant",
      },
      {
        kind: "setting",
        key: "observed",
        value: "the overnight floor rose from 1.2s to 6.4s across the same window",
        source: "assistant",
      },
    ],
    suggestion: {
      setting: "replication lag threshold",
      current: "none declared",
      proposed: "warn above 30s, so the drift is visible without reading four nights of charts",
    },
  },
  {
    id: "orders-api:container-no-memory-limit",
    provenance: { rule: { id: "docker.container-no-memory-limit" } },
    rule: "docker.container-no-memory-limit",
    profileId: "orders-api",
    profileName: "orders-api",
    severity: "advisory",
    title: "2 containers can use all the host's memory",
    detail:
      "orders-worker-3, orders-api run with no memory limit. A leak in any one of them is " +
      "a host-wide event rather than a container-wide one: the kernel's OOM killer picks " +
      "by score, and it regularly picks a healthy neighbour over the process that is " +
      "actually growing. A limit turns that into a restart of the thing at fault.",
    evidence: [
      { kind: "setting", key: "HostConfig.Memory", value: "0 (unlimited)", source: "inspect (HostConfig)" },
    ],
    suggestion: {
      setting: "memory limit",
      current: "unlimited",
      proposed: "a ceiling per container",
      apply: "docker update --memory 2g --memory-swap 2g <container>   # or set mem_limit in the compose file",
    },
  },
];

/** Severity-ordered, the way advisor.rs returns them. */
const SEVERITY_RANK: Record<Finding["severity"], number> = { now: 3, soon: 2, advisory: 1 };

export function findings(profileId?: string): Finding[] {
  return FINDINGS.filter((f) => !profileId || f.profileId === profileId).sort(
    (a, b) => SEVERITY_RANK[b.severity] - SEVERITY_RANK[a.severity],
  );
}

// ------------------------------------------------------- the judgement record

/**
 * An in-memory stand-in for journal.db.
 *
 * Append-only like the real thing, so the browser exercises the same
 * behaviour: rating something twice leaves both rows and only the latest one
 * counts. It does NOT persist — that is the whole difference between the
 * fixture and the product, and pretending otherwise in a preview would hide
 * the one property the real store exists to provide.
 */
const VERDICT_LOG: RecordedVerdict[] = [];
const RECALL_LOG: Recall[] = [];

export function recordVerdict(verdict: Verdict): number {
  const id = VERDICT_LOG.length + 1;
  const provenance =
    "rule" in verdict.provenance
      ? `rule:${verdict.provenance.rule.id}`
      : "model" in verdict.provenance
        ? "model"
        : `model+rule:${verdict.provenance.modelCitingRule.rule}`;
  const family = provenance.startsWith("rule:")
    ? "rule"
    : provenance === "model"
      ? "model"
      : "model+rule";

  VERDICT_LOG.push({
    ...verdict,
    id,
    at: iso(0),
    provenance,
    family,
    fingerprint: String(id).padStart(16, "0"),
    rulesVersion: "1",
  });
  return id;
}

export function recordRecall(recall: Recall): number {
  RECALL_LOG.push(recall);
  return RECALL_LOG.length;
}

/** Latest per finding, matching the store's own view. */
export function verdicts(): RecordedVerdict[] {
  const latest = new Map<string, RecordedVerdict>();
  for (const v of VERDICT_LOG) latest.set(v.findingId, v);
  return [...latest.values()].reverse();
}

export function judgedIncidents(): string[] {
  return [...new Set(RECALL_LOG.map((r) => r.incidentId))];
}

export function scorecard(): Scorecard {
  const latest = verdicts();
  const families = new Map<string, FamilyScore>();

  for (const v of latest) {
    const score =
      families.get(v.family) ??
      {
        family: v.family,
        judged: 0,
        real: 0,
        notAProblem: 0,
        alreadyKnew: 0,
        unsure: 0,
        fixRight: 0,
        fixWrong: 0,
        fixPartly: 0,
      };
    score.judged += 1;
    if (v.problem === "real") score.real += 1;
    if (v.problem === "notAProblem") score.notAProblem += 1;
    if (v.problem === "alreadyKnew") score.alreadyKnew += 1;
    if (v.problem === "unsure") score.unsure += 1;
    if (v.fix === "right") score.fixRight += 1;
    if (v.fix === "wrong") score.fixWrong += 1;
    if (v.fix === "partlyRight") score.fixPartly += 1;
    families.set(v.family, score);
  }

  const rules = new Map<string, RuleScore>();
  for (const v of latest.filter((x) => x.family === "rule")) {
    const name = v.provenance.replace(/^rule:/, "");
    const score = rules.get(name) ?? { rule: name, judged: 0, real: 0, notAProblem: 0 };
    score.judged += 1;
    if (v.problem === "real") score.real += 1;
    if (v.problem === "notAProblem") score.notAProblem += 1;
    rules.set(name, score);
  }

  const byIncident = new Map<string, Recall>();
  for (const r of RECALL_LOG) byIncident.set(r.incidentId, r);
  const recalls = [...byIncident.values()];

  return {
    byFamily: [...families.values()].sort((a, b) => a.family.localeCompare(b.family)),
    byRule: [...rules.values()].sort((a, b) => b.notAProblem - a.notAProblem || b.judged - a.judged),
    recall: {
      incidents: recalls.length,
      called: recalls.filter((r) => r.called === "yes").length,
      missed: recalls.filter((r) => r.called === "no").length,
      partly: recalls.filter((r) => r.called === "partly").length,
    },
    rulesVersion: "1",
  };
}

// ------------------------------------------------------------------ backtest

/**
 * A replay result shaped like the real one.
 *
 * Written out rather than simulated: reimplementing the sliding-window replay
 * in the browser would give two versions of the machinery to keep honest, and
 * the interesting part — what it looks like when a rule turns out to have had
 * seventeen hours of lead time on something nobody was warned about — is a
 * shape, not an algorithm.
 *
 * The numbers are the ones a live Prometheus actually produced for a week of
 * a monotonically climbing series.
 */
export function backtest(profileId: string): Replay {
  const end = Date.now();
  const at = (hoursAgo: number) => new Date(end - hoursAgo * 3_600_000).toISOString();
  const name = SNAPSHOTS.find((s) => s.profileId === profileId)?.profileName ?? profileId;

  if (profileId !== "session-cache") {
    // Most connections declare no long-range source, and the honest result is
    // that there was nothing to replay — not a clean bill of health.
    return {
      profileId,
      profileName: name,
      from: at(0),
      to: at(0),
      evaluations: 0,
      samples: 0,
      episodes: [],
      behaviour: [],
      coverage: [],
      caveats: [{ kind: "configIsCurrent" }, { kind: "noHistorySource" }],
    };
  }

  return {
    profileId,
    profileName: name,
    from: at(168),
    to: at(0),
    evaluations: 162,
    samples: 6709,
    episodes: [
      {
        rule: "caching.maxmemory-noeviction",
        title: "Writes will start failing in about 12 hours",
        severity: "soon",
        from: at(41),
        to: at(37),
        evaluations: 5,
      },
      {
        rule: "caching.maxmemory-noeviction-near",
        title: "Close to the ceiling, and configured to refuse writes",
        severity: "soon",
        from: at(29),
        to: at(29),
        evaluations: 1,
      },
      {
        rule: "caching.maxmemory-noeviction",
        title: "Writes will start failing in about 3 hours",
        severity: "now",
        from: at(28),
        to: at(24),
        evaluations: 5,
      },
    ],
    behaviour: [
      {
        rule: "caching.maxmemory-noeviction",
        episodes: 2,
        firingSecs: 8 * 3600,
        dutyCycle: 0.048,
        worst: "now",
      },
      {
        rule: "caching.maxmemory-noeviction-near",
        episodes: 1,
        firingSecs: 0,
        dutyCycle: 0,
        worst: "soon",
      },
    ],
    coverage: [
      {
        incidentId: "i-past-1",
        headline: "session-cache stopped accepting writes",
        startedAt: at(24),
        recorded: "no",
        rule: "caching.maxmemory-noeviction",
        leadSecs: 17 * 3600,
        newlyCaught: true,
      },
    ],
    caveats: [{ kind: "configIsCurrent" }],
  };
}

// --------------------------------------------------------- scenario library

/**
 * The scenario suite, as it stands against the current rules.
 *
 * A passing suite, because that is what a shipped baseline looks like — a
 * library that fails on the code it ships with is not a baseline. The browser
 * fixture reports the real library's shape rather than reimplementing the
 * runner, which lives in Rust and is the only version worth having.
 */
export function scenarios(): SuiteResult {
  const pass = (id: string, title: string, why: string, kind: string, fired: string[]) => ({
    id,
    title,
    why,
    kind,
    engine: "rules" as const,
    passed: true,
    fired,
  });

  return {
    total: 17,
    passed: 17,
    missed: 0,
    spurious: 0,
    wrongSeverity: 0,
    outcomes: [
      pass(
        "redis-fills-noeviction",
        "Cache filling with eviction disabled",
        "The commonest way a cache becomes an outage. maxmemory-policy defaults to noeviction, so when the ceiling arrives Redis stops accepting writes rather than evicting — and a failed write looks like an application bug to everyone downstream.",
        "redis",
        ["redis.maxmemory-noeviction"],
      ),
      pass(
        "redis-fills-with-eviction",
        "Cache filling with LRU eviction",
        "The same fill with a policy that evicts. The service stays up and the hit rate falls, so this must read as a capacity note rather than an outage — reporting the two identically is what teaches people to ignore both.",
        "redis",
        ["redis.maxmemory-eviction"],
      ),
      pass(
        "redis-sawtooth",
        "Cache that sawtooths without trending",
        "Memory swinging between a third and two thirds of the ceiling all day. A straight line through it has a slope and yields a confident date that is pure artefact — this is the case the forecaster's fit gate exists to refuse, and the scenario that proves it still does.",
        "redis",
        [],
      ),
      pass(
        "redis-already-over",
        "Cache already past its ceiling, and erratic",
        "Over the limit right now, with history too ragged to fit. Being past a threshold is a reading of the present rather than a projection, so it must still be reported — an earlier version gated this behind the fit and reported nothing at all on an instance that was already refusing writes.",
        "redis",
        ["redis.maxmemory-noeviction"],
      ),
      pass(
        "config-drift",
        "File and running process disagree",
        "An edit nothing reloaded. The file is right and has never taken effect, so everyone reading it is reasoning about a service that does not match it — and a restart will silently adopt the file's values without anyone deciding to.",
        "redis",
        ["redis.config-drift"],
      ),
      pass(
        "config-same-size-two-ways",
        "The same ceiling written two ways",
        "2147483648 and 2gb are the same number. A drift rule that called this a disagreement would fire on every Redis in existence and be switched off within a day, taking the real drift detection with it.",
        "redis",
        [],
      ),
      pass(
        "k8s-besteffort",
        "Workload scheduled as BestEffort",
        "A container with neither CPU nor memory requests is placed wherever there is room and evicted first when a node comes under pressure, regardless of how important it is.",
        "kubernetes",
        ["kubernetes.workload-besteffort", "kubernetes.workload-no-memory-limit"],
      ),
      pass(
        "no-config-at-all",
        "A connection with no configuration declared",
        "Metrics climbing toward a threshold with nothing known about how the service is set up. Nothing may fire: every finding here rests on a setting, and inventing one would be exactly the guess the whole module is built to avoid.",
        "redis",
        [],
      ),
    ],
  };
}

/**
 * A runtime report with one of each disagreement worth seeing.
 *
 * Agreement is deliberately absent: the real command returns only connections
 * with something to say, because listing every declaration root agrees with
 * would bury the few that are wrong.
 */
export function runtimeReport(): RuntimeReport {
  return {
    declared: 3,
    total: 12,
    checks: [
      {
        profileId: "payments-db",
        profileName: "payments-db",
        declared: {
          kind: "container",
          hostProfileId: "orders-api",
          name: "payments-db",
        },
        discrepancies: [
          {
            kind: "disagrees",
            host: "orders-api",
            declared: "payments-db",
            inferred: "payments-db-primary",
          },
        ],
      },
      {
        profileId: "session-cache",
        profileName: "session-cache",
        declared: {
          kind: "container",
          hostProfileId: "orders-api",
          name: "session-cache",
        },
        discrepancies: [
          { kind: "notFound", host: "orders-api", name: "session-cache" },
        ],
      },
    ],
    uncovered: [
      {
        hostProfileId: "orders-api",
        hostLabel: "orders-api",
        name: "orders-redis-sidecar",
        kind: "redis",
      },
    ],
  };
}

// ------------------------------------------------------------------ history
//
// What a database would have kept. The ring buffer holds hours; these answer
// the questions that need weeks, so the timestamps deliberately run back past
// anything `seriesFor` produces — a preview where "30 days" and "3 hours" drew
// the same rows would hide the whole point of the history store.

const DAY = 24 * 60 * 60_000;

/** Past firings of the conditions this scenario is already showing. */
export function alertHistory(profileId: string): PastAlert[] {
  const live = ALERTS.filter((a) => a.profileId === profileId);
  if (live.length === 0) return [];

  // Each live alert, plus earlier firings of the same condition. The
  // fingerprint is what ties them together, exactly as it does on disk.
  return live.flatMap((alert) =>
    [0, 3, 9, 16].map((daysAgo, i) => ({
      fingerprint: alert.fingerprint,
      profileId: alert.profileId,
      profileName: alert.profileName,
      severity: alert.severity as string,
      title: alert.title,
      detail: alert.detail,
      component: alert.component,
      predicted: false,
      firstSeen: iso(daysAgo * DAY),
      lastSeen: iso(daysAgo * DAY - 6 * 60_000),
      // The newest one is still firing; the older ones ended.
      resolvedAt: i === 0 ? undefined : iso(daysAgo * DAY - 6 * 60_000),
    })),
  );
}

/** Conditions that fired more than once over the window, worst first. */
export function flapping(): FlappingCondition[] {
  const counts = new Map<string, FlappingCondition>();
  for (const alert of ALERTS) {
    const key = `${alert.profileId}:${alert.fingerprint}`;
    const seen = counts.get(key);
    if (seen) seen.times += 1;
    else
      counts.set(key, {
        profileName: alert.profileName,
        title: alert.title,
        times: 4,
      });
  }
  return [...counts.values()].sort((a, b) => b.times - a.times);
}

/** Recorded changes, reaching back further than the live change feed. */
export function changeHistory(profileId?: string): PastChange[] {
  const scoped = profileId ? CHANGES.filter((c) => c.profileId === profileId) : CHANGES;
  return scoped.flatMap((change, i) =>
    [0, 5, 12].map((daysAgo, n) => ({
      // The newest occurrence keeps the live row's id: on a real backend it is
      // that row, read back off disk, and the merge has to dedupe it.
      id: n === 0 ? change.id : `${change.id}-h${n}`,
      at: n === 0 ? change.at : iso(daysAgo * DAY + i * 90_000),
      profileId: change.profileId,
      profileName: change.profileName,
      kind: change.kind as string,
      change: change.change as string,
      source: change.source as string,
      section: change.section,
      component: change.component,
      from: change.from,
      to: change.to,
      summary: change.summary,
    })),
  );
}

/** A configured store with a plausible amount in it. */
export function historyStats(): HistoryStats {
  return {
    configured: true,
    timescale: true,
    rows: {
      alerts: 1_284,
      changes: 3_907,
      metric_samples: 2_410_556,
      log_lines: 18_332_104,
    },
  };
}

// ------------------------------------------------------------------ inline AI
//
// No model in the preview. These answers are written from the fixtures the
// way a model given the same facts would answer, and say so in the model
// field, so the inline notes can be looked at and judged without a key.

/** The chart's "✦ Explain this", answered from what the fixtures hold. */
export function explainWindow(profileId: string, from: string, to: string): Promise<ComponentReading> {
  const name = SNAPSHOTS.find((s) => s.profileId === profileId)?.profileName ?? profileId;
  const change = CHANGES.find((c) => c.profileId === profileId);
  const alert = ALERTS.find((a) => a.profileId === profileId);
  const hm = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const text =
    change && alert
      ? `The climb starts a few minutes after ${change.component} moved ${change.from ?? "?"} → ${change.to} at ${hm(change.at)}, and "${alert.title}" follows at ${hm(alert.firstSeen)} — the deploy is the likely cause. The errors in the window are one message repeating, which fits a memory regression in ${change.to}; payments-db degrading at the same moment looks like consequence, not cause.`
      : `Nothing recorded in this window explains the movement on ${name}: no alerts, no changes and no warnings. Check whether traffic shifted upstream.`;
  const grounds = [
    `connection: ${name}`,
    `window: ${hm(from)} to ${hm(to)}`,
    "metric CPU: 58% → 91% (low 57%, peak 94%); the window before averaged 59%",
    ...(change ? [`change ${hm(change.at)} ${change.summary}`] : []),
    ...(alert ? [`alert ${hm(alert.firstSeen)} critical: "${alert.title}" — still firing`] : []),
    "logs: 41 lines at warn or above in the window, 37 of them errors",
    "log ×33: OOMKilled: container worker exceeded memory limit #Mi",
  ];
  return new Promise((resolve) =>
    setTimeout(
      () => resolve({ text, model: "preview — no model called", grounds, readAt: new Date().toISOString() }),
      1100,
    ),
  );
}

/** The log view's "✦ Explain these lines", answered from the selection. */
export function explainLines(lines: LogEvent[]): Promise<ComponentReading> {
  const name = lines[0]?.profileName ?? "this connection";
  const bad = lines.filter((l) => l.level === "error" || l.level === "fatal").length;
  const hm = (iso: string) => new Date(iso).toLocaleTimeString([], { hour12: false });
  const text =
    bad > 0
      ? `${lines.length === 1 ? "This line is" : "These lines are"} ${name} losing its connection mid-request and retrying. The same message has repeated through the last hour, starting just after the 2.14.3 deploy, so this is a recurring fault rather than a blip; payments-db's replication lag at the same moment looks like consequence, not cause.`
      : `Routine activity on ${name}: nothing in these lines indicates a fault, and nothing failed elsewhere at the same moment.`;
  const grounds = [
    `selected: ${lines.length} line${lines.length === 1 ? "" : "s"}`,
    ...lines.slice(0, 6).map((l) => `line ${hm(l.timestamp)} ${l.level.toUpperCase()} [${l.profileName} / ${l.source}] ${l.message}`),
    `connection ${name}: now down`,
    "change 15:19 worker 2.14.2 → 2.14.3",
    'recurrence: "connection reset by #.#.#.#" ×30 in the last hour, first at 15:20',
    "elsewhere [payments-db] replication lag #s",
  ];
  return new Promise((resolve) =>
    setTimeout(
      () => resolve({ text, model: "preview — no model called", grounds, readAt: new Date().toISOString() }),
      1000,
    ),
  );
}
