// Time ranges the metric ring buffer can actually satisfy.
//
// The backend keeps a fixed number of samples per metric, not a fixed duration
// (`MAX_POINTS` in src-tauri/src/history.rs). So how far back a connection can
// be read depends entirely on how often it polls: 720 samples is three hours at
// 15s and twelve hours at 60s.
//
// Offering a 24h button regardless meant "24h" and "3h" drew the same series
// with different labels — silently, with no error. Everything here exists so a
// range control only offers windows that hold data.

import type { MetricSeries } from "./types";

/** Mirrors `MAX_POINTS` in src-tauri/src/history.rs. Keep the two in step. */
export const MAX_POINTS = 720;

/** The backend clamps `minutes` to this, whatever we ask for. */
export const BACKEND_MAX_MINUTES = 1440;

export interface Range {
  minutes: number;
  label: string;
}

const CANDIDATES: Range[] = [
  { minutes: 5, label: "5m" },
  { minutes: 15, label: "15m" },
  { minutes: 60, label: "1h" },
  { minutes: 180, label: "3h" },
  { minutes: 360, label: "6h" },
  { minutes: 720, label: "12h" },
  { minutes: 1440, label: "24h" },
];

/** Longest window the ring can hold at this poll interval, in minutes. */
export function ceilingMinutes(pollSecs: number): number {
  const held = Math.floor((MAX_POINTS * Math.max(1, pollSecs)) / 60);
  return Math.min(held, BACKEND_MAX_MINUTES);
}

/**
 * Ranges worth offering for a connection. Always at least the two shortest —
 * a very fast poll can hold less than five minutes, and a control with one
 * button is worse than a slightly optimistic one.
 */
export function rangesFor(pollSecs: number): Range[] {
  const ceiling = ceilingMinutes(pollSecs);
  const usable = CANDIDATES.filter((r) => r.minutes <= ceiling);
  return usable.length >= 2 ? usable : CANDIDATES.slice(0, 2);
}

/** The widest set any of these connections can satisfy, for the global screens. */
export function rangesAcross(pollIntervals: number[]): Range[] {
  if (pollIntervals.length === 0) return CANDIDATES.slice(0, 4);
  return rangesFor(Math.max(...pollIntervals));
}

/** Snap a requested window down to something the connection can serve. */
export function clampMinutes(minutes: number, pollSecs: number): number {
  const available = rangesFor(pollSecs);
  const exact = available.find((r) => r.minutes === minutes);
  if (exact) return exact.minutes;
  return available[available.length - 1].minutes;
}

export interface Span {
  /** Oldest sample across the series, in ms. */
  from?: number;
  /** Newest sample across the series, in ms. */
  to?: number;
  /** Most samples any one series carries. */
  points: number;
  /** True once a series has hit the ring's limit — older data is being dropped. */
  saturated: boolean;
}

/** What the data actually covers, as opposed to what was asked for. */
export function spanOf(series: MetricSeries[]): Span {
  let from = Infinity;
  let to = -Infinity;
  let points = 0;

  for (const s of series) {
    if (s.points.length === 0) continue;
    points = Math.max(points, s.points.length);
    const first = new Date(s.points[0].t).getTime();
    const last = new Date(s.points[s.points.length - 1].t).getTime();
    if (Number.isFinite(first)) from = Math.min(from, first);
    if (Number.isFinite(last)) to = Math.max(to, last);
  }

  return {
    from: from === Infinity ? undefined : from,
    to: to === -Infinity ? undefined : to,
    points,
    saturated: points >= MAX_POINTS,
  };
}

/** "2h 58m", "14m", "42s" — the real depth of what is on screen. */
export function describeSpan(span: Span): string {
  if (span.from === undefined || span.to === undefined) return "no history";
  const seconds = Math.max(0, Math.round((span.to - span.from) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`;
}
