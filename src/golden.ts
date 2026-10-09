// Which golden signal a metric speaks to, so every service's dashboard opens
// on the same four questions in the same order — is it slow, how busy is it,
// is it failing, how full is it — whatever kind of thing it is.
//
// Classified from the metric's key, label and unit rather than listed per
// kind: twenty-eight collectors and the demo name their metrics freely, and a
// list per kind is out of date the day a collector gains a metric. A kind
// whose metric the rules misread gets an entry in OVERRIDES instead.
//
// No imports beyond types, so `scripts/check-golden.ts` runs it under Node.

import type { MetricUnit } from "./types";

export type Signal = "latency" | "traffic" | "errors" | "saturation" | "other";

/** The order a dashboard reads in: Google's SRE order, the rest last. */
export const SIGNAL_ORDER: Signal[] = ["latency", "traffic", "errors", "saturation", "other"];

export const SIGNAL_LABEL: Record<Signal, string> = {
  latency: "Latency",
  traffic: "Traffic",
  errors: "Errors",
  saturation: "Saturation",
  other: "Inventory",
};

/** `kind:key` → signal, where the rules below get a metric wrong. */
const OVERRIDES: Record<string, Signal> = {
  // A count of executor slots in use is how full Jenkins is, not traffic.
  "jenkins:building": "saturation",
  // Up is availability: a check that stops answering is a failure.
  "http_endpoint:up": "errors",
};

const ERRORS =
  /error|fail|5xx|reject|drop|abort|deadlock|unhealthy|unstable|broken|stuck|unassigned|offline|under.?replicated|restart|dlq|_down|down_|alerts_firing|unacked/;
const LATENCY = /latency|p50|p9\d|response|duration|slow|replica_lag|replication_lag/;
const TRAFFIC =
  /rate|rps|qps|\bops\b|^ops|op_|requests|throughput|_msgs|net_rx|net_tx|bytes_sent|byte_rate|in_bytes|out_bytes|sessions|connections_created/;
const SATURATION =
  /cpu|mem|heap|disk|load|fill|used|size|queue|backlog|lag|blocked|waiting|busy|threads|connection|clients|quota|fullest|saturation|fragment|open_slots|idle|evict|hit/;

export function signalOf(metric: { key: string; label: string; unit: MetricUnit }, kind?: string): Signal {
  const override = kind ? OVERRIDES[`${kind}:${metric.key}`] : undefined;
  if (override) return override;
  const text = `${metric.key} ${metric.label}`.toLowerCase();
  if (ERRORS.test(text)) return "errors";
  if (metric.unit === "millis" || LATENCY.test(text)) return "latency";
  if (/lag/.test(text) && metric.unit === "seconds") return "latency";
  // A hit ratio is how well a cache copes with its size, not how busy it is.
  if (/hit/.test(text)) return "saturation";
  if (metric.unit === "per_second" || metric.unit === "bytes_per_second" || TRAFFIC.test(text)) {
    return "traffic";
  }
  if (metric.unit === "percent" || SATURATION.test(text)) return "saturation";
  return "other";
}

/** Metrics grouped by signal, in reading order; within a group, as collected. */
export function bySignal<T extends { key: string; label: string; unit: MetricUnit }>(
  metrics: T[],
  kind?: string,
): [Signal, T[]][] {
  const groups = new Map<Signal, T[]>();
  for (const metric of metrics) {
    const signal = signalOf(metric, kind);
    groups.set(signal, [...(groups.get(signal) ?? []), metric]);
  }
  return SIGNAL_ORDER.filter((s) => groups.has(s)).map((s) => [s, groups.get(s)!]);
}
