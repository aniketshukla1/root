import type { CellValue, ColumnSpec, Metric, MetricUnit } from "./types";

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
    case "seconds": {
      // Past minutes into hours and days: an uptime of 41 days printed as
      // "59600.3m" on every axis it touched.
      const abs = Math.abs(value);
      if (abs < 90) return `${value.toFixed(1)}s`;
      if (abs < 90 * 60) return `${(value / 60).toFixed(1)}m`;
      if (abs < 48 * 3600) return `${(value / 3600).toFixed(1)}h`;
      return `${(value / 86400).toFixed(1)}d`;
    }
    case "per_second":
      return `${value.toFixed(1)}/s`;
    case "bytes_per_second":
      return `${formatBytes(value)}/s`;
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

/**
 * How long until something happens, at a scale a person plans around.
 *
 * Deliberately coarse. A projection is an estimate with a fit score attached,
 * and rendering it as "6d 4h 12m" would dress a straight line through noisy
 * data as a countdown. Days are the unit capacity decisions are actually made
 * in, and beyond a season the honest answer is that it is not the problem.
 */
export function formatHorizon(seconds: number): string {
  if (seconds <= 0) return "now";
  const hours = seconds / 3600;
  if (hours < 1) return `${Math.max(1, Math.round(seconds / 60))}m`;
  if (hours < 48) return `${Math.round(hours)}h`;
  const days = hours / 24;
  if (days < 90) return `${Math.round(days)}d`;
  return "beyond 90d";
}

/**
 * A rate, over a period the reader plans in rather than per second.
 *
 * Storage grows in gigabytes a day, not bytes a second, and the per-second
 * figure is unreadable for every unit that matters to capacity planning.
 *
 * The period follows the window the rate was fitted over, because quoting a
 * one-hour slope per day multiplies it by twenty-four and the result stops
 * being defensible: a CPU drifting down over an hour was reading "−197%/day",
 * which is not a thing a percentage can do. Half a day of history earns a
 * daily figure; less than that gets an hourly one, so the number quoted is
 * never more than about twice the span actually measured.
 */
export function formatRate(perSecond: number, unit: MetricUnit, windowSecs?: number): string {
  const daily = windowSecs === undefined || windowSecs >= 12 * 3600;
  const period = daily ? 86_400 : 3_600;
  const scaled = perSecond * period;
  if (Math.abs(scaled) < 1e-9) return "flat";
  const sign = scaled > 0 ? "+" : "−";
  return `${sign}${formatMetricValue(Math.abs(scaled), unit)}/${daily ? "day" : "h"}`;
}

/**
 * One cell, as a string.
 *
 * The table renders cells as elements — a meter, a badge, a row of tags. A
 * tooltip has no room for any of that and wants the same value as text, so the
 * formatting rules live here rather than being approximated a second time.
 */
export function cellText(column: ColumnSpec, value: CellValue | undefined): string | null {
  if (!value || value.type === "null") return null;
  switch (value.type) {
    case "status":
      return value.value;
    case "tags":
      return value.value.filter(Boolean).join(" · ") || null;
    case "duration":
      return formatDuration(value.value * 1000);
    case "timestamp":
      return formatRelative(value.value);
    case "gauge":
      return formatMetricValue(
        value.value,
        column.kind.type === "gauge" ? column.kind.unit : "count",
      );
    case "number":
      return formatMetricValue(
        value.value,
        column.kind.type === "metric" ? column.kind.unit : "count",
      );
    default:
      return value.value || null;
  }
}
