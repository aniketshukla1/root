import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MotionConfig } from "motion/react";

import { listen } from "./events";

import { api, errorMessage } from "./api";
import { Catalog, EMPTY_CATALOG } from "./catalog";
import { buildModel } from "./model";
import { leadIncident } from "./incident";
import type { Entity, Issue } from "./model";
import type { ExplainTarget } from "./explain";
import type { View } from "./nav";
import type {
  Alert,
  AppConfig,
  ChangeEvent,
  ConnectionStatusEvent,
  Correlation,
  DashboardState,
  HealthSnapshot,
  HealthRun,
  HistogramBucket,
  Incident,
  LogEvent,
  MetricSeries,
  PendingConfirmation,
  TopologyView,
} from "./types";
import { GLOBAL_STREAMS } from "./types";

import { CommandPalette } from "./shell/CommandPalette";
import { ConfirmDialog } from "./shell/ConfirmDialog";
import { AssistantDock } from "./shell/AssistantDock";
import { ComponentDialog } from "./shell/ComponentDialog";
import { Sidebar } from "./shell/Sidebar";
import { Toasts, toastFromAlert, type Toast } from "./shell/Toasts";

import { Alerts } from "./screens/Alerts";
import { Assistant } from "./screens/Assistant";
import { ComponentDetail } from "./screens/ComponentDetail";
import { Correlate } from "./screens/Correlate";
import { Logs } from "./screens/Logs";
import { Metrics } from "./screens/Metrics";
import { Overview } from "./screens/Overview";
import { ServiceDetail } from "./screens/ServiceDetail";
import { Services } from "./screens/Services";
import { Settings } from "./screens/Settings";

/** Mirror of the backend ring buffer; the UI never needs more than this. */
const CLIENT_LOG_CAPACITY = 5000;

/** Backstop refresh — events drive the UI, this catches anything missed. */
const DASHBOARD_POLL_MS = 5000;

/** Series and histograms are coarser than the live snapshot stream. */
const HISTORY_POLL_MS = 15000;

const HISTOGRAM_BUCKETS = 64;
const CONVERSATION_ID = "main";

/** Until the first fetch lands, an estate with no declared edges — not a lie. */
const EMPTY_TOPOLOGY: TopologyView = { dependsOn: {}, dependents: {}, roots: [] };

export default function App() {
  const [view, setView] = useState<View>({ name: "overview" });
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem("root-sidebar-collapsed") === "true";
    } catch {
      return false;
    }
  });

  const [dashboard, setDashboard] = useState<DashboardState | null>(null);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [catalog, setCatalog] = useState<Catalog>(EMPTY_CATALOG);
  const [logs, setLogs] = useState<LogEvent[]>([]);
  const [history, setHistory] = useState<Record<string, MetricSeries[]>>({});
  const [buckets, setBuckets] = useState<HistogramBucket[]>([]);
  const [healthRuns, setHealthRuns] = useState<Record<string, HealthRun[]>>({});
  const [incidents, setIncidents] = useState<Incident[]>([]);
  const [changes, setChanges] = useState<ChangeEvent[]>([]);
  const [topology, setTopology] = useState<TopologyView>(EMPTY_TOPOLOGY);
  const [onsetCorrelation, setOnsetCorrelation] = useState<Correlation | undefined>();
  /** The dock is one surface with a subject, not a second chat window. */
  const [explain, setExplain] = useState<ExplainTarget | null>(null);
  const askAssistant = useCallback(() => setExplain({ kind: "ask" }), []);
  const explainAlert = useCallback((alert: Alert) => setExplain({ kind: "alert", alert }), []);
  const explainLog = useCallback((event: LogEvent) => setExplain({ kind: "log", event }), []);
  const explainIncident = useCallback(
    (incident: Incident) => setExplain({ kind: "incident", incident }),
    [],
  );

  /** What a cell in the heatmap does when you click it. */
  const [inspecting, setInspecting] = useState<{ entity: Entity; issue?: Issue } | null>(null);

  const openComponent = useCallback((entity: Entity) => {
    setInspecting(null);
    // A component with no section or row of its own — a connection that failed
    // to collect at all — has nothing deeper to open, so it lands on its
    // connection instead of a route that cannot resolve.
    setView(
      entity.sectionId && entity.rowId
        ? {
            name: "component",
            profileId: entity.profileId,
            sectionId: entity.sectionId,
            rowId: entity.rowId,
          }
        : { name: "service", profileId: entity.profileId },
    );
  }, []);

  const selectComponent = useCallback(
    (entity: Entity, issue?: Issue) => {
      // Healthy things go straight through. Opening a dialog to say "this is
      // fine" would spend the click that was meant to be fast.
      if (entity.health === "healthy") openComponent(entity);
      else setInspecting({ entity, issue });
    },
    [openComponent],
  );
  const [minutes, setMinutes] = useState(60);
  const [confirmations, setConfirmations] = useState<PendingConfirmation[]>([]);
  const [connectionErrors, setConnectionErrors] = useState<ConnectionStatusEvent[]>([]);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [loading, setLoading] = useState(true);

  // Alert ids already toasted, so a still-firing alert does not re-notify on
  // every dashboard refresh.
  const notified = useRef<Set<string>>(new Set());

  useEffect(() => {
    try {
      localStorage.setItem("root-sidebar-collapsed", String(collapsed));
    } catch {
      // Navigation still works when storage is unavailable.
    }
  }, [collapsed]);

  const refreshDashboard = useCallback(async () => {
    try {
      setDashboard(await api.getDashboard());
    } catch (error) {
      console.error("dashboard refresh failed:", errorMessage(error));
    } finally {
      setLoading(false);
    }
  }, []);

  const refreshConfig = useCallback(async () => {
    try {
      setConfig(await api.getConfig());
    } catch (error) {
      console.error("config load failed:", errorMessage(error));
    }
  }, []);

  const refreshHistory = useCallback(async () => {
    try {
      setHistory(await api.getAllMetricHistory(minutes));
    } catch (error) {
      console.error("history load failed:", errorMessage(error));
    }
  }, [minutes]);

  const refreshHealthRuns = useCallback(async () => {
    try {
      setHealthRuns(await api.getHealthRuns(minutes));
    } catch (error) {
      console.error("health runs load failed:", errorMessage(error));
    }
  }, [minutes]);

  /**
   * Incidents are derived by the backend on every call rather than stored, so
   * this is a read of the present rather than a sync — there is nothing to
   * reconcile and nothing to go stale. It rides the dashboard cadence because
   * an incident IS the alert set, and the two disagreeing on screen for five
   * seconds is exactly the kind of flicker that costs trust.
   */
  const refreshIncidents = useCallback(async () => {
    try {
      setIncidents(await api.getIncidents());
    } catch (error) {
      console.error("incident load failed:", errorMessage(error));
    }
  }, []);

  const refreshContext = useCallback(async () => {
    try {
      const [nextChanges, nextTopology] = await Promise.all([
        api.getChangeEvents(undefined, Math.max(minutes, 180)),
        api.getTopology(),
      ]);
      setChanges(nextChanges);
      setTopology(nextTopology);
    } catch (error) {
      console.error("incident context load failed:", errorMessage(error));
    }
  }, [minutes]);

  const refreshHistogram = useCallback(async () => {
    try {
      setBuckets(
        await api.logHistogram({ streams: GLOBAL_STREAMS }, minutes, HISTOGRAM_BUCKETS),
      );
    } catch (error) {
      console.error("histogram load failed:", errorMessage(error));
    }
  }, [minutes]);

  // Initial load.
  useEffect(() => {
    refreshDashboard();
    refreshConfig();
    refreshIncidents();
    api
      .getServiceCatalog()
      .then((descriptors) => setCatalog(new Catalog(descriptors)))
      .catch((e) => console.error("service catalog load failed:", errorMessage(e)));
    api
      .queryLogs({ limit: 1000 })
      .then(setLogs)
      .catch((e) => console.error("log load failed:", errorMessage(e)));
    api.pendingConfirmations().then(setConfirmations).catch(() => undefined);
  }, [refreshDashboard, refreshConfig, refreshIncidents]);

  // Re-runs whenever the window changes, which is what makes the range
  // controls on Metrics and Logs actually refetch rather than just re-slice.
  useEffect(() => {
    refreshHistory();
    refreshHealthRuns();
    refreshHistogram();
    refreshContext();
  }, [refreshHistory, refreshHealthRuns, refreshHistogram, refreshContext]);

  useEffect(() => {
    const dashboardTimer = setInterval(() => {
      refreshDashboard();
      refreshIncidents();
    }, DASHBOARD_POLL_MS);
    const historyTimer = setInterval(() => {
      refreshHistory();
      refreshHealthRuns();
      refreshHistogram();
      refreshContext();
    }, HISTORY_POLL_MS);
    return () => {
      clearInterval(dashboardTimer);
      clearInterval(historyTimer);
    };
  }, [
    refreshDashboard,
    refreshIncidents,
    refreshHistory,
    refreshHealthRuns,
    refreshHistogram,
    refreshContext,
  ]);

  /**
   * What moved when the incident started, from the same correlation the
   * Correlate screen uses. Keyed on the onset rather than the incident id so
   * it is fetched once per actual event: members joining an incident do not
   * change what happened at 14:04.
   */
  const onsetAt = leadIncident(incidents)?.started;
  useEffect(() => {
    if (!onsetAt) {
      setOnsetCorrelation(undefined);
      return;
    }
    let live = true;
    api
      .correlate(onsetAt)
      .then((result) => {
        if (live) setOnsetCorrelation(result);
      })
      .catch((e) => console.error("onset correlation failed:", errorMessage(e)));
    return () => {
      live = false;
    };
  }, [onsetAt]);

  // ⌘K for the palette, ⌘J for the assistant — the two things you reach for
  // without deciding to, from anywhere.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      const key = event.key.toLowerCase();
      if (key === "k") {
        event.preventDefault();
        setPaletteOpen((open) => !open);
      }
      if (key === "j") {
        event.preventDefault();
        // Toggling closed keeps the subject, so reopening returns to it.
        setExplain((current) => (current ? null : { kind: "ask" }));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Live updates pushed from the collectors and the agent.
  useEffect(() => {
    const subscriptions = [
      listen<HealthSnapshot>("monitor://snapshot", (event) => {
        setDashboard((current) => {
          if (!current) return current;
          const snapshots = current.snapshots.map((s) =>
            s.profileId === event.payload.profileId ? event.payload : s,
          );
          // A connection polling for the first time is not in the list yet;
          // the periodic refresh places it properly.
          if (!snapshots.some((s) => s.profileId === event.payload.profileId)) {
            snapshots.push(event.payload);
          }
          return { ...current, snapshots };
        });
        setConnectionErrors((errors) =>
          errors.filter((e) => e.profileId !== event.payload.profileId),
        );
      }),

      listen<LogEvent[]>("monitor://logs", (event) => {
        // Batches arrive oldest-first; the buffer is newest-first.
        const incoming = [...event.payload].reverse();
        setLogs((current) => [...incoming, ...current].slice(0, CLIENT_LOG_CAPACITY));
      }),

      listen<Alert[]>("monitor://alerts", (event) => {
        refreshDashboard();
        // Only newly raised alerts arrive here, but guard anyway so a reconnect
        // cannot replay a wall of notifications.
        const fresh = event.payload.filter((alert) => !notified.current.has(alert.id));
        for (const alert of fresh) notified.current.add(alert.id);
        const worth = fresh.filter((a) => a.severity !== "info");
        if (worth.length > 0) {
          setToasts((current) => [
            ...current,
            ...worth.map((alert) =>
              toastFromAlert(alert, () =>
                setView({ name: "service", profileId: alert.profileId }),
              ),
            ),
          ]);
        }
      }),

      listen<ConnectionStatusEvent>("monitor://connection-status", (event) => {
        setConnectionErrors((errors) => [
          ...errors.filter((e) => e.profileId !== event.payload.profileId),
          event.payload,
        ]);
      }),

      listen<PendingConfirmation>("agent://confirm-request", (event) => {
        setConfirmations((current) => [...current, event.payload]);
      }),

      listen<{ callId: string }>("agent://confirm-resolved", (event) => {
        setConfirmations((current) => current.filter((c) => c.callId !== event.payload.callId));
      }),
    ];

    return () => {
      subscriptions.forEach((subscription) => {
        subscription.then((unlisten) => unlisten());
      });
    };
  }, [refreshDashboard]);

  const model = useMemo(
    () => buildModel(dashboard, config, catalog),
    [dashboard, config, catalog],
  );

  const refreshAll = useCallback(() => {
    refreshDashboard();
    refreshHistory();
    refreshHealthRuns();
    refreshHistogram();
  }, [refreshDashboard, refreshHistory, refreshHealthRuns, refreshHistogram]);

  const acknowledge = useCallback(
    (alertId: string) => {
      api
        .acknowledgeAlert(alertId)
        .then(() => refreshDashboard())
        .catch((error) => console.error("acknowledge failed:", errorMessage(error)));
    },
    [refreshDashboard],
  );

  const onConfigChange = useCallback(
    (next: AppConfig) => {
      setConfig(next);
      // Enabling, disabling, or removing a connection changes the dashboard.
      refreshAll();
    },
    [refreshAll],
  );

  const dismissToast = useCallback((id: string) => {
    setToasts((current) => current.filter((t) => t.id !== id));
  }, []);

  return (
    // `reducedMotion="user"` is resolved once here, so no variant has to
    // re-check the preference and no plot can forget to.
    <MotionConfig reducedMotion="user">
    <div className="flex h-screen w-screen overflow-hidden bg-page">
      <Sidebar
        view={view}
        model={model}
        collapsed={collapsed}
        onNavigate={setView}
        onToggleCollapsed={() => setCollapsed((c) => !c)}
        onOpenPalette={() => setPaletteOpen(true)}
      />

      <main className="flex min-w-0 flex-1 flex-col">
        {connectionErrors.length > 0 && view.name !== "settings" && (
          <div className="shrink-0 border-b border-line px-4 py-2">
            {connectionErrors.map((error) => (
              <p key={error.profileId} className="text-xs">
                <span className="font-medium text-crit">{error.profileName} could not start.</span>{" "}
                <span className="text-ink-2">{error.message}</span>
              </p>
            ))}
          </div>
        )}

        {view.name === "overview" && (
          <Overview
            model={model}
            catalog={catalog}
            incidents={incidents}
            changes={changes}
            topology={topology}
            correlation={onsetCorrelation}
            buckets={buckets}
            healthRuns={healthRuns}
            history={history}
            logs={logs}
            loading={loading}
            minutes={minutes}
            syncedAt={dashboard?.generatedAt}
            onNavigate={setView}
            onRefresh={refreshAll}
            onAcknowledge={acknowledge}
            onAsk={askAssistant}
            onAskIncident={explainIncident}
            onExplain={explainAlert}
            onSelectComponent={selectComponent}
          />
        )}

        {view.name === "services" && (
          <Services
            model={model}
            catalog={catalog}
            history={history}
            syncedAt={dashboard?.generatedAt}
            onNavigate={setView}
            onRefresh={refreshAll}
            onAsk={askAssistant}
          />
        )}

        {view.name === "alerts" && (
          <Alerts
            alerts={dashboard?.alerts ?? []}
            model={model}
            catalog={catalog}
            syncedAt={dashboard?.generatedAt}
            onNavigate={setView}
            onRefresh={refreshDashboard}
            onAcknowledge={acknowledge}
            onAsk={askAssistant}
            onExplain={explainAlert}
          />
        )}

        {view.name === "metrics" && (
          <Metrics
            history={history}
            model={model}
            catalog={catalog}
            minutes={minutes}
            onMinutesChange={setMinutes}
            syncedAt={dashboard?.generatedAt}
            onNavigate={setView}
            onRefresh={refreshAll}
            onAsk={askAssistant}
          />
        )}

        {view.name === "logs" && (
          <Logs
            logs={logs}
            buckets={buckets}
            model={model}
            catalog={catalog}
            minutes={minutes}
            onMinutesChange={setMinutes}
            onClear={() => {
              api.clearLogs().catch(() => undefined);
              setLogs([]);
              refreshHistogram();
            }}
            onNavigate={setView}
            onAsk={askAssistant}
            onExplain={explainLog}
          />
        )}

        {view.name === "service" && (
          <ServiceDetail
            profileId={view.profileId}
            tab={view.tab}
            model={model}
            catalog={catalog}
            logs={logs}
            history={history[view.profileId] ?? []}
            onNavigate={setView}
            onRefresh={refreshAll}
            onAcknowledge={acknowledge}
            onAsk={askAssistant}
          />
        )}

        {view.name === "component" && (
          <ComponentDetail
            profileId={view.profileId}
            sectionId={view.sectionId}
            rowId={view.rowId}
            model={model}
            catalog={catalog}
            logs={logs}
            onNavigate={setView}
            onAcknowledge={acknowledge}
            onRefresh={refreshAll}
          />
        )}

        {view.name === "correlate" && (
          <Correlate at={view.at} catalog={catalog} onNavigate={setView} />
        )}

        {view.name === "assistant" && (
          <Assistant
            config={config}
            conversationId={CONVERSATION_ID}
            onOpenSettings={() => setView({ name: "settings" })}
          />
        )}

        {view.name === "settings" && (
          <Settings
            config={config}
            catalog={catalog}
            onConfigChange={onConfigChange}
            onAsk={askAssistant}
          />
        )}
      </main>

      {/* Docked, not overlaid: the screen the problem is on stays visible and
          keeps its alignment while the answer arrives beside it. */}
      {explain && (
        <AssistantDock
          target={explain}
          configured={Boolean(config?.llm.hasApiKey || config?.llm.provider === "ollama")}
          onClose={() => setExplain(null)}
          onExpand={() => {
            setExplain(null);
            setView({ name: "assistant" });
          }}
        />
      )}

      {inspecting && (
        <ComponentDialog
          entity={inspecting.entity}
          issue={inspecting.issue}
          catalog={catalog}
          onClose={() => setInspecting(null)}
          onOpenComponent={() => openComponent(inspecting.entity)}
          onExplain={() => {
            const alert = inspecting.issue?.alert;
            setInspecting(null);
            if (alert) setExplain({ kind: "alert", alert });
            else setExplain({ kind: "ask" });
          }}
          onAcknowledge={acknowledge}
        />
      )}

      {paletteOpen && (
        <CommandPalette
          model={model}
          catalog={catalog}
          onNavigate={setView}
          onRefresh={refreshAll}
          onClose={() => setPaletteOpen(false)}
        />
      )}

      <Toasts toasts={toasts} onDismiss={dismissToast} />

      {/* One approval at a time, oldest first — batching them would invite
          rubber-stamping. */}
      {confirmations.length > 0 && (
        <ConfirmDialog
          request={confirmations[0]}
          onResolved={(callId) =>
            setConfirmations((current) => current.filter((c) => c.callId !== callId))
          }
        />
      )}
    </div>
    </MotionConfig>
  );
}
