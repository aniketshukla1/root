// Explore's arithmetic, apart from React so `scripts/check-explore.ts` can run
// it under plain Node: what the cursor is in the middle of in a PromQL or
// LogQL query and what could go there, and how the time window moves.

import type { ExploreLanguage } from "./types";

export const DAY_MS = 86_400_000;

export const PROMQL_WORDS = [
  "rate", "irate", "increase", "delta", "deriv", "predict_linear", "absent",
  "sum", "avg", "min", "max", "count", "topk", "bottomk", "quantile", "stddev",
  "histogram_quantile", "avg_over_time", "max_over_time", "min_over_time",
  "sum_over_time", "count_over_time", "quantile_over_time", "last_over_time",
  "abs", "ceil", "floor", "round", "clamp_min", "clamp_max", "label_replace",
  "by", "without", "offset", "on", "ignoring", "group_left", "group_right",
  "and", "or", "unless",
];

export const LOGQL_WORDS = [
  "rate", "count_over_time", "bytes_over_time", "bytes_rate", "sum_over_time",
  "avg_over_time", "max_over_time", "min_over_time", "quantile_over_time",
  "absent_over_time", "sum", "avg", "min", "max", "count", "topk", "bottomk",
  "by", "without", "json", "logfmt", "pattern", "regexp", "unpack",
  "line_format", "label_format", "unwrap", "drop", "keep",
];

/** What the cursor is in the middle of, and the text a choice replaces. */
export interface Completion {
  /** `name`: a metric name or a function; `label`: a label name; `value`: one label's value. */
  kind: "name" | "label" | "value";
  /** For a value, the label it belongs to. */
  label?: string;
  /** A label inside `by (…)` takes no `="` after it. */
  grouping?: boolean;
  /** What has been typed of the token so far. */
  prefix: string;
  /** Where the token starts: a choice replaces `text[from, cursor)`. */
  from: number;
}

const LABEL = "[a-zA-Z_][a-zA-Z0-9_]*";

/**
 * What could be typed at `cursor`. Inside braces: a label name, or a label's
 * value once `name="` (or `!=`, `=~`, `!~`) is open. Inside `by (…)`: a label
 * name. Elsewhere: a metric name or a function, once a letter is typed — a
 * list on every space would be noise.
 */
export function completionAt(text: string, cursor: number, language: ExploreLanguage): Completion | null {
  if (language === "traces") return null;
  const before = text.slice(0, cursor);
  const open = before.lastIndexOf("{");
  if (open > before.lastIndexOf("}")) {
    const inside = before.slice(open + 1);
    const value = new RegExp(`(${LABEL})\\s*(?:=~|!~|!=|=)\\s*"((?:[^"\\\\]|\\\\.)*)$`).exec(inside);
    if (value) return { kind: "value", label: value[1], prefix: value[2], from: cursor - value[2].length };
    const name = new RegExp(`(?:^|,)\\s*(${LABEL})?$`).exec(inside);
    if (!name) return null;
    const prefix = name[1] ?? "";
    return { kind: "label", prefix, from: cursor - prefix.length };
  }
  // Inside a string or a range like `[5m`, nothing from a list fits.
  if (insideString(before) || before.lastIndexOf("[") > before.lastIndexOf("]")) return null;
  const grouping = new RegExp(`\\b(?:by|without|on|ignoring|group_left|group_right)\\s*\\(\\s*(?:${LABEL}\\s*,\\s*)*(${LABEL})?$`).exec(before);
  if (grouping) {
    const prefix = grouping[1] ?? "";
    return { kind: "label", grouping: true, prefix, from: cursor - prefix.length };
  }
  const word = /[a-zA-Z_:][a-zA-Z0-9_:]*$/.exec(before);
  if (!word) return null;
  return { kind: "name", prefix: word[0], from: cursor - word[0].length };
}

/** An odd number of unescaped double quotes: the cursor is inside a string. */
function insideString(text: string): boolean {
  let inside = false;
  for (let i = 0; i < text.length; i += 1) {
    if (text[i] === "\\") i += 1;
    else if (text[i] === '"') inside = !inside;
  }
  return inside;
}

/** Prefix matches first, shortest first; then anything containing it. */
export function rank(candidates: string[], prefix: string, cap = 8): string[] {
  const needle = prefix.toLowerCase();
  const starts: string[] = [];
  const contains: string[] = [];
  for (const c of candidates) {
    const lower = c.toLowerCase();
    if (lower === needle) continue;
    if (lower.startsWith(needle)) starts.push(c);
    else if (needle && lower.includes(needle)) contains.push(c);
  }
  starts.sort((a, b) => a.length - b.length || a.localeCompare(b));
  return [...starts, ...contains].slice(0, cap);
}

/**
 * The query with `choice` in place of the token being typed, and where the
 * cursor goes. A label inside a selector arrives with `="` so its values are
 * offered next; a value arrives escaped and closed.
 */
export function accept(text: string, cursor: number, c: Completion, choice: string): { text: string; cursor: number } {
  const after = text.slice(cursor);
  let insert = choice;
  let landing: number | null = null;
  if (c.kind === "label" && !c.grouping && !/^\s*[=!]/.test(after)) {
    insert = `${choice}="`;
    if (!after.startsWith('"')) {
      insert += '"';
      landing = c.from + choice.length + 2;
    }
  }
  if (c.kind === "value") {
    // Escaped for the string it lands in, and the cursor after its closing quote.
    const escaped = choice.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
    insert = after.startsWith('"') ? escaped : `${escaped}"`;
    landing = c.from + escaped.length + 1;
  }
  const next = text.slice(0, c.from) + insert + after;
  return { text: next, cursor: landing ?? c.from + insert.length };
}

// ------------------------------------------------------------------ windows

/** A window is relative — the last `minutes` — or absolute, in epoch ms. */
export type Window = { minutes: number } | { from: number; to: number };

export function resolve(window: Window, now: number): [number, number] {
  return "minutes" in window ? [now - window.minutes * 60_000, now] : [window.from, window.to];
}

/** Twice as wide around the same middle, never past now. */
export function zoomOut(window: Window, now: number): Window {
  if ("minutes" in window) return { minutes: Math.min(window.minutes * 2, 90 * 24 * 60) };
  const span = window.to - window.from;
  const to = Math.min(now, window.to + span / 2);
  return { from: to - span * 2, to };
}

/** `2026-10-10T09:30` in local time, for `<input type="datetime-local">`. */
export function toLocalInput(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function fromLocalInput(value: string): number | null {
  const ms = new Date(value).getTime();
  return Number.isFinite(ms) ? ms : null;
}

/** Lines per bucket across the window, for a volume strip on the shared axis. */
export function volume(timestamps: number[], [from, to]: [number, number], buckets: number): number[] {
  const counts = new Array<number>(buckets).fill(0);
  const width = (to - from) / buckets;
  for (const t of timestamps) {
    if (t < from || t > to) continue;
    counts[Math.min(buckets - 1, Math.floor((t - from) / width))] += 1;
  }
  return counts;
}

/** A 32- or 16-hex-digit trace id written into a log line, if there is one. */
export function traceIdIn(line: string): string | null {
  return /\btrace_?id[=:"\s]+([0-9a-f]{32}|[0-9a-f]{16})\b/i.exec(line)?.[1] ?? null;
}
