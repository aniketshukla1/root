// Time ranges that can actually be satisfied.
//
// Two sources, with very different reach, and the rule is the same for both: a
// range control only offers windows that hold data.
//
//   - **The ring buffer**, always present. It keeps a fixed number of samples
//     per metric, not a fixed duration (`MAX_POINTS` in
//     src-tauri/src/history.rs), so how far back it reaches depends entirely on
//     the poll interval: 720 samples is three hours at 15s and twelve at 60s.
//   - **The history store**, when one is configured. Its hourly rollup has no
//     retention policy, so it reaches back as far as the database goes.
//
// Offering a 24h button against a ring that holds three meant "24h" and "3h"
// drew the same series under different labels — silently, with no error. The
// same trap exists in the other direction now: offering only ring-sized
// windows to somebody who has a database hides the history they are paying to
// keep. Hence `retained`, which is answered by the backend rather than
// guessed.

import type { MetricSeries } from "./types";

/** Mirrors `MAX_POINTS` in src-tauri/src/history.rs. Keep the two in step. */
export const MAX_POINTS = 720;

/** The backend clamps a ring-only `minutes` to this, whatever we ask for. */
export const BACKEND_MAX_MINUTES = 1440;

/**
 * And a store-backed one to this — ninety days, matching `STORE_MAX_MINUTES`
 * in src-tauri/src/commands.rs. Keep the two in step.
 */
export const STORE_MAX_MINUTES = 90 * 24 * 60;

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

/**
 * Only offered when a database is behind them.
 *
 * These are read from the hourly rollup, so a point is an hour no matter which
 * one is picked — 7d is 168 points and 30d is 720. Nothing finer exists that
 * far back, and pretending otherwise would put a smooth line where there is
 * really one sample an hour.
 */
const LONG_CANDIDATES: Range[] = [
  { minutes: 2 * 1440, label: "2d" },
  { minutes: 7 * 1440, label: "7d" },
  { minutes: 30 * 1440, label: "30d" },
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
 *
 * `retained` says a history store is configured, which is the only thing that
 * makes the day-scale windows answerable. It defaults to false so a caller
 * that has not been taught about the store cannot offer one by accident.
 */
export function rangesFor(pollSecs: number, retained = false): Range[] {
  const ceiling = ceilingMinutes(pollSecs);
  const usable = CANDIDATES.filter((r) => r.minutes <= ceiling);
  const base = usable.length >= 2 ? usable : CANDIDATES.slice(0, 2);
  if (!retained) return base;

  // Everything the ring cannot reach is still answerable off disk, so the
  // in-between windows the ring was too small for come back too.
  const upToADay = CANDIDATES.filter((r) => r.minutes > ceiling);
  return [...base, ...upToADay, ...LONG_CANDIDATES];
}

/** The widest set any of these connections can satisfy, for the global screens. */
export function rangesAcross(pollIntervals: number[], retained = false): Range[] {
  if (pollIntervals.length === 0) return CANDIDATES.slice(0, 4);
  return rangesFor(Math.max(...pollIntervals), retained);
}

/** Snap a requested window down to something the connection can serve. */
export function clampMinutes(minutes: number, pollSecs: number, retained = false): number {
  const available = rangesFor(pollSecs, retained);
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

/** "6d 2h", "2h 58m", "14m", "42s" — the real depth of what is on screen. */
export function describeSpan(span: Span): string {
  if (span.from === undefined || span.to === undefined) return "no history";
  const seconds = Math.max(0, Math.round((span.to - span.from) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  // Days, once there are any. A store-backed window reads "720h" otherwise,
  // which is a number nobody converts in their head.
  if (hours >= 24) {
    const days = Math.floor(hours / 24);
    const spare = hours % 24;
    return spare === 0 ? `${days}d` : `${days}d ${spare}h`;
  }
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`;
}
