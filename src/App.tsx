import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, MotionConfig } from "motion/react";

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
  CapacityRisk,
  Finding,
  MetricSeries,
  Recall,
  RecordedVerdict,
  Verdict,
  PendingConfirmation,
  TeamState,
  TopologyView,
} from "./types";
import { GLOBAL_STREAMS } from "./types";

import { CommandPalette } from "./shell/CommandPalette";
import { ConfirmDialog } from "./shell/ConfirmDialog";
import { AssistantDock } from "./shell/AssistantDock";
import { ComponentDialog } from "./shell/ComponentDialog";
import { Sidebar } from "./shell/Sidebar";
import { Splash } from "./shell/Splash";
import { SEQUENCE_MS } from "./shell/splash/script";
import { Toasts, toastFromAlert, type Toast } from "./shell/Toasts";

import { Alerts } from "./screens/Alerts";
import { Assistant } from "./screens/Assistant";
import { ComponentDetail } from "./screens/ComponentDetail";
import { Correlate } from "./screens/Correlate";
import { Logs } from "./screens/Logs";
import { Metrics } from "./screens/Metrics";
import * as remote from "./remote";
import { Onboarding } from "./screens/Onboarding";
import { ServerSignIn } from "./screens/ServerSignIn";
import { SignIn } from "./screens/SignIn";
import { Overview } from "./screens/Overview";
import { Scorecard } from "./screens/Scorecard";
import { ServiceDetail } from "./screens/ServiceDetail";
import { TraceDetail } from "./screens/TraceDetail";
import { ProfileDetail } from "./screens/ProfileDetail";
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

/**
 * How long the boot screen is guaranteed, and how long it is allowed.
 *
 * The floor is the title sequence's own length, so the name always lands — a
 * splash that is cut off before its last beat is worse than none at all. A key
 * or a click ends it sooner once the app is ready. The ceiling exists because a
 * collector that never answers must not hold the whole app hostage: the estate
 * has its own vocabulary for a connection that has not reported, and it is
 * better at saying so than a boot screen is.
 */
const SPLASH_MIN_MS = SEQUENCE_MS;
const SPLASH_MAX_MS = 8000;
/**
 * When the last answer lands after the floor, the splash stays this much
 * longer — long enough to see that square take its colour, which is the fact
 * the whole screen was waiting to show.
 */
const SPLASH_BEAT_MS = 350;

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
  const [capacity, setCapacity] = useState<CapacityRisk[]>([]);
  const [findings, setFindings] = useState<Finding[]>([]);
  const [verdicts, setVerdicts] = useState<Record<string, RecordedVerdict>>({});
  const [judgedIncidents, setJudgedIncidents] = useState<string[]>([]);
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
  const [booting, setBooting] = useState(true);
  /** Set by "Skip for now": the estate is empty on purpose, for now. */
  const [skippedOnboarding, setSkippedOnboarding] = useState(false);

  // Against a server, the first question is whether this browser is allowed
  // in at all. `undefined` while the answer is outstanding, so the estate is
  // never rendered against a server that has not accepted us.
  const server = remote.serverUrl();
  const [authorized, setAuthorized] = useState<boolean | undefined>(
    server ? undefined : true,
  );

  useEffect(() => {
    if (!server) return;
    let cancelled = false;
    const token = remote.token();
    fetch(`${server}/auth/me`, {
      headers: token ? { authorization: `Bearer ${token}` } : {},
    })
      .then((r) => !cancelled && setAuthorized(r.ok))
      // A server that cannot be reached is not a server that refused us, but
      // the sign-in screen is the only place that can say so usefully.
      .catch(() => !cancelled && setAuthorized(false));
    return () => {
      cancelled = true;
    };
  }, [server]);
  const [team, setTeam] = useState<TeamState | null>(null);

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

  /**
   * Local mode is the default and needs no fetch to be usable, so a failure
   * here is not fatal: it leaves the app local rather than locking someone out
   * of their own machine over a setting they may never have set.
   */
  const refreshTeam = useCallback(async () => {
    try {
      setTeam(await api.getTeam());
    } catch (error) {
      console.error("workspace load failed:", errorMessage(error));
      setTeam({ hasSession: false });
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

  /**
   * Rides the history cadence but takes no window, because the forecast
   * behind it does not: the fit uses every retained sample regardless of what
   * the metrics screen is currently showing.
   */
  const refreshCapacity = useCallback(async () => {
    try {
      setCapacity(await api.getCapacityRisks(8));
    } catch (error) {
      console.error("capacity load failed:", errorMessage(error));
    }
  }, []);

  /**
   * Config findings, on the same slow cadence as capacity.
   *
   * Config is not telemetry — it changes when somebody changes it — so this
   * does not need the dashboard's cadence. It does need to be loaded without
   * being asked for: a finding nobody navigates to is a finding nobody has.
   */
  const refreshFindings = useCallback(async () => {
    try {
      setFindings(await api.getFindings());
    } catch (error) {
      console.error("findings load failed:", errorMessage(error));
    }
  }, []);

  /**
   * What has already been judged.
   *
   * Loaded separately from findings because it outlives them: a verdict
   * recorded last month is still the reason a card shows as rated today, and
   * the findings themselves are recomputed on every read.
   */
  const refreshJudgements = useCallback(async () => {
    try {
      const [recorded, incidents] = await Promise.all([
        api.getVerdicts(),
        api.getJudgedIncidents(),
      ]);
      setVerdicts(Object.fromEntries(recorded.map((v) => [v.findingId, v])));
      setJudgedIncidents(incidents);
    } catch (error) {
      // A journal that will not open must not stop the estate being watched.
      console.error("judgement record unavailable:", errorMessage(error));
    }
  }, []);

  const judge = useCallback(
    async (verdict: Verdict) => {
      try {
        await api.recordVerdict(verdict);
        await refreshJudgements();
      } catch (error) {
        console.error("could not record that verdict:", errorMessage(error));
      }
    },
    [refreshJudgements],
  );

  const judgeIncident = useCallback(
    async (recall: Recall) => {
      try {
        await api.recordRecall(recall);
        await refreshJudgements();
      } catch (error) {
        console.error("could not record that:", errorMessage(error));
      }
    },
    [refreshJudgements],
  );

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
    refreshTeam();
    api
      .getServiceCatalog()
      .then((descriptors) => setCatalog(new Catalog(descriptors)))
      .catch((e) => console.error("service catalog load failed:", errorMessage(e)));
    api
      .queryLogs({ limit: 1000 })
      .then(setLogs)
      .catch((e) => console.error("log load failed:", errorMessage(e)));
    api.pendingConfirmations().then(setConfirmations).catch(() => undefined);
  }, [refreshDashboard, refreshConfig, refreshIncidents, refreshTeam]);

  // Re-runs whenever the window changes, which is what makes the range
  // controls on Metrics and Logs actually refetch rather than just re-slice.
  useEffect(() => {
    refreshHistory();
    refreshHealthRuns();
    refreshHistogram();
    refreshContext();
    refreshCapacity();
    refreshFindings();
    refreshJudgements();
  }, [refreshHistory, refreshHealthRuns, refreshHistogram, refreshContext, refreshCapacity, refreshFindings, refreshJudgements]);

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
      refreshCapacity();
      refreshFindings();
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
    refreshCapacity,
    refreshFindings,
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
    refreshCapacity();
    refreshFindings();
  }, [refreshDashboard, refreshHistory, refreshHealthRuns, refreshHistogram, refreshCapacity, refreshFindings]);

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

  /**
   * The boot screen stands down when the app can actually answer for itself:
   * the workspace is loaded, the catalog is loaded, and every configured
   * connection has reported at least once. A first run with no connections
   * clears as soon as the catalog is in — there is nothing to reach.
   *
   * The floor keeps it from flashing; the ceiling keeps one unreachable
   * collector from holding the whole app behind a screen that cannot explain
   * itself. The estate has better words for a connection that has not
   * reported than a splash does.
   */
  const reached = dashboard?.snapshots.length ?? 0;
  const configured = config?.profiles.length ?? 0;
  /** The same test the dock uses: a key, or a local server that needs none. */
  const assistantConfigured = Boolean(
    config?.llm.hasApiKey || config?.llm.provider === "ollama",
  );
  const bootReady =
    config !== null &&
    !catalog.isEmpty &&
    team !== null &&
    (configured === 0 || reached >= configured);

  // Measured from mount rather than from the last change. Re-arming both
  // timers each time readiness changed meant a slow collector restarted the
  // five-second ceiling, and a late answer added a whole floor after itself.
  const bootStarted = useRef(performance.now());
  useEffect(() => {
    if (!booting) return;
    const elapsed = performance.now() - bootStarted.current;
    const ceiling = window.setTimeout(
      () => setBooting(false),
      Math.max(0, SPLASH_MAX_MS - elapsed),
    );
    const floor = bootReady
      ? window.setTimeout(
          () => setBooting(false),
          Math.max(SPLASH_MIN_MS - elapsed, SPLASH_BEAT_MS),
        )
      : undefined;
    return () => {
      window.clearTimeout(ceiling);
      if (floor !== undefined) window.clearTimeout(floor);
    };
  }, [booting, bootReady]);

  /** The splash may be ended early by a key or a click, but only once the app can stand. */
  const skipSplash = useCallback(() => {
    if (bootReady) setBooting(false);
  }, [bootReady]);

  /**
   * True from the first frame until the splash has finished leaving. The
   * sidebar hides its own mark meanwhile, because the splash's mark is flying
   * to that exact spot and two of them would land on top of each other.
   */
  const [handingOff, setHandingOff] = useState(true);

  // The handoff ends when the splash has finished leaving or, at the latest,
  // shortly after it began to. A window hidden while the splash leaves gets no
  // animation frames, so its exit cannot finish until someone looks — and the
  // sidebar must not keep its mark hidden for an exit nobody can see.
  useEffect(() => {
    if (booting || !handingOff) return;
    const late = window.setTimeout(() => setHandingOff(false), 1200);
    return () => window.clearTimeout(late);
  }, [booting, handingOff]);

  const splash = (
    <MotionConfig reducedMotion="user">
      <AnimatePresence onExitComplete={() => setHandingOff(false)}>
        {booting && (
          <Splash
            key="splash"
            workspaceLoaded={config !== null}
            catalogLoaded={!catalog.isEmpty}
            connections={config?.profiles ?? []}
            snapshots={dashboard?.snapshots ?? []}
            ready={bootReady}
            onSkip={skipSplash}
          />
        )}
      </AnimatePresence>
    </MotionConfig>
  );

  /**
   * Every screen below is drawn under the same splash, at the same place in
   * the tree, so the splash can leave over whichever one the app lands on
   * instead of being torn down the moment the gate changes.
   */
  const frame = (screen: React.ReactNode) => (
    <>
      {screen}
      {splash}
    </>
  );

  if (booting) return frame(null);

  /**
   * A workspace server is configured but this machine has no session.
   *
   * Only reachable once someone has pointed root at a server: in local mode —
   * the default, and what the packaged app does out of the box — there is
   * nothing to sign in to and this never appears. A monitoring tool that
   * demanded an account before it would look at your own Docker socket would
   * have taken something away rather than added anything.
   */
  // Before every other gate. Nothing below can load its data without one.
  if (server && authorized === undefined) return null;
  if (server && !authorized) {
    return frame(
      <ServerSignIn
        server={server}
        onSignedIn={() => {
          setAuthorized(true);
          // Reload rather than re-fetch piecemeal: every screen's data was
          // fetched as an unauthorized client and all of it is empty.
          window.location.reload();
        }}
      />,
    );
  }

  if (team?.serverUrl && !team.hasSession) {
    return frame(
      <SignIn
        serverUrl={team.serverUrl}
        model={model}
        minutes={minutes}
        healthRuns={healthRuns}
        onSignedIn={setTeam}
        onUseLocal={() => {
          api
            .saveTeamServer(undefined)
            .then(setTeam)
            .catch((e) => console.error("could not return to local mode:", errorMessage(e)));
        }}
      />,
    );
  }

  /**
   * Nothing configured means a first run, not an empty dashboard.
   *
   * Dropping someone into an estate view with no estate makes them hunt for
   * Settings to find out what the product even wants from them. Skipping is
   * allowed and remembered for the session — an operator who wants to look
   * around first should be able to.
   */
  if (configured === 0 && !skippedOnboarding && !catalog.isEmpty) {
    return frame(
      <Onboarding
        catalog={catalog}
        onConfigChange={onConfigChange}
        onSkip={() => setSkippedOnboarding(true)}
      />,
    );
  }

  return frame(
    // `reducedMotion="user"` is resolved once here, so no variant has to
    // re-check the preference and no plot can forget to.
    <MotionConfig reducedMotion="user">
    <div className="flex h-screen w-screen overflow-hidden bg-page">
      <Sidebar
        view={view}
        model={model}
        team={team}
        collapsed={collapsed}
        markHidden={handingOff}
        onNavigate={setView}
        onToggleCollapsed={() => setCollapsed((c) => !c)}
        onOpenPalette={() => setPaletteOpen(true)}
        onOpenWorkspace={() => setView({ name: "settings" })}
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
            assistantReady={assistantConfigured}
            model={model}
            catalog={catalog}
            incidents={incidents}
            changes={changes}
            topology={topology}
            correlation={onsetCorrelation}
            buckets={buckets}
            healthRuns={healthRuns}
            history={history}
            capacity={capacity}
            findings={findings}
            verdicts={verdicts}
            judgedIncidents={judgedIncidents}
            onJudge={judge}
            onJudgeIncident={judgeIncident}
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
            incidents={incidents}
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
            incidents={incidents}
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
            incidents={incidents}
            topology={topology}
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

        {view.name === "trace" && (
          <TraceDetail
            profileId={view.profileId}
            traceId={view.traceId}
            model={model}
            onNavigate={setView}
          />
        )}

        {view.name === "profile" && (
          <ProfileDetail
            profileId={view.profileId}
            query={view.query}
            model={model}
            onNavigate={setView}
          />
        )}

        {view.name === "correlate" && (
          <Correlate at={view.at} catalog={catalog} onNavigate={setView} />
        )}

        {view.name === "scorecard" && (
          <Scorecard
            connections={model.services.map((s) => ({
              id: s.snapshot.profileId,
              name: s.snapshot.profileName,
            }))}
            onNavigate={setView}
            onRefresh={refreshJudgements}
          />
        )}

        {view.name === "assistant" && (
          <Assistant
            config={config}
            incidents={incidents}
            conversationId={CONVERSATION_ID}
            onOpenSettings={() => setView({ name: "settings" })}
          />
        )}

        {view.name === "settings" && (
          <Settings
            config={config}
            openProfileId={view.name === "settings" ? view.profileId : undefined}
            openTab={view.name === "settings" ? view.tab : undefined}
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
          configured={assistantConfigured}
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
    </MotionConfig>,
  );
}
