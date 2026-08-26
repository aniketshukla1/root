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
  AppConfig,
  DashboardState,
  HealthSnapshot,
  HistogramBucket,
  LogEvent,
  LogLevel,
  MetricSeries,
  Point,
  ServiceDescriptor,
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

function series(
  n: number,
  seed: number,
  base: number,
  shape: "flat" | "rise" | "fall" | "spike",
): Point[] {
  const r = seeded(seed);
  const out: Point[] = [];
  let v = base;
  for (let i = 0; i < n; i += 1) {
    v += (r() - 0.5) * base * 0.13;
    if (shape === "rise") v += base * 0.012;
    if (shape === "fall") v -= base * 0.01;
    if (shape === "spike" && i > n * 0.72) v += base * 0.06;
    v = Math.max(base * 0.1, v);
    out.push({ t: iso((n - i) * 15_000), v: Number(v.toFixed(2)) });
  }
  return out;
}

const METRIC_KINDS = ["docker", "postgres", "elasticsearch", "jenkins", "rabbitmq",
  "nginx", "kubernetes", "kafka", "redis", "mongodb", "prometheus", "http_endpoint"];

export const DESCRIPTORS: ServiceDescriptor[] = METRIC_KINDS.map((kind, i) => ({
  kind,
  label: kind.replace(/[_-]+/g, " ").replace(/^./, (c) => c.toUpperCase()),
  monogram: kind.slice(0, 2).toUpperCase(),
  accent: i,
  category:
    ["docker", "kubernetes"].includes(kind) ? "Containers & orchestration"
    : ["postgres", "mongodb", "redis"].includes(kind) ? "Databases"
    : ["kafka", "rabbitmq"].includes(kind) ? "Messaging"
    : "Web & endpoints",
  blurb: "",
  endpoint: { key: "endpoint", label: "Endpoint", kind: "text", required: true, advanced: false },
  fields: [],
  defaultPollSecs: 15,
  capabilities: { logs: true, actions: [], diagnostics: [] },
}));

type Spec = [id: string, name: string, kind: string, health: HealthSnapshot["health"],
  summary: string, team: string, parts: number];

const ESTATE: Spec[] = [
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

const METRICS: Record<string, [string, string, number, string, number?][]> = {
  "orders-api": [["cpu", "CPU", 62, "percent", 75], ["memory", "Memory", 7.4, "bytes", 8],
    ["running", "Running", 5, "count"], ["error_rate", "Error rate", 8.1, "percent", 2]],
  "payments-db": [["lag", "Lag", 42, "seconds", 30], ["conns", "Connections", 318, "count"]],
  "search-cluster": [["heap", "Heap", 81, "percent", 85], ["nodes", "Nodes", 6, "count"]],
  "ci-pipeline": [["duration", "Duration", 14, "seconds"], ["queue", "Queue", 7, "count"]],
  "queue-broker": [],
  "edge-proxy": [["rps", "Req/s", 4200, "per_second"], ["p99", "p99", 38, "millis"]],
  "checkout-web": [["pods", "Pods ready", 24, "count"], ["cpu", "CPU", 34, "percent", 75]],
  "events-bus": [["lag", "Lag", 1200, "count"], ["brokers", "Brokers", 3, "count"]],
  "session-cache": [["hit", "Hit rate", 99.1, "percent"], ["memory", "Memory", 1.4, "bytes"]],
  "billing-mongo": [["conns", "Connections", 88, "count"]],
  "metrics-store": [["targets", "Targets up", 42, "count"]],
  "status-page": [["latency", "Latency", 42, "millis", 800]],
};

function rowsFor(spec: Spec): HealthSnapshot["sections"] {
  const [id, , , health, , , parts] = spec;
  const r = seeded(id.length * 977);
  const bad = health === "down" ? 3 : health === "degraded" ? 2 : health === "unknown" ? 0 : 0;
  const rows = Array.from({ length: Math.min(parts, 24) }, (_, i) => {
    const rowHealth: HealthSnapshot["health"] =
      i < bad ? (health === "down" && i < 2 ? "down" : "degraded") : "healthy";
    return {
      id: `${id}-${i + 1}`,
      name: `${id.split("-")[0]}-worker-${i + 1}`,
      health: rowHealth,
      summary: rowHealth === "healthy" ? "running" : "exited 137",
      cells: {
        image: { type: "text" as const, value: `${id}:2.14.3` },
        cpu: { type: "number" as const, value: Math.round(r() * 90) },
        memory: { type: "number" as const, value: Number((r() * 8).toFixed(1)) },
        restarts: { type: "number" as const, value: rowHealth === "healthy" ? 0 : Math.ceil(r() * 9) },
      },
      actions: [],
    };
  });
  return [{
    id: "components", label: "Components", priority: 1, tracked: true,
    emptyText: "No components reported",
    columns: [
      { key: "name", label: "Name", kind: { type: "text" }, align: "start", primary: true, sortable: true, secondary: false, revision: false },
      { key: "image", label: "Image", kind: { type: "code" }, align: "start", primary: false, sortable: true, secondary: true, revision: true },
      { key: "cpu", label: "CPU", kind: { type: "metric", unit: "percent" }, align: "end", primary: false, sortable: true, secondary: false, revision: false },
      { key: "memory", label: "Memory", kind: { type: "metric", unit: "bytes" }, align: "end", primary: false, sortable: true, secondary: false, revision: false },
      { key: "restarts", label: "Restarts", kind: { type: "text" }, align: "end", primary: false, sortable: true, secondary: true, revision: false },
    ],
    rows,
  }];
}

export const SNAPSHOTS: HealthSnapshot[] = ESTATE.map((spec) => {
  const [id, name, kind, health, summary] = spec;
  return {
    profileId: id,
    profileName: name,
    kind,
    health,
    summary,
    metrics: (METRICS[id] ?? []).map(([key, label, value, unit, warnAbove]) => ({
      key, label, value, unit: unit as never, warnAbove,
    })),
    sections: rowsFor(spec),
    checkedAt: iso(6_000 + id.length * 900),
    latencyMs: 20 + id.length * 3,
    error: health === "unknown" ? "collector unreachable" : undefined,
  };
});

export const ALERTS: Alert[] = [
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
];

export const DASHBOARD: DashboardState = {
  snapshots: SNAPSHOTS,
  alerts: ALERTS,
  overallHealth: "degraded",
  servicesTotal: SNAPSHOTS.length,
  servicesHealthy: SNAPSHOTS.filter((s) => s.health === "healthy").length,
  servicesDown: SNAPSHOTS.filter((s) => s.health === "down").length,
  generatedAt: iso(0),
};

export function seriesFor(profileId: string): MetricSeries[] {
  return (METRICS[profileId] ?? []).map(([key, label, value, unit, warnAbove], i) => {
    const shape = key === "memory" || key === "lag" || key === "heap" ? "rise"
      : key === "error_rate" ? "spike" : key === "running" ? "fall" : "flat";
    const points = series(60, profileId.length * 31 + i * 7, value, shape);
    const values = points.map((p) => p.v);
    return {
      key, label, unit: unit as never, warnAbove, points,
      changePct: Number((((values[values.length - 1] - values[0]) / (values[0] || 1)) * 100).toFixed(1)),
      min: Math.min(...values), max: Math.max(...values), latest: values[values.length - 1],
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

export const LOGS: LogEvent[] = Array.from({ length: 240 }, (_, i) => {
  const [profileId, source, level, message] = LINES[i % LINES.length];
  const spec = ESTATE.find((e) => e[0] === profileId)!;
  return {
    id: `log-${i}`, profileId, profileName: profileId, kind: spec[2], source, level, message,
    timestamp: iso(i * 4_300), stream: level === "info" ? "event" : "runtime",
  };
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

export const CONFIG: AppConfig = {
  profiles: ESTATE.map(([id, name, kind, , , team]) => ({
    id, name, kind, endpoint: `${kind}://${id}.internal`,
    hasSecret: true, enabled: true, pollIntervalSecs: 15, options: {},
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
};

/** One observed deploy in the window, so charts have a marker to render. */
export const CHANGES = [
  {
    id: "c1",
    profileId: "orders-api",
    profileName: "orders-api",
    kind: "docker",
    component: "worker",
    change: "deployed" as const,
    from: "2.14.2",
    to: "2.14.3",
    at: iso(22 * 60_000),
    reported: false,
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

  return {
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
}
