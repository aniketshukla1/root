import type { Metric, MetricUnit } from "./types";

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return "—";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let value = Math.abs(bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const decimals = value >= 100 || unit === 0 ? 0 : 1;
  return `${(bytes < 0 ? -value : value).toFixed(decimals)} ${units[unit]}`;
}

export function formatMetricValue(value: number, unit: MetricUnit): string {
  if (!Number.isFinite(value)) return "—";
  switch (unit) {
    case "percent":
      return `${value.toFixed(value >= 10 ? 0 : 1)}%`;
    case "bytes":
      return formatBytes(value);
    case "millis":
      return value >= 1000 ? `${(value / 1000).toFixed(1)}s` : `${Math.round(value)}ms`;
    case "seconds":
      return value >= 90 ? `${(value / 60).toFixed(1)}m` : `${value.toFixed(1)}s`;
    case "per_second":
      return `${value.toFixed(1)}/s`;
    case "count": {
      // Keeps climbing past thousands: a document count runs to hundreds of
      // millions, and "412908.1k" is not a number anyone can read.
      const abs = Math.abs(value);
      if (abs >= 1e9) return `${(value / 1e9).toFixed(1)}B`;
      if (abs >= 1e6) return `${(value / 1e6).toFixed(1)}M`;
      if (abs >= 10000) return `${(value / 1000).toFixed(1)}k`;
      return `${Math.round(value)}`;
    }
  }
}

export function formatMetric(metric: Metric): string {
  return formatMetricValue(metric.value, metric.unit);
}

export function isBreaching(metric: Metric): boolean {
  return metric.warnAbove !== undefined && metric.warnAbove !== null && metric.value > metric.warnAbove;
}

/** Compact wall-clock time for log lines. */
export function formatTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleTimeString([], { hour12: false });
}

export function formatDateTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

/** "12s ago", "4m ago" — how stale a snapshot or alert is. */
export function formatRelative(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  const seconds = Math.round((Date.now() - date.getTime()) / 1000);
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.round(ms / 60000)}m`;
}
