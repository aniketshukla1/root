/** Every destination in the app. Drill-downs carry their own target. */
export type View =
  | { name: "overview" }
  | { name: "services" }
  | { name: "alerts" }
  | { name: "metrics" }
  | { name: "logs" }
  | { name: "assistant" }
  /** `profileId` opens that connection's editor straight away. */
  | { name: "settings"; profileId?: string }
  | { name: "service"; profileId: string; tab?: ServiceTab }
  /** One component inside a service — a container, a cluster node, a job. */
  | { name: "component"; profileId: string; sectionId: string; rowId: string }
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
  if (view.name === "service" || view.name === "component") return "services";
  if (view.name === "correlate") return "logs";
  return view.name;
}
