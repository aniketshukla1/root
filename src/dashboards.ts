// Dashboards people build: the arithmetic and the import formats, apart from
// React so `scripts/check-dashboards.ts` can run them under plain Node.
//
// The grid has twelve columns and rows of a fixed height. A panel's place is
// its order in the list plus its width and height; the browser's grid packs
// them, which is all "arranging" needs to mean for panels a person drags
// into order — absolute coordinates would only add overlaps to resolve.

import type { Dashboard, DashboardPanel, MetricUnit, PanelQuery } from "./types";

export const COLUMNS = 12;
export const MAX_ROWS = 6;

/** The variables a panel can name in place of a fixed connection. */
export const VARIABLES = ["environment", "team", "service"] as const;
export type Variable = (typeof VARIABLES)[number];

export function newId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `id-${Math.random().toString(36).slice(2)}-${Date.now().toString(36)}`;
}

export function newDashboard(title = "Untitled dashboard"): Dashboard {
  return { id: newId(), title, panels: [], variables: {} };
}

export function newPanel(query: PanelQuery, title: string): DashboardPanel {
  return { id: newId(), title, kind: "timeseries", w: 6, h: 3, query };
}

/** The connection a panel reads, with `$service` replaced by the variable. */
export function resolveProfile(profileId: string, variables: Record<string, string>): string | null {
  if (profileId === "$service") return variables.service || null;
  return profileId || null;
}

/** Move the panel at `from` to `to`, keeping everything else in order. */
export function movePanel<T>(panels: T[], from: number, to: number): T[] {
  if (from === to || from < 0 || from >= panels.length) return panels;
  const next = [...panels];
  const [moved] = next.splice(from, 1);
  next.splice(Math.max(0, Math.min(to, next.length)), 0, moved);
  return next;
}

/** Grow or shrink a panel, kept inside the grid. */
export function resizePanel(panel: DashboardPanel, dw: number, dh: number): DashboardPanel {
  return {
    ...panel,
    w: Math.max(2, Math.min(COLUMNS, panel.w + dw)),
    h: Math.max(1, Math.min(MAX_ROWS, panel.h + dh)),
  };
}

/**
 * PromQL series labels as people read them: `{instance="web-1",job="node"}`
 * shows as `web-1` when every series has the same job. What all of them share
 * says nothing about which line is which.
 */
export function shortLabels(labels: string[]): string[] {
  const parsed = labels.map((l) => [...l.matchAll(/(\w+)="((?:[^"\\]|\\.)*)"/g)].map((m) => [m[1], m[2]]));
  if (labels.length < 2 || parsed.some((pairs) => pairs.length === 0)) return labels;
  const shared = (key: string, value: string) => parsed.every((pairs) => pairs.some(([k, v]) => k === key && v === value));
  return parsed.map((pairs, i) => {
    const distinct = pairs.filter(([k, v]) => !shared(k, v)).map(([, v]) => v);
    return distinct.length > 0 ? distinct.join(" · ") : labels[i];
  });
}

// ------------------------------------------------------------ import/export

/** root's own format: the dashboard as saved, minus what the server stamps. */
export function exportJson(dashboard: Dashboard): string {
  const { updatedAt: _updatedAt, ...rest } = dashboard;
  return JSON.stringify({ root: 1, ...rest }, null, 2);
}

export interface Imported {
  dashboard: Dashboard;
  format: "root" | "grafana";
  /** Panels left out, and why, for the import dialog to say so. */
  skipped: string[];
}

/**
 * A dashboard from pasted or uploaded JSON: root's own export, or a Grafana
 * one — the dashboard model itself, or the `{ dashboard: … }` wrapper the
 * Grafana API returns. PromQL targets run against `prometheusId`.
 */
export function parseImport(text: string, prometheusId: string | null): Imported {
  let json: unknown;
  try {
    json = JSON.parse(text);
  } catch {
    throw new Error("That is not JSON. Paste a dashboard export, or choose the file.");
  }
  if (!json || typeof json !== "object") throw new Error("That JSON is not a dashboard.");
  const object = json as Record<string, unknown>;

  if (object.root === 1 && Array.isArray(object.panels)) {
    const dashboard = object as unknown as Dashboard;
    // A fresh id, so importing a colleague's export never overwrites yours.
    return {
      dashboard: { ...dashboard, id: newId(), panels: dashboard.panels.map((p) => ({ ...p, id: newId() })) },
      format: "root",
      skipped: [],
    };
  }

  const grafana = (object.dashboard && typeof object.dashboard === "object" ? object.dashboard : object) as Record<
    string,
    unknown
  >;
  if (!Array.isArray(grafana.panels)) {
    throw new Error("No panels in that JSON — is it a dashboard export?");
  }
  return fromGrafana(grafana, prometheusId);
}

interface GrafanaPanel {
  type?: string;
  title?: string;
  gridPos?: { x?: number; y?: number; w?: number; h?: number };
  targets?: { expr?: string; refId?: string; hide?: boolean }[];
  panels?: GrafanaPanel[];
  fieldConfig?: { defaults?: { unit?: string } };
  /** The pre-7 graph panel kept its unit on the left axis. */
  yaxes?: { format?: string }[];
}

/** Grafana unit ids root can draw. Anything else stays a plain number. */
const GRAFANA_UNITS: Record<string, MetricUnit> = {
  percent: "percent",
  bytes: "bytes",
  decbytes: "bytes",
  Bps: "bytes_per_second",
  binBps: "bytes_per_second",
  s: "seconds",
  ms: "millis",
  reqps: "per_second",
  ops: "per_second",
  rps: "per_second",
  wps: "per_second",
  iops: "per_second",
};

/** Grafana's built-in interval variables, as fixed values root can run. */
function concrete(expr: string): string {
  return expr
    .replace(/\$__rate_interval|\$\{__rate_interval\}/g, "5m")
    .replace(/\$__interval|\$\{__interval\}/g, "1m")
    .replace(/\$__range|\$\{__range\}/g, "1h");
}

const STAT_TYPES = new Set(["stat", "singlestat", "gauge", "bargauge"]);
const SERIES_TYPES = new Set(["timeseries", "graph"]);

export function fromGrafana(grafana: Record<string, unknown>, prometheusId: string | null): Imported {
  // Collapsed rows carry their panels inside them; expanded rows carry none.
  const flat: GrafanaPanel[] = [];
  for (const panel of grafana.panels as GrafanaPanel[]) {
    if (panel.type === "row") flat.push(...(panel.panels ?? []));
    else flat.push(panel);
  }
  flat.sort((a, b) => (a.gridPos?.y ?? 0) - (b.gridPos?.y ?? 0) || (a.gridPos?.x ?? 0) - (b.gridPos?.x ?? 0));

  const skipped: string[] = [];
  const panels: DashboardPanel[] = [];
  for (const panel of flat) {
    const title = panel.title?.trim() || "Untitled panel";
    const type = panel.type ?? "graph";
    if (!STAT_TYPES.has(type) && !SERIES_TYPES.has(type)) {
      skipped.push(`${title} — a ${type} panel, which root does not draw yet`);
      continue;
    }
    const target = (panel.targets ?? []).find((t) => !t.hide && t.expr?.trim());
    if (!target?.expr) {
      skipped.push(`${title} — no PromQL query`);
      continue;
    }
    // Grafana's grid is 24 columns, and rows of 30px with 8px between them;
    // root's is 12 columns, and rows of 80px with 12px between. Heights match
    // in pixels: 38h - 8 there against 92h - 12 here.
    panels.push({
      id: newId(),
      title,
      kind: STAT_TYPES.has(type) ? "stat" : "timeseries",
      w: Math.max(2, Math.min(COLUMNS, Math.round((panel.gridPos?.w ?? 12) / 2))),
      h: Math.max(1, Math.min(MAX_ROWS, Math.round((38 * (panel.gridPos?.h ?? 8) + 4) / 92))),
      query: { source: "promql", profileId: prometheusId ?? "", expr: concrete(target.expr) },
      unit: GRAFANA_UNITS[panel.fieldConfig?.defaults?.unit ?? panel.yaxes?.[0]?.format ?? ""],
    });
  }
  return {
    dashboard: {
      id: newId(),
      title: typeof grafana.title === "string" && grafana.title.trim() ? grafana.title : "Imported dashboard",
      panels,
      variables: {},
    },
    format: "grafana",
    skipped,
  };
}
