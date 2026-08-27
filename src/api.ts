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
  get_config: () => fixtures.CONFIG,
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
  get_component_history: () => [],
  get_health_runs: () => fixtures.healthRuns(),
  get_change_events: () => fixtures.CHANGES,
  correlate: ({ at }) => ({ at: at ?? new Date().toISOString(), lanes: [] }),
  pending_confirmations: () => [],
  get_ingest_settings: () => ({ enabled: false, port: 7717, hasToken: false }),
  secret_store_available: () => true,
  list_models: () => ["claude-opus-5", "claude-sonnet-5"],
  test_profile: () => "connected — 8 containers found",
  send_message: ({ conversationId, message }) =>
    fixtures.agentTurn(conversationId as string, message as string),
  clear_conversation: () => undefined,
};

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!fixtures.isTauri()) {
    const fixture = FIXTURES[cmd];
    if (fixture) return Promise.resolve(fixture(args ?? {}) as T);
    return Promise.reject(new Error(`No development fixture for "${cmd}"`));
  }
  return tauriInvoke<T>(cmd, args);
}

import type {
  Alert,
  AgentTurn,
  AppConfig,
  ChangeEvent,
  ConnectionProfile,
  Correlation,
  DashboardState,
  HealthRun,
  HealthSnapshot,
  HistogramBucket,
  IngestStatus,
  LlmSettings,
  LogEvent,
  LogFilter,
  MetricSeries,
  PendingConfirmation,
  ServiceDescriptor,
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

  /** The service registry — what kinds exist and how to render their forms. */
  getServiceCatalog: () => invoke<ServiceDescriptor[]>("get_service_catalog"),

  /** State runs for every connection across the window — the lane view. */
  getHealthRuns: (minutes?: number) =>
    invoke<Record<string, HealthRun[]>>("get_health_runs", { minutes: minutes ?? null }),

  getConfig: () => invoke<AppConfig>("get_config"),

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
