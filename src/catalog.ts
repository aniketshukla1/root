// The frontend's view of what services exist.
//
// Every lookup degrades gracefully for a kind this build has never heard of: a
// newer backend can add a collector and this frontend still renders it with a
// derived label, monogram, and a stable colour. Nothing in the UI may enumerate
// service types.

import type { Health, ServiceDescriptor, ServiceKind } from "./types";

/** Deterministic accent for a kind with no descriptor, so it stays stable. */
function fallbackAccent(kind: ServiceKind): number {
  let hash = 0;
  for (let i = 0; i < kind.length; i += 1) {
    hash = (hash * 31 + kind.charCodeAt(i)) >>> 0;
  }
  return hash;
}

/** "http_endpoint" → "Http endpoint" */
function humanize(kind: ServiceKind): string {
  const spaced = kind.replace(/[_-]+/g, " ").trim();
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

function fallbackMonogram(kind: ServiceKind): string {
  const words = kind.split(/[_-]+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  return kind.slice(0, 2).toUpperCase();
}

export function synthesizeDescriptor(kind: ServiceKind): ServiceDescriptor {
  return {
    kind,
    label: humanize(kind),
    monogram: fallbackMonogram(kind),
    accent: fallbackAccent(kind),
    category: "Other",
    blurb: "",
    endpoint: { key: "endpoint", label: "Endpoint", kind: "text", required: false, advanced: false },
    fields: [],
    defaultPollSecs: 15,
    capabilities: { logs: false, actions: [], diagnostics: [] },
  };
}

export class Catalog {
  private byKind: Map<ServiceKind, ServiceDescriptor>;

  constructor(public readonly descriptors: ServiceDescriptor[]) {
    this.byKind = new Map(descriptors.map((d) => [d.kind, d]));
  }

  /** Never returns undefined — an unknown kind gets a synthesized descriptor. */
  get(kind: ServiceKind): ServiceDescriptor {
    return this.byKind.get(kind) ?? synthesizeDescriptor(kind);
  }

  label(kind: ServiceKind): string {
    return this.get(kind).label;
  }

  category(kind: ServiceKind): string {
    return this.get(kind).category;
  }

  /** Descriptors grouped by category, for the connection picker. */
  byCategory(): { category: string; services: ServiceDescriptor[] }[] {
    const groups = new Map<string, ServiceDescriptor[]>();
    for (const descriptor of this.descriptors) {
      groups.set(descriptor.category, [...(groups.get(descriptor.category) ?? []), descriptor]);
    }
    return [...groups.entries()]
      .map(([category, services]) => ({ category, services }))
      .sort((a, b) => a.category.localeCompare(b.category));
  }

  get isEmpty(): boolean {
    return this.descriptors.length === 0;
  }
}

export const EMPTY_CATALOG = new Catalog([]);

// ------------------------------------------------------------------- colour
//
// Two different jobs, deliberately kept apart:
//
//   markColor  — a *stable* identity colour for one service kind. It must not
//                move when the set of visible services changes, so it is keyed
//                off the descriptor's accent. With more kinds than slots, two
//                kinds can share a hue; that is fine because the badge also
//                carries a logo, a monogram, and the service name.
//
//   seriesColor — a chart assignment. Slots are handed out in fixed order to
//                 the series actually plotted, never cycled and never sorted by
//                 value, so filtering a series out cannot repaint the rest.
//
// Confusing the two is what makes a chart legend shuffle its colours when a
// filter changes, so they never share a code path.

/** The validated categorical order. Index 0 is slot 1. */
export const SERIES = [
  "var(--color-series-1)",
  "var(--color-series-2)",
  "var(--color-series-3)",
  "var(--color-series-4)",
  "var(--color-series-5)",
  "var(--color-series-6)",
  "var(--color-series-7)",
  "var(--color-series-8)",
] as const;

/**
 * Identity hues, ordered so the ones furthest from a status hue are handed out
 * first. A service badge sits inches from a health dot, and while health always
 * carries a glyph and a word, leading with blue/aqua/violet/magenta keeps the
 * two channels obviously separate at a glance.
 */
const IDENTITY_ORDER = [0, 2, 6, 4, 1, 3, 5, 7];

export function markColor(accent: number): string {
  return SERIES[IDENTITY_ORDER[Math.abs(accent) % IDENTITY_ORDER.length]];
}

/**
 * The line colour for a chart that plots one thing.
 *
 * Those charts used to wear the service kind's identity hue, and a kind whose
 * slot happened to be series-2, -4 or -8 drew its CPU line in something
 * indistinguishable from serious, warn or crit: a healthy MongoDB showed a red
 * sparkline beside the word "Healthy". A chart with one series has no legend
 * for a colour to key into, so it gets no identity colour at all — the badge,
 * the logo and the name already carry that.
 */
export const DATA_INK = "var(--color-data)";

/**
 * Chart series colour for the nth plotted series. Past 8, fold into "Other".
 *
 * Handed out in IDENTITY_ORDER rather than raw slot order, so the hues that sit
 * next to a status colour are only reached once a chart is already comparing
 * five or more things and has a legend to say which line is which.
 */
export function seriesColor(index: number): string {
  return SERIES[IDENTITY_ORDER[index % IDENTITY_ORDER.length]];
}

export const SERIES_CAP = SERIES.length;

// ------------------------------------------------------------------- health

export function healthColor(health: Health): string {
  switch (health) {
    case "healthy":
      return "var(--color-ok)";
    case "degraded":
      return "var(--color-warn)";
    case "down":
      return "var(--color-crit)";
    default:
      return "var(--color-unknown)";
  }
}

/** Status is never colour alone — every dot ships beside one of these. */
export function healthLabel(health: Health): string {
  switch (health) {
    case "healthy":
      return "Healthy";
    case "degraded":
      return "Degraded";
    case "down":
      return "Down";
    default:
      return "Unknown";
  }
}

/**
 * Shape channel for places too dense for a text label — the component matrix,
 * chart markers. Distinct silhouettes, not just distinct fills.
 */
export function healthShape(health: Health): "square" | "triangle" | "circle" | "ring" {
  switch (health) {
    case "down":
      return "square";
    case "degraded":
      return "triangle";
    case "healthy":
      return "circle";
    default:
      return "ring";
  }
}

export function isProblem(health: Health): boolean {
  return health === "down" || health === "degraded";
}
