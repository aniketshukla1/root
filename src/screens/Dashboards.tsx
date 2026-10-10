// Dashboards people build. A list, and one dashboard: panels on a twelve-
// column grid drawing either a metric root already collects or a PromQL query
// against a Prometheus connection; a variable bar (environment, team,
// service) that re-targets every panel naming `$service`; one crosshair
// across all of them; and an edit mode that adds, arranges and resizes.
// Grafana exports import as PromQL panels — see `dashboards.ts`.

import { useEffect, useMemo, useState, type ReactNode } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { DATA_INK, seriesColor } from "../catalog";
import {
  exportJson,
  movePanel,
  newDashboard,
  newPanel,
  parseImport,
  resizePanel,
  resolveProfile,
  shortLabels,
  type Imported,
} from "../dashboards";
import { formatMetricValue, formatRelative } from "../format";
import type { EstateModel, ServiceView } from "../model";
import type { View } from "../nav";
import { rangesAcross, type Range } from "../ranges";
import type { AppConfig, Dashboard, DashboardPanel, MetricSeries, MetricUnit, PanelQuery } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Sparkline, TimeSeries, useWidth, type Domain, type Series } from "../ui/charts";
import { Button, cx, Empty, Input, Segmented, Select, ServiceMark } from "../ui/primitives";

/** One grid row, in pixels; a panel `h` rows tall is h of these plus the gaps. */
const ROW_PX = 80;
const GAP_PX = 12;

type DashboardsView = Extract<View, { name: "dashboards" } | { name: "dashboard" }>;

interface Shared {
  config: AppConfig | null;
  model: EstateModel;
  catalog: Catalog;
  history: Record<string, MetricSeries[]>;
  minutes: number;
  ranges: Range[];
  onMinutesChange: (minutes: number) => void;
  onConfigChange: (config: AppConfig) => void;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAsk: () => void;
  syncedAt?: string;
}

export function Dashboards({ view, ...props }: Omit<Shared, "ranges"> & { view: DashboardsView }) {
  const pollIntervals = props.model.services.map((s) => s.profile?.pollIntervalSecs ?? 15);
  const ranges = useMemo(() => rangesAcross(pollIntervals), [pollIntervals.join()]);
  const shared: Shared = { ...props, ranges };
  const dashboards = shared.config?.dashboards ?? [];
  if (view.name === "dashboard") {
    const dashboard = dashboards.find((d) => d.id === view.dashboardId);
    if (!dashboard) {
      return (
        <>
          <ScreenHeader title="Dashboards" onAsk={shared.onAsk} />
          <Screen>
            <Empty
              title="That dashboard is not here"
              hint="It may have been deleted, or the link came from another server."
            />
            <div className="mt-3">
              <Button onClick={() => shared.onNavigate({ name: "dashboards" })}>All dashboards</Button>
            </div>
          </Screen>
        </>
      );
    }
    return (
      <DashboardView
        key={dashboard.id}
        dashboard={dashboard}
        linkVariables={view.variables}
        startEditing={view.edit ?? false}
        {...shared}
      />
    );
  }
  return <DashboardList dashboards={dashboards} {...shared} />;
}

// ------------------------------------------------------------------- list

function DashboardList({ dashboards, ...shared }: Shared & { dashboards: Dashboard[] }) {
  const [importing, setImporting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  const create = async () => {
    setBusy(true);
    setError(undefined);
    try {
      const dashboard = newDashboard();
      shared.onConfigChange(await api.saveDashboard(dashboard));
      shared.onNavigate({ name: "dashboard", dashboardId: dashboard.id, edit: true });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <ScreenHeader
        title="Dashboards"
        meta={`${dashboards.length} saved`}
        syncedAt={shared.syncedAt}
        onRefresh={shared.onRefresh}
        onAsk={shared.onAsk}
      >
        <Button onClick={() => setImporting(true)}>Import</Button>
        <Button variant="primary" loading={busy} onClick={() => void create()}>
          New dashboard
        </Button>
      </ScreenHeader>
      <Screen className="flex flex-col gap-3">
        {error && <p className="text-xs text-crit">{error}</p>}
        {dashboards.length === 0 ? (
          <Empty
            title="No dashboards yet"
            hint="Build one from the metrics root collects, or import a Grafana export — its PromQL panels run against your Prometheus connection."
          />
        ) : (
          <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(260px,1fr))]">
            {[...dashboards]
              .sort((a, b) => a.title.localeCompare(b.title))
              .map((d) => (
                <button
                  key={d.id}
                  type="button"
                  onClick={() => shared.onNavigate({ name: "dashboard", dashboardId: d.id })}
                  className="panel row-hover flex flex-col gap-2 p-3 text-left"
                >
                  <span className="truncate text-base text-ink">{d.title}</span>
                  <MiniLayout panels={d.panels} />
                  <span className="num text-2xs text-ink-4">
                    {d.panels.length} panel{d.panels.length === 1 ? "" : "s"}
                    {d.updatedAt && ` · saved ${formatRelative(d.updatedAt)}`}
                  </span>
                </button>
              ))}
          </div>
        )}
      </Screen>
      {importing && (
        <ImportDialog
          model={shared.model}
          onCancel={() => setImporting(false)}
          onImported={(config, id) => {
            setImporting(false);
            shared.onConfigChange(config);
            shared.onNavigate({ name: "dashboard", dashboardId: id });
          }}
        />
      )}
    </>
  );
}

/** A dashboard's layout in miniature, so a card shows its shape at a glance. */
function MiniLayout({ panels }: { panels: DashboardPanel[] }) {
  if (panels.length === 0) return <div className="h-12 rounded-sm border border-dashed border-line" />;
  return (
    <div className="grid h-12 gap-[3px] overflow-hidden [grid-auto-rows:10px] [grid-template-columns:repeat(12,minmax(0,1fr))]">
      {panels.slice(0, 24).map((p) => (
        <span
          key={p.id}
          className="rounded-[2px] bg-line-strong"
          style={{ gridColumn: `span ${p.w} / span ${p.w}`, gridRow: `span ${Math.min(p.h, 3)} / span ${Math.min(p.h, 3)}` }}
        />
      ))}
    </div>
  );
}

// -------------------------------------------------------------- one board

function DashboardView({
  dashboard,
  linkVariables,
  startEditing,
  ...shared
}: Shared & {
  dashboard: Dashboard;
  linkVariables?: Record<string, string>;
  startEditing: boolean;
}) {
  const { model, catalog, history, minutes } = shared;
  const [editing, setEditing] = useState(startEditing);
  const [draft, setDraft] = useState<Dashboard>(dashboard);
  const [variables, setVariables] = useState<Record<string, string>>(() => ({
    ...dashboard.variables,
    ...linkVariables,
  }));
  const [editingPanel, setEditingPanel] = useState<DashboardPanel | "new" | null>(null);
  const [dragging, setDragging] = useState<number | null>(null);
  const [hoverTime, setHoverTime] = useState<number | null>(null);
  const [zoom, setZoom] = useState<Domain | null>(null);
  const [tick, setTick] = useState(0);
  const [notice, setNotice] = useState<string>();
  const [error, setError] = useState<string>();
  const [saving, setSaving] = useState(false);
  const [ref, width] = useWidth<HTMLDivElement>();

  // The saved copy changed underneath (another tab, another person): take it
  // unless this one is mid-edit.
  useEffect(() => {
    if (!editing) setDraft(dashboard);
  }, [dashboard, editing]);

  // PromQL panels refetch on this; root's own metrics refresh with the app.
  useEffect(() => {
    const timer = setInterval(() => setTick((t) => t + 1), 30_000);
    return () => clearInterval(timer);
  }, []);

  const domain = useMemo<Domain>(() => {
    const now = Date.now();
    return zoom ?? [now - minutes * 60_000, now];
    // `history` moves the live window forward with each refresh.
  }, [minutes, history, zoom, tick]);

  const environments = unique(model.services.map((s) => s.ownership.environment));
  const teams = unique(model.services.map((s) => s.ownership.team));
  const services = model.services.filter(
    (s) =>
      (!variables.environment || s.ownership.environment === variables.environment) &&
      (!variables.team || s.ownership.team === variables.team),
  );
  // A service the narrowing no longer includes falls back to the first one
  // that it does, so `$service` panels never point at nothing silently.
  const service =
    services.find((s) => s.snapshot.profileId === variables.service)?.snapshot.profileId ??
    services[0]?.snapshot.profileId ??
    "";
  const resolved = { ...variables, service };
  const usesService = draft.panels.some((p) => p.query.profileId === "$service");

  // Half as many columns' worth of space below 900px: a 3-wide panel at 760px
  // would be narrower than its own value.
  const span = (w: number) => (width > 0 && width < 900 ? Math.min(12, w * 2) : w);

  const update = (next: Dashboard) => setDraft(next);
  const save = async () => {
    setSaving(true);
    setError(undefined);
    try {
      shared.onConfigChange(await api.saveDashboard({ ...draft, variables: resolved }));
      setEditing(false);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSaving(false);
    }
  };
  const remove = async () => {
    if (!window.confirm(`Delete "${draft.title}"? This cannot be undone.`)) return;
    try {
      shared.onConfigChange(await api.deleteDashboard(draft.id));
      shared.onNavigate({ name: "dashboards" });
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  const copy = async (text: string, what: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setNotice(`${what} copied`);
    } catch {
      setNotice(`Could not reach the clipboard — ${what.toLowerCase()} not copied`);
    }
    setTimeout(() => setNotice(undefined), 2500);
  };
  const link = () => {
    const params = new URLSearchParams({ dashboard: draft.id });
    for (const [k, v] of Object.entries(resolved)) if (v) params.set(k, v);
    return `${window.location.origin}${window.location.pathname}${window.location.search}#${params.toString()}`;
  };

  return (
    <>
      <ScreenHeader
        title={
          editing ? (
            <Input
              value={draft.title}
              onChange={(e) => update({ ...draft, title: e.target.value })}
              aria-label="Dashboard title"
              className="w-[min(340px,40vw)]"
            />
          ) : (
            draft.title
          )
        }
        meta={`${draft.panels.length} panel${draft.panels.length === 1 ? "" : "s"}${notice ? ` · ${notice}` : ""}`}
        syncedAt={shared.syncedAt}
        onRefresh={() => {
          setTick((t) => t + 1);
          shared.onRefresh();
        }}
        onAsk={shared.onAsk}
      >
        {editing ? (
          <>
            <Button variant="ghost" onClick={() => void remove()}>
              Delete
            </Button>
            <Button
              variant="ghost"
              onClick={() => {
                setDraft(dashboard);
                setEditing(false);
              }}
            >
              Cancel
            </Button>
            <Button onClick={() => setEditingPanel("new")}>Add panel</Button>
            <Button variant="primary" loading={saving} onClick={() => void save()}>
              Save
            </Button>
          </>
        ) : (
          <>
            <Button variant="ghost" onClick={() => shared.onNavigate({ name: "dashboards" })}>
              All dashboards
            </Button>
            <Button variant="ghost" onClick={() => void copy(exportJson(draft), "JSON")}>
              Copy JSON
            </Button>
            <Button variant="ghost" onClick={() => void copy(link(), "Link")}>
              Copy link
            </Button>
            <Button onClick={() => setEditing(true)}>Edit</Button>
          </>
        )}
      </ScreenHeader>

      <Screen className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2">
          {(usesService || editing) && (
            <>
              <VariableSelect
                label="Environment"
                value={variables.environment ?? ""}
                options={environments}
                onChange={(v) => setVariables({ ...variables, environment: v })}
              />
              <VariableSelect
                label="Team"
                value={variables.team ?? ""}
                options={teams}
                onChange={(v) => setVariables({ ...variables, team: v })}
              />
              <label className="flex items-center gap-1.5 text-2xs text-ink-4">
                Service
                <Select
                  size="sm"
                  value={service}
                  onChange={(e) => setVariables({ ...variables, service: e.target.value })}
                  aria-label="Service"
                >
                  {services.map((s) => (
                    <option key={s.snapshot.profileId} value={s.snapshot.profileId}>
                      {s.snapshot.profileName}
                    </option>
                  ))}
                </Select>
              </label>
            </>
          )}
          <span className="ml-auto flex items-center gap-2">
            {zoom && (
              <Button variant="ghost" onClick={() => setZoom(null)}>
                Reset zoom
              </Button>
            )}
            <Segmented
              label="Time range"
              value={String(minutes)}
              onChange={(v) => shared.onMinutesChange(Number(v))}
              options={shared.ranges.map((r) => ({ value: String(r.minutes), label: r.label }))}
            />
          </span>
        </div>
        {error && <p className="text-xs text-crit">{error}</p>}

        {draft.panels.length === 0 ? (
          <Empty
            title="An empty dashboard"
            hint={editing ? "Add a panel to start." : "Edit it to add panels."}
          />
        ) : (
          <div
            ref={ref}
            className="grid gap-3 [grid-template-columns:repeat(12,minmax(0,1fr))]"
            style={{ gridAutoRows: ROW_PX }}
          >
            {draft.panels.map((panel, index) => (
              <div
                key={panel.id}
                className={cx(
                  "panel flex min-w-0 flex-col overflow-hidden",
                  editing && dragging === index && "opacity-40",
                )}
                style={{
                  gridColumn: `span ${span(panel.w)} / span ${span(panel.w)}`,
                  gridRow: `span ${panel.h} / span ${panel.h}`,
                }}
                onDragOver={editing ? (e) => e.preventDefault() : undefined}
                onDrop={
                  editing
                    ? () => {
                        if (dragging !== null) update({ ...draft, panels: movePanel(draft.panels, dragging, index) });
                        setDragging(null);
                      }
                    : undefined
                }
              >
                <div className="flex h-[30px] shrink-0 items-center gap-1.5 border-b border-line px-2.5">
                  {editing && (
                    <span
                      draggable
                      // Firefox starts no drag without data on it.
                      onDragStart={(e) => {
                        e.dataTransfer.setData("text/plain", panel.id);
                        setDragging(index);
                      }}
                      onDragEnd={() => setDragging(null)}
                      // The keyboard way to do the same: arrows move it one place.
                      role="button"
                      tabIndex={0}
                      onKeyDown={(e) => {
                        const step = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[e.key];
                        if (!step) return;
                        e.preventDefault();
                        update({ ...draft, panels: movePanel(draft.panels, index, index + step) });
                      }}
                      className="cursor-grab rounded-sm text-ink-4 select-none focus-visible:outline focus-visible:outline-accent"
                      title="Drag to move, or use the arrow keys"
                      aria-label={`Move ${panel.title}: drag, or use the arrow keys`}
                    >
                      ⠿
                    </span>
                  )}
                  <span className="min-w-0 flex-1 truncate text-xs text-ink-3" title={panel.title}>
                    {panel.title}
                  </span>
                  {editing && (
                    <PanelControls
                      onResize={(dw, dh) =>
                        update({
                          ...draft,
                          panels: draft.panels.map((p) => (p.id === panel.id ? resizePanel(p, dw, dh) : p)),
                        })
                      }
                      onEdit={() => setEditingPanel(panel)}
                      onRemove={() => update({ ...draft, panels: draft.panels.filter((p) => p.id !== panel.id) })}
                    />
                  )}
                </div>
                <div className="min-h-0 flex-1 px-2.5 py-1.5">
                  <PanelBody
                    panel={panel}
                    profileId={resolveProfile(panel.query.profileId, resolved)}
                    history={history}
                    catalog={catalog}
                    model={model}
                    minutes={minutes}
                    domain={domain}
                    tick={tick}
                    height={panel.h * ROW_PX + (panel.h - 1) * GAP_PX - 30 - 12}
                    hoverTime={hoverTime}
                    onHoverTime={setHoverTime}
                    onBrush={setZoom}
                  />
                </div>
              </div>
            ))}
          </div>
        )}
      </Screen>

      {editingPanel && (
        <PanelEditor
          initial={
            editingPanel === "new"
              ? newPanel({ source: "metric", profileId: "$service", metricKey: "" }, "")
              : editingPanel
          }
          model={model}
          service={service}
          onCancel={() => setEditingPanel(null)}
          onSave={(panel) => {
            const exists = draft.panels.some((p) => p.id === panel.id);
            update({
              ...draft,
              panels: exists ? draft.panels.map((p) => (p.id === panel.id ? panel : p)) : [...draft.panels, panel],
            });
            setEditingPanel(null);
          }}
        />
      )}
    </>
  );
}

function unique(values: string[]): string[] {
  return [...new Set(values.filter(Boolean))].sort();
}

function VariableSelect({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: string[];
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex items-center gap-1.5 text-2xs text-ink-4">
      {label}
      <Select size="sm" value={value} onChange={(e) => onChange(e.target.value)} aria-label={label}>
        <option value="">All</option>
        {options.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </Select>
    </label>
  );
}

function PanelControls({
  onResize,
  onEdit,
  onRemove,
}: {
  onResize: (dw: number, dh: number) => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const control = (label: string, glyph: string, run: () => void) => (
    <button
      type="button"
      onClick={run}
      title={label}
      aria-label={label}
      className="grid h-5 w-5 place-items-center rounded-sm text-2xs text-ink-4 hover:bg-raised hover:text-ink"
    >
      {glyph}
    </button>
  );
  return (
    <span className="flex shrink-0 items-center text-2xs text-ink-4">
      <span className="ml-1">W</span>
      {control("Narrower", "−", () => onResize(-1, 0))}
      {control("Wider", "+", () => onResize(1, 0))}
      <span className="ml-1">H</span>
      {control("Shorter", "−", () => onResize(0, -1))}
      {control("Taller", "+", () => onResize(0, 1))}
      {control("Edit panel", "✎", onEdit)}
      {control("Remove panel", "×", onRemove)}
    </span>
  );
}

// ------------------------------------------------------------- panel body

function PanelBody({
  panel,
  profileId,
  history,
  catalog,
  model,
  minutes,
  domain,
  tick,
  height,
  hoverTime,
  onHoverTime,
  onBrush,
}: {
  panel: DashboardPanel;
  profileId: string | null;
  history: Record<string, MetricSeries[]>;
  catalog: Catalog;
  model: EstateModel;
  minutes: number;
  domain: Domain;
  tick: number;
  height: number;
  hoverTime: number | null;
  onHoverTime: (t: number | null) => void;
  onBrush: (range: Domain) => void;
}) {
  const promql = usePromql(
    panel.query.source === "promql" ? profileId : null,
    panel.query.source === "promql" ? panel.query.expr : "",
    minutes,
    tick,
    panel.query.source === "promql",
  );

  if (!profileId) {
    return <Hint>{panel.query.source === "promql" ? "Pick a Prometheus connection for this panel." : "No service selected."}</Hint>;
  }

  let found: MetricSeries[];
  if (panel.query.source === "metric") {
    const key = panel.query.metricKey;
    found = (history[profileId] ?? []).filter((s) => s.key === key && s.points.length > 1);
    if (found.length === 0) {
      const owner = model.services.find((s) => s.snapshot.profileId === profileId);
      const name = owner?.snapshot.profileName ?? profileId;
      const metric = owner?.snapshot.metrics.find((m) => m.key === key);
      return (
        <Hint>
          {metric ? `No history for ${metric.label} on ${name} yet.` : `${name} does not report ${key || "this metric"}.`}
        </Hint>
      );
    }
  } else {
    if (promql.error) return <Hint tone="crit">{promql.error}</Hint>;
    if (promql.loading && promql.series.length === 0) return <Hint>Querying…</Hint>;
    if (promql.series.length === 0) return <Hint>The query returned no series.</Hint>;
    found = promql.series;
  }

  if (panel.kind === "stat") {
    const first = found[0];
    const unit = panel.unit ?? first.unit;
    const over = first.warnAbove !== undefined && first.latest > first.warnAbove;
    return (
      <div className="flex h-full items-end justify-between gap-3">
        <span className={cx("num leading-none", over ? "text-warn" : "text-ink")} style={{ fontSize: Math.min(40, height * 0.45) }}>
          {formatMetricValue(first.latest, unit)}
        </span>
        <Sparkline points={first.points} width={96} height={Math.min(36, height - 8)} color={DATA_INK} warnAbove={first.warnAbove} />
      </div>
    );
  }

  const names = panel.query.source === "promql" ? shortLabels(found.map((s) => s.label)) : found.map((s) => s.label);
  const series: Series[] = found.map((s, i) => ({
    key: s.key,
    label: names[i],
    color: found.length === 1 ? DATA_INK : seriesColor(i),
    points: s.points,
    warnAbove: s.warnAbove,
    icon:
      panel.query.source === "metric" ? undefined : (
        <ServiceMark descriptor={catalog.get("prometheus")} size={12} />
      ),
  }));
  // A legend table only where it fits; the tooltip names every line anyway.
  const legendRows = series.length > 1 && height >= 220 ? series.length + 1 : 0;
  return (
    <TimeSeries
      series={series}
      unit={panel.unit ?? found[0].unit}
      domain={domain}
      height={Math.max(60, height - legendRows * 18)}
      showLegend={legendRows > 0}
      hoverTime={hoverTime}
      onHoverTime={onHoverTime}
      onBrush={onBrush}
    />
  );
}

function Hint({ children, tone }: { children: ReactNode; tone?: "crit" }) {
  return (
    <div className={cx("grid h-full place-items-center px-2 text-center text-2xs", tone === "crit" ? "text-crit" : "text-ink-4")}>
      <span className="line-clamp-3">{children}</span>
    </div>
  );
}

function usePromql(profileId: string | null, expr: string, minutes: number, tick: number, enabled: boolean) {
  const [state, setState] = useState<{ series: MetricSeries[]; loading: boolean; error?: string }>({
    series: [],
    loading: enabled,
  });
  useEffect(() => {
    if (!enabled) return;
    if (!profileId || !expr.trim()) {
      setState({ series: [], loading: false, error: expr.trim() ? undefined : "This panel has no query yet." });
      return;
    }
    let live = true;
    setState((s) => ({ ...s, loading: true }));
    api
      .queryPrometheus(profileId, expr, minutes)
      .then((series) => live && setState({ series, loading: false }))
      .catch((e) => live && setState({ series: [], loading: false, error: errorMessage(e) }));
    return () => {
      live = false;
    };
  }, [profileId, expr, minutes, tick, enabled]);
  return state;
}

// ----------------------------------------------------------- panel editor

function Modal({ title, children, onClose }: { title: string; children: ReactNode; onClose: () => void }) {
  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/60 p-4 backdrop-blur-sm"
      onClick={onClose}
      role="dialog"
      aria-modal
      aria-label={title}
    >
      <div className="panel w-full max-w-lg bg-overlay p-4 overlay-plate" onClick={(e) => e.stopPropagation()}>
        <h2 className="mb-3 text-sm font-semibold text-ink">{title}</h2>
        {children}
      </div>
    </div>
  );
}

const UNITS: { value: MetricUnit | ""; label: string }[] = [
  { value: "", label: "Plain number" },
  { value: "percent", label: "Percent (0–100)" },
  { value: "bytes", label: "Bytes" },
  { value: "bytes_per_second", label: "Bytes per second" },
  { value: "per_second", label: "Per second" },
  { value: "seconds", label: "Seconds" },
  { value: "millis", label: "Milliseconds" },
];

function PanelEditor({
  initial,
  model,
  service,
  onSave,
  onCancel,
}: {
  initial: DashboardPanel;
  model: EstateModel;
  /** What `$service` currently resolves to, for listing its metrics. */
  service: string;
  onSave: (panel: DashboardPanel) => void;
  onCancel: () => void;
}) {
  const [title, setTitle] = useState(initial.title);
  const [kind, setKind] = useState(initial.kind);
  const [unit, setUnit] = useState<MetricUnit | "">(initial.unit ?? "");
  const [query, setQuery] = useState<PanelQuery>(initial.query);
  const prometheus = model.services.filter((s) => s.snapshot.kind === "prometheus");

  const metricOwner: ServiceView | undefined =
    query.source === "metric"
      ? model.services.find((s) => s.snapshot.profileId === (query.profileId === "$service" ? service : query.profileId))
      : undefined;
  const metrics = metricOwner?.snapshot.metrics ?? [];
  const valid =
    query.source === "metric" ? Boolean(query.profileId && query.metricKey) : Boolean(query.profileId && query.expr.trim());
  // A panel named after what it shows, unless someone typed a name.
  const fallbackTitle =
    query.source === "metric"
      ? (metrics.find((m) => m.key === query.metricKey)?.label ?? "")
      : query.expr.trim();

  return (
    <Modal title={initial.title ? "Edit panel" : "Add panel"} onClose={onCancel}>
      <div className="flex flex-col gap-3">
        <label className="flex flex-col gap-1 text-2xs text-ink-4">
          Title
          <Input value={title} onChange={(e) => setTitle(e.target.value)} placeholder={fallbackTitle || "Panel title"} />
        </label>
        <div className="flex flex-wrap items-center gap-3">
          <Segmented
            label="Draw as"
            value={kind}
            onChange={setKind}
            options={[
              { value: "timeseries", label: "Time series" },
              { value: "stat", label: "Stat" },
            ]}
          />
          <Segmented
            label="Source"
            value={query.source}
            onChange={(source) =>
              setQuery(
                source === "metric"
                  ? { source: "metric", profileId: "$service", metricKey: "" }
                  : { source: "promql", profileId: prometheus[0]?.snapshot.profileId ?? "", expr: "" },
              )
            }
            options={[
              { value: "metric", label: "root metric" },
              { value: "promql", label: "PromQL" },
            ]}
          />
        </div>

        {query.source === "metric" ? (
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="flex flex-col gap-1 text-2xs text-ink-4">
              Connection
              <Select
                value={query.profileId}
                onChange={(e) => setQuery({ ...query, profileId: e.target.value, metricKey: "" })}
              >
                <option value="$service">$service — the dashboard&apos;s variable</option>
                {model.services.map((s) => (
                  <option key={s.snapshot.profileId} value={s.snapshot.profileId}>
                    {s.snapshot.profileName}
                  </option>
                ))}
              </Select>
            </label>
            <label className="flex flex-col gap-1 text-2xs text-ink-4">
              Metric
              <Select value={query.metricKey} onChange={(e) => setQuery({ ...query, metricKey: e.target.value })}>
                <option value="">Choose…</option>
                {metrics.map((m) => (
                  <option key={m.key} value={m.key}>
                    {m.label}
                  </option>
                ))}
              </Select>
            </label>
          </div>
        ) : (
          <div className="flex flex-col gap-3">
            <div className="grid gap-3 sm:grid-cols-2">
              <label className="flex flex-col gap-1 text-2xs text-ink-4">
                Prometheus connection
                <Select value={query.profileId} onChange={(e) => setQuery({ ...query, profileId: e.target.value })}>
                  {prometheus.length === 0 && <option value="">No Prometheus connection yet</option>}
                  {prometheus.map((s) => (
                    <option key={s.snapshot.profileId} value={s.snapshot.profileId}>
                      {s.snapshot.profileName}
                    </option>
                  ))}
                </Select>
              </label>
              <label className="flex flex-col gap-1 text-2xs text-ink-4">
                Unit
                <Select value={unit} onChange={(e) => setUnit(e.target.value as MetricUnit | "")}>
                  {UNITS.map((u) => (
                    <option key={u.value} value={u.value}>
                      {u.label}
                    </option>
                  ))}
                </Select>
              </label>
            </div>
            <label className="flex flex-col gap-1 text-2xs text-ink-4">
              PromQL
              <textarea
                value={query.expr}
                onChange={(e) => setQuery({ ...query, expr: e.target.value })}
                rows={3}
                spellCheck={false}
                placeholder="sum by (instance) (rate(node_cpu_seconds_total{mode!=&quot;idle&quot;}[5m]))"
                className="rounded-md border border-line bg-sunk p-2 font-mono text-xs text-ink focus:border-accent focus:outline-none"
              />
            </label>
          </div>
        )}

        <div className="mt-1 flex justify-end gap-2">
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={!valid}
            onClick={() =>
              onSave({
                ...initial,
                title: title.trim() || fallbackTitle || "Untitled panel",
                kind,
                query,
                // root's own metrics carry their unit; a PromQL number needs telling.
                unit: query.source === "promql" && unit ? unit : undefined,
              })
            }
          >
            {initial.title ? "Save panel" : "Add panel"}
          </Button>
        </div>
      </div>
    </Modal>
  );
}

// ----------------------------------------------------------------- import

function ImportDialog({
  model,
  onImported,
  onCancel,
}: {
  model: EstateModel;
  onImported: (config: AppConfig, dashboardId: string) => void;
  onCancel: () => void;
}) {
  const prometheus = model.services.filter((s) => s.snapshot.kind === "prometheus");
  const [text, setText] = useState("");
  const [prom, setProm] = useState(prometheus[0]?.snapshot.profileId ?? "");
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string>();

  // Parsed as typed, so what will happen is visible before it does.
  const parsed = useMemo((): { preview?: Imported; error?: string } => {
    if (!text.trim()) return {};
    try {
      return { preview: parseImport(text, prom || null) };
    } catch (e) {
      return { error: errorMessage(e) };
    }
  }, [text, prom]);

  const confirm = async () => {
    if (!parsed.preview) return;
    setBusy(true);
    setSaveError(undefined);
    try {
      onImported(await api.saveDashboard(parsed.preview.dashboard), parsed.preview.dashboard.id);
    } catch (e) {
      setSaveError(errorMessage(e));
      setBusy(false);
    }
  };

  return (
    <Modal title="Import a dashboard" onClose={onCancel}>
      <div className="flex flex-col gap-3">
        <p className="text-2xs leading-relaxed text-ink-4">
          A root export, or a Grafana one — the dashboard JSON or the API&apos;s wrapper around it.
          Grafana panels come in as PromQL panels against the connection below.
        </p>
        <textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={8}
          spellCheck={false}
          placeholder="Paste the JSON here"
          className="rounded-md border border-line bg-sunk p-2 font-mono text-2xs text-ink focus:border-accent focus:outline-none"
          aria-label="Dashboard JSON"
        />
        <div className="flex flex-wrap items-center gap-3">
          <label className="text-2xs text-ink-4">
            or a file{" "}
            <input
              type="file"
              accept=".json,application/json"
              className="text-2xs text-ink-3"
              onChange={(e) => {
                const file = e.target.files?.[0];
                if (file) void file.text().then(setText);
              }}
            />
          </label>
          <label className="ml-auto flex items-center gap-1.5 text-2xs text-ink-4">
            PromQL runs on
            <Select size="sm" value={prom} onChange={(e) => setProm(e.target.value)} aria-label="Prometheus connection">
              <option value="">{prometheus.length === 0 ? "No Prometheus connection yet" : "Choose later"}</option>
              {prometheus.map((s) => (
                <option key={s.snapshot.profileId} value={s.snapshot.profileId}>
                  {s.snapshot.profileName}
                </option>
              ))}
            </Select>
          </label>
        </div>
        {parsed.error && <p className="text-2xs text-crit">{parsed.error}</p>}
        {parsed.preview && (
          <div className="rounded-md border border-line bg-page p-2.5 text-2xs text-ink-3">
            <span className="text-ink">{parsed.preview.dashboard.title}</span> —{" "}
            {parsed.preview.dashboard.panels.length} panel{parsed.preview.dashboard.panels.length === 1 ? "" : "s"} from a{" "}
            {parsed.preview.format === "grafana" ? "Grafana" : "root"} export
            {parsed.preview.skipped.length > 0 && (
              <ul className="mt-1.5 list-disc pl-4 text-ink-4">
                {parsed.preview.skipped.map((s) => (
                  <li key={s}>Left out: {s}</li>
                ))}
              </ul>
            )}
          </div>
        )}
        {saveError && <p className="text-2xs text-crit">{saveError}</p>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!parsed.preview} loading={busy} onClick={() => void confirm()}>
            Import
          </Button>
        </div>
      </div>
    </Modal>
  );
}
