// Run: node --experimental-strip-types scripts/check-golden.ts
import assert from "node:assert/strict";

import { bySignal, signalOf } from "../src/golden.ts";

const cases: [kind: string, key: string, label: string, unit: string, expected: string][] = [
  ["http_endpoint", "latency", "Latency", "millis", "latency"],
  ["edge-proxy", "p99", "p99", "millis", "latency"],
  ["postgres", "replication_lag", "Replication lag", "seconds", "latency"],
  ["jenkins", "duration", "Duration", "seconds", "latency"],
  ["nginx", "request_rate", "Requests", "per_second", "traffic"],
  ["redis", "ops", "Ops/s", "per_second", "traffic"],
  ["mongodb", "op_query", "Queries", "per_second", "traffic"],
  ["nats", "in_msgs", "Messages in", "per_second", "traffic"],
  ["tomcat", "error_rate", "Error rate", "percent", "errors"],
  ["jenkins", "failing", "Failing jobs", "count", "errors"],
  ["kubernetes", "restarts", "Restarts", "count", "errors"],
  ["docker", "unhealthy", "Unhealthy", "count", "errors"],
  ["prometheus", "targets_down", "Targets down", "count", "errors"],
  ["http_endpoint", "up", "Up", "count", "errors"],
  ["docker", "cpu", "CPU", "percent", "saturation"],
  ["docker", "memory", "Memory", "bytes", "saturation"],
  ["kafka", "lag", "Consumer lag", "count", "saturation"],
  ["redis", "hit_rate", "Hit rate", "percent", "saturation"],
  ["redis", "evicted", "Evicted keys", "count", "saturation"],
  ["database", "connections", "Connections", "count", "saturation"],
  ["jenkins", "building", "Building", "count", "saturation"],
  ["docker", "containers", "Containers", "count", "other"],
  ["kafka", "topics", "Topics", "count", "other"],
  ["linux_host", "uptime", "Uptime", "seconds", "other"],
];
for (const [kind, key, label, unit, expected] of cases) {
  assert.equal(signalOf({ key, label, unit: unit as never }, kind), expected, `${kind}:${key}`);
}

// Groups come out in reading order, each keeping collection order inside.
const groups = bySignal(
  [
    { key: "containers", label: "Containers", unit: "count" as const },
    { key: "memory", label: "Memory", unit: "bytes" as const },
    { key: "cpu", label: "CPU", unit: "percent" as const },
    { key: "error_rate", label: "Error rate", unit: "percent" as const },
    { key: "p99", label: "p99", unit: "millis" as const },
  ],
  "docker",
);
assert.deepEqual(
  groups.map(([signal, list]) => [signal, list.map((m) => m.key)]),
  [
    ["latency", ["p99"]],
    ["errors", ["error_rate"]],
    ["saturation", ["memory", "cpu"]],
    ["other", ["containers"]],
  ],
);

console.log(`golden: ok (${cases.length} metrics)`);
