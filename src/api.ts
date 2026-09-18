import { invoke as tauriInvoke } from "@tauri-apps/api/core";

import * as fixtures from "./fixtures";

/**
 * Outside the Tauri webview there is no `invoke`, so a plain browser would
 * render nothing but empty states. Rather than make every frontend change
 * need a full Rust build, unhandled commands fall back to the development
 * estate in `fixtures.ts`. In the packaged app `__TAURI_INTERNALS__` is always
 * present, so this branch is never taken.
 */
const FIXTURES: Record<string, (args: Record<string, unknown>) => unknown> = {
  get_dashboard: () => fixtures.DASHBOARD,
  get_service_catalog: () => fixtures.DESCRIPTORS,
  get_config: () => fixtures.currentConfig(),
  get_team: () => fixtures.team(),
  save_team_server: ({ serverUrl }) => fixtures.saveTeamServer(serverUrl as string | null),
  sign_in: ({ email }) => fixtures.signIn(email as string),
  sign_out: () => fixtures.signOut(),
  get_workspace: () => fixtures.workspace(),
  refresh_connection: ({ profileId }) =>
    fixtures.SNAPSHOTS.find((s) => s.profileId === profileId) ?? fixtures.SNAPSHOTS[0],
  acknowledge_alert: ({ alertId }) => ({
    ...(fixtures.ALERTS.find((a) => a.id === alertId) ?? fixtures.ALERTS[0]),
    acknowledged: true,
  }),
  query_logs: ({ filter }) => {
    const f = (filter ?? {}) as { profileIds?: string[]; search?: string; levels?: string[] };
    return fixtures.LOGS.filter(
      (l) =>
        (!f.profileIds?.length || f.profileIds.includes(l.profileId)) &&
        (!f.levels?.length || f.levels.includes(l.level)) &&
        (!f.search || l.message.toLowerCase().includes(f.search.toLowerCase())),
    );
  },
  clear_logs: () => undefined,
  log_histogram: ({ buckets }) => fixtures.histogram((buckets as number) ?? 64),
  get_metric_history: ({ profileId }) => fixtures.seriesFor(profileId as string),
  get_all_metric_history: () =>
    Object.fromEntries(fixtures.SNAPSHOTS.map((s) => [s.profileId, fixtures.seriesFor(s.profileId)])),
  get_component_history: ({ profileId, sectionId, rowId }) =>
    fixtures.componentSeriesFor(profileId as string, sectionId as string, rowId as string),
  get_capacity_risks: ({ take }) => fixtures.capacityRisks((take as number) ?? 8),
  get_config_documents: ({ profileId }) => fixtures.configDocuments(profileId as string),
  get_findings: ({ profileId }) => fixtures.findings(profileId as string | undefined),
  record_verdict: ({ verdict }) => fixtures.recordVerdict(verdict as Verdict),
  record_recall: ({ recall }) => fixtures.recordRecall(recall as Recall),
  get_verdicts: () => fixtures.verdicts(),
  get_judged_incidents: () => fixtures.judgedIncidents(),
  get_scorecard: () => fixtures.scorecard(),
  get_long_history: () => [],
  run_backtest: ({ profileId }) => fixtures.backtest(profileId as string),
  run_scenarios: () => fixtures.scenarios(),
  get_runtime_report: () => fixtures.runtimeReport(),
  get_health_runs: () => fixtures.healthRuns(),
  get_topology: () => fixtures.topology(),
  get_incidents: () => fixtures.incidents(),
  get_change_events: () => fixtures.CHANGES,
  correlate: ({ at, windowSecs }) =>
    fixtures.correlation((at as string) ?? new Date().toISOString(), (windowSecs as number) ?? 120),
  pending_confirmations: () => [],
  get_ingest_settings: () => ({ enabled: false, port: 7717, hasToken: false }),
  get_mcp_settings: () => ({
    enabled: false,
    port: 7718,
    hasToken: false,
    url: "http://127.0.0.1:7718/mcp",
    keychainAvailable: true,
    tools: ["list_connections", "get_health", "search_logs"],
  }),
  secret_store_available: () => true,
  list_models: () => ["claude-opus-5", "claude-sonnet-5"],
  // A HealthSnapshot, not a string: the form reads snapshot.summary and
  // snapshot.health, and a string here rendered as a shrug.
  test_profile: ({ profile }) => {
    const p = profile as { id?: string; name?: string; kind?: string };
    return {
      profileId: p.id ?? "new",
      profileName: p.name ?? "new connection",
      kind: p.kind ?? "docker",
      health: "healthy",
      summary: "connected — 8 containers found",
      metrics: [],
      sections: [],
      checkedAt: new Date().toISOString(),
      latencyMs: 42,
    };
  },
  save_profile: ({ profile }) => fixtures.saveProfile(profile as never),
  delete_profile: ({ profileId }) => fixtures.deleteProfile(profileId as string),
  explain_component: ({ profileId, sectionId, rowId }) =>
    fixtures.componentReading(profileId as string, sectionId as string, rowId as string),
  send_message: ({ conversationId, message }) =>
    fixtures.agentTurn(conversationId as string, message as string),
  clear_conversation: () => undefined,
};

/**
 * `?slow=<ms>` delays every fixture response.
 *
 * The boot screen only shows its named steps when the boot is actually slow —
 * on a fast one it is a mark and a wordmark and then it is gone, which is the
 * intent. That made the steps impossible to look at while designing them,
 * since fixtures resolve in the same tick. This makes the slow path
 * reachable. Fixtures only: the packaged app never takes this branch.
 */
const SLOW_MS = (() => {
  if (typeof window === "undefined") return 0;
  const asked = Number(new URLSearchParams(window.location.search).get("slow"));
  return Number.isFinite(asked) ? Math.min(Math.max(asked, 0), 20_000) : 0;
})();

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!fixtures.isTauri()) {
    const fixture = FIXTURES[cmd];
    if (!fixture) return Promise.reject(new Error(`No development fixture for "${cmd}"`));
    const value = fixture(args ?? {}) as T;
    return SLOW_MS > 0
      ? new Promise((resolve) => setTimeout(() => resolve(value), SLOW_MS))
      : Promise.resolve(value);
  }
  return tauriInvoke<T>(cmd, args);
}

import type {
  Alert,
  AgentTurn,
  AppConfig,
  ChangeEvent,
  CapacityRisk,
  ComponentReading,
  ConfigDocument,
  ConnectionProfile,
  Finding,
  Recall,
  RecordedVerdict,
  RuntimeReport,
  Replay,
  Scorecard,
  SuiteResult,
  Verdict,
  Correlation,
  DashboardState,
  HealthRun,
  HealthSnapshot,
  HistogramBucket,
  Incident,
  IngestStatus,
  McpStatus,
  LlmSettings,
  LogEvent,
  LogFilter,
  MetricSeries,
  PendingConfirmation,
  ServiceDescriptor,
  TeamState,
  TopologyView,
  Trace,
  Workspace,
} from "./types";

export const api = {
  getDashboard: () => invoke<DashboardState>("get_dashboard"),

  refreshConnection: (profileId: string) =>
    invoke<HealthSnapshot>("refresh_connection", { profileId }),

  acknowledgeAlert: (alertId: string) => invoke<Alert>("acknowledge_alert", { alertId }),

  /** Run a mutating action from a section row. The UI confirms before calling. */
  runRowAction: (profileId: string, tool: string, args: Record<string, unknown>) =>
    invoke<string>("run_row_action", { profileId, tool, args }),

  queryLogs: (filter: LogFilter) => invoke<LogEvent[]>("query_logs", { filter }),

  clearLogs: () => invoke<void>("clear_logs"),

  logHistogram: (filter: LogFilter, minutes: number, buckets: number) =>
    invoke<HistogramBucket[]>("log_histogram", { filter, minutes, buckets }),

  /** What every system was doing around one moment. */
  correlate: (at: string, windowSecs?: number) =>
    invoke<Correlation>("correlate", { at, windowSecs: windowSecs ?? null }),

  /** Console output for one build — kept with the build, not in the stream. */
  getBuildConsole: (profileId: string, job: string, build?: number, tailLines?: number) =>
    invoke<{ job: string; build: string; totalLines: number; console: string }>(
      "get_build_console",
      { profileId, job, build: build ?? null, tailLines: tailLines ?? null },
    ),

  /** One trace, already assembled into a tree by the backend. */
  getTrace: (profileId: string, traceId: string) =>
    invoke<Trace>("get_trace", { profileId, traceId }),

  getMetricHistory: (profileId: string, minutes?: number) =>
    invoke<MetricSeries[]>("get_metric_history", { profileId, minutes: minutes ?? null }),

  /** Empty for sections the backend does not track — those have no series. */
  getComponentHistory: (
    profileId: string,
    sectionId: string,
    rowId: string,
    minutes?: number,
  ) =>
    invoke<MetricSeries[]>("get_component_history", {
      profileId,
      sectionId,
      rowId,
      minutes: minutes ?? null,
    }),

  /**
   * What is closest to a limit across the whole estate, soonest first.
   *
   * Deliberately takes no window: the fit uses every retained sample, because
   * a fifteen-minute view cannot see a disk three weeks out and the answer to
   * "when does this fill" should not change because somebody clicked 1h.
   */
  getCapacityRisks: (take?: number) =>
    invoke<CapacityRisk[]>("get_capacity_risks", { take: take ?? null }),

  /**
   * Declared files and live config together. Bodies arrive redacted — there is
   * no variant of this that returns the original bytes.
   */
  getConfigDocuments: (profileId: string) =>
    invoke<ConfigDocument[]>("get_config_documents", { profileId }),

  /** Omit `profileId` for the whole estate. */
  getFindings: (profileId?: string) =>
    invoke<Finding[]>("get_findings", { profileId: profileId ?? null }),

  /**
   * Record a judgement. Always an insert — changing a rating writes a second
   * row and leaves the first, because a score that can be edited cannot
   * answer "did root get better".
   */
  recordVerdict: (verdict: Verdict) => invoke<number>("record_verdict", { verdict }),

  /**
   * Record whether root called an incident.
   *
   * The half ratings on findings cannot produce: if root never mentioned the
   * thing that broke, there was no card to rate.
   */
  recordRecall: (recall: Recall) => invoke<number>("record_recall", { recall }),

  /** The latest judgement per finding, so the UI knows what is already rated. */
  getVerdicts: (limit?: number) =>
    invoke<RecordedVerdict[]>("get_verdicts", { limit: limit ?? null }),

  /** Incidents already judged, so nobody is asked the same question twice. */
  getJudgedIncidents: () => invoke<string[]>("get_judged_incidents"),

  getScorecard: () => invoke<Scorecard>("get_scorecard"),

  /**
   * Replay the current rules across stored history.
   *
   * Scored against incidents already judged: the useful output is not "these
   * rules fire" but "these would have fired before the thing recorded as a
   * miss". Uses the long-range source when one is declared, and says so in its
   * caveats when it had to fall back to root's own short ring.
   */
  /**
   * Run the built-in scenarios against the rules as they stand.
   *
   * Touches no infrastructure and needs nothing configured — it grades the
   * rules against embedded fixtures, so it works on a fresh install.
   */
  runScenarios: () => invoke<SuiteResult>("run_scenarios"),

  /**
   * Declared runtimes checked against what root can see, plus the containers
   * nothing is watching. Only disagreements come back — agreement is not news.
   */
  getRuntimeReport: () => invoke<RuntimeReport>("get_runtime_report"),

  runBacktest: (profileId: string, days?: number, stepMinutes?: number) =>
    invoke<Replay>("run_backtest", {
      profileId,
      days: days ?? null,
      stepMinutes: stepMinutes ?? null,
    }),

  /**
   * Metric history from the estate's own Prometheus, over a window root's own
   * ring buffer cannot reach. Empty when the connection declares no source.
   */
  getLongHistory: (profileId: string, hours?: number) =>
    invoke<MetricSeries[]>("get_long_history", { profileId, hours: hours ?? null }),

  getAllMetricHistory: (minutes?: number) =>
    invoke<Record<string, MetricSeries[]>>("get_all_metric_history", {
      minutes: minutes ?? null,
    }),

  /** Deploys and other changes. Omit `profileId` for the whole estate. */
  getChangeEvents: (profileId?: string, minutes?: number, limit?: number) =>
    invoke<ChangeEvent[]>("get_change_events", {
      profileId: profileId ?? null,
      minutes: minutes ?? null,
      limit: limit ?? null,
    }),

  getIngestSettings: () => invoke<IngestStatus>("get_ingest_settings"),

  saveIngestSettings: (enabled: boolean, port: number) =>
    invoke<IngestStatus>("save_ingest_settings", { enabled, port }),

  /** Returns the new token once. It cannot be read back afterwards. */
  rotateIngestToken: () => invoke<string>("rotate_ingest_token"),

  getMcpSettings: () => invoke<McpStatus>("get_mcp_settings"),

  saveMcpSettings: (enabled: boolean, port: number) =>
    invoke<McpStatus>("save_mcp_settings", { enabled, port }),

  /** Returns the new token once. It cannot be read back afterwards. */
  rotateMcpToken: () => invoke<string>("rotate_mcp_token"),

  /** The service registry — what kinds exist and how to render their forms. */
  getServiceCatalog: () => invoke<ServiceDescriptor[]>("get_service_catalog"),

  /** State runs for every connection across the window — the lane view. */
  getHealthRuns: (minutes?: number) =>
    invoke<Record<string, HealthRun[]>>("get_health_runs", { minutes: minutes ?? null }),

  /** The declared dependency graph, plus which current failures are roots. */
  getTopology: () => invoke<TopologyView>("get_topology"),

  /**
   * The active alerts grouped into the things actually happening.
   *
   * Derived on every call, never stored: an incident is a claim about the
   * present, so it should stop existing the moment its alerts clear. Each one
   * carries the reasons its members were grouped, which is what makes a wrong
   * grouping arguable instead of mysterious.
   */
  getIncidents: () => invoke<Incident[]>("get_incidents"),

  getConfig: () => invoke<AppConfig>("get_config"),

  /** Local mode, or a workspace on a server you run. Never a hosted service. */
  getTeam: () => invoke<TeamState>("get_team"),

  /** Pass `undefined` to clear it and return to local mode, which signs out. */
  saveTeamServer: (serverUrl?: string) =>
    invoke<TeamState>("save_team_server", { serverUrl: serverUrl ?? null }),

  /** The password is exchanged for a token and is not retained. */
  signIn: (email: string, password: string) =>
    invoke<TeamState>("sign_in", { email, password }),

  signOut: () => invoke<TeamState>("sign_out"),

  /** Errors in local mode: nobody is not the same fact as no workspace. */
  getWorkspace: () => invoke<Workspace>("get_workspace"),

  secretStoreAvailable: () => invoke<boolean>("secret_store_available"),

  /** `secret === undefined` keeps any credential already in the keychain. */
  saveProfile: (profile: ConnectionProfile, secret?: string) =>
    invoke<AppConfig>("save_profile", { profile, secret: secret ?? null }),

  deleteProfile: (profileId: string) => invoke<AppConfig>("delete_profile", { profileId }),

  testProfile: (profile: ConnectionProfile, secret?: string) =>
    invoke<HealthSnapshot>("test_profile", { profile, secret: secret ?? null }),

  saveLlmSettings: (settings: LlmSettings, apiKey?: string) =>
    invoke<AppConfig>("save_llm_settings", { settings, apiKey: apiKey ?? null }),

  listModels: () => invoke<string[]>("list_models"),

  /**
   * A short reading of one failing component. Cached in the backend by the
   * component's state, so hovering the same broken container twice is one call.
   */
  explainComponent: (profileId: string, sectionId: string, rowId: string) =>
    invoke<ComponentReading>("explain_component", { profileId, sectionId, rowId }),

  sendMessage: (conversationId: string, message: string) =>
    invoke<AgentTurn>("send_message", { conversationId, message }),

  resolveConfirmation: (callId: string, approved: boolean) =>
    invoke<void>("resolve_confirmation", { callId, approved }),

  pendingConfirmations: () => invoke<PendingConfirmation[]>("pending_confirmations"),

  clearConversation: (conversationId: string) =>
    invoke<void>("clear_conversation", { conversationId }),
};

/** Tauri rejects with the serialized `AppError` string. */
export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}
