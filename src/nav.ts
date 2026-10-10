import type { ExploreLanguage } from "./types";

/** Every destination in the app. Drill-downs carry their own target. */
export type View =
  | { name: "overview" }
  | { name: "services" }
  | { name: "alerts" }
  | { name: "metrics" }
  | { name: "dashboards" }
  /** `variables` arrive from a shared link; `edit` opens a new one ready to build. */
  | { name: "dashboard"; dashboardId: string; variables?: Record<string, string>; edit?: boolean }
  /** `seed` opens on a query another screen sent over. */
  | { name: "explore"; seed?: { profileId: string; language: ExploreLanguage; query: string } }
  | { name: "logs" }
  | { name: "assistant" }
  /** Is root any good yet — precision from findings, recall from incidents. */
  | { name: "scorecard" }
  /** `profileId` opens that connection's editor straight away. */
  | { name: "settings"; profileId?: string; tab?: "notifications" }
  | { name: "service"; profileId: string; tab?: ServiceTab }
  /** One component inside a service — a container, a cluster node, a job. */
  | { name: "component"; profileId: string; sectionId: string; rowId: string }
  /** One distributed trace, as a waterfall. */
  | { name: "trace"; profileId: string; traceId: string }
  /** One flame graph. `query` is the store's own profile selector. */
  | { name: "profile"; profileId: string; query: string }
  | { name: "correlate"; at: string };

export type ServiceTab =
  | "dashboard"
  | "components"
  | "alerts"
  | "metrics"
  | "logs"
  | "config"
  | "configuration";

/** Which nav entry should read as current, including for drill-downs. */
export function rootOf(view: View): string {
  if (
    view.name === "service" ||
    view.name === "component" ||
    view.name === "trace" ||
    view.name === "profile"
  )
    return "services";
  if (view.name === "correlate") return "logs";
  if (view.name === "dashboard") return "dashboards";
  return view.name;
}

/**
 * A shared dashboard link, `#dashboard=<id>&service=…`, as the view it names.
 * Read once before the first render and taken out of the address bar, so a
 * reload after moving elsewhere lands where the person is, not on the link.
 */
export function claimDashboardLink(): View | null {
  const params = new URLSearchParams(window.location.hash.slice(1));
  const dashboardId = params.get("dashboard");
  if (!dashboardId) return null;
  params.delete("dashboard");
  window.history.replaceState(null, "", window.location.pathname + window.location.search);
  return { name: "dashboard", dashboardId, variables: Object.fromEntries(params) };
}
