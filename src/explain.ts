import type { Alert, GroupingReason, Incident, LogEvent } from "./types";

/**
 * What the operator clicked.
 *
 * The dock exists for one moment: someone is looking at a line they do not
 * understand and does not want to lose their place to find out. So the target
 * travels with enough of itself to be both restated in the panel and turned
 * into a grounded question — never "explain this error", which would invite
 * the model to invent one.
 */
export type ExplainTarget =
  | { kind: "log"; event: LogEvent }
  | { kind: "alert"; alert: Alert }
  | { kind: "incident"; incident: Incident }
  | { kind: "ask" };

/** The line shown back to the operator, so the panel is visibly about their click. */
export function targetSubject(target: ExplainTarget): {
  meta: string;
  body: string;
} | null {
  switch (target.kind) {
    case "log":
      return {
        meta: `${new Date(target.event.timestamp).toLocaleTimeString([], { hour12: false })} · ${target.event.profileName} · ${target.event.source}`,
        body: target.event.message,
      };
    case "alert":
      return {
        meta: `${target.alert.profileName}${target.alert.component ? ` · ${target.alert.component}` : ""} · ${target.alert.severity}`,
        body: target.alert.title,
      };
    case "incident":
      return {
        meta: `${target.incident.profileIds.join(", ")} · ${target.incident.alertIds.length} alert${target.incident.alertIds.length === 1 ? "" : "s"} · ${target.incident.confidence} confidence`,
        body: target.incident.title,
      };
    case "ask":
      return null;
  }
}

/**
 * The question, built from the artefact rather than from a template with a
 * blank in it. Everything the model needs to answer without guessing is
 * already on screen, so it is repeated here verbatim — the assistant's own
 * tools then supply the surrounding estate.
 */
export function promptFor(target: ExplainTarget): string {
  switch (target.kind) {
    case "log":
      return [
        `Explain this log line for an operator who does not recognise it.`,
        ``,
        `connection: ${target.event.profileName}`,
        `source: ${target.event.source}`,
        `level: ${target.event.level}`,
        `at: ${target.event.timestamp}`,
        `message: ${target.event.message}`,
        ``,
        `Say what it means, why it is happening on this connection specifically,`,
        `and what to check next. Cite the evidence you looked at.`,
      ].join("\n");
    case "alert":
      return [
        `Explain this alert for an operator who does not recognise it.`,
        ``,
        `connection: ${target.alert.profileName}`,
        `severity: ${target.alert.severity}`,
        `title: ${target.alert.title}`,
        `detail: ${target.alert.detail}`,
        `firing since: ${target.alert.firstSeen}`,
        ``,
        `Say what the condition means, whether anything else in the estate`,
        `explains it, and what to check next.`,
      ].join("\n");
    case "incident":
      return incidentPrompt(target.incident);
    case "ask":
      return "";
  }
}

/**
 * The incident is handed over with the grouping the correlator actually made,
 * including its reasons and its confidence, and the model is asked to explain
 * THAT — not to re-derive a story of its own.
 *
 * This is the whole shape of the division. The correlator decides what is one
 * incident, deterministically, from onsets and declared edges; the model
 * explains the mechanism and says what to check. A model that disagrees with
 * the grouping is worth hearing, so it is invited to say so explicitly rather
 * than quietly narrating a different failure — but it cannot silently replace
 * a derivation with a guess, because the derivation is in the prompt.
 */
function incidentPrompt(incident: Incident): string {
  return [
    `Explain this incident to an operator who has just opened it.`,
    ``,
    `title: ${incident.title}`,
    `severity: ${incident.severity} · health: ${incident.health}`,
    `started: ${incident.started} (still firing as of ${incident.lastSeen})`,
    `connections involved: ${incident.profileIds.join(", ")}`,
    `not explained by another member failing: ${incident.roots.join(", ") || "none identified"}`,
    `alerts grouped: ${incident.alertIds.length}`,
    ``,
    `root grouped these alerts as one incident for these reasons, at ${incident.confidence} confidence:`,
    ...incident.grouping.map((reason) => `  - ${reasonLine(reason)}`),
    ...(incident.grouping.length === 0 ? ["  - a single connection, so nothing was merged"] : []),
    ``,
    `Use your tools to look at the metrics, logs and recent changes on these`,
    `connections. Then: say what is most likely happening and why, name the`,
    `mechanism rather than restating the alert, and list what to check next in`,
    `priority order. Cite the evidence you actually looked at.`,
    ``,
    `If the evidence contradicts the grouping above — if these look like`,
    `separate problems, or if the origin is a connection root did not name —`,
    `say so first and explain what the grouping missed.`,
  ].join("\n");
}

function reasonLine(reason: GroupingReason): string {
  switch (reason.kind) {
    case "sameConnection":
      return `several conditions on ${reason.profileId}`;
    case "dependencyFailedFirst":
      return `${reason.dependent} depends on ${reason.root}, which failed first`;
    case "dependentPressuredIt":
      return `${reason.origin} failed first and ${reason.pressured}, which it depends on, degraded within 15 minutes — read as back-pressure, not causation`;
    case "simultaneousOnset":
      return `${reason.count} independent failures began within ${reason.windowSecs}s with no declared dependency between them`;
  }
}

/** A stable conversation per target, so switching what you ask about starts clean. */
export function conversationFor(target: ExplainTarget): string {
  switch (target.kind) {
    case "log":
      return `dock:log:${target.event.id}`;
    case "alert":
      return `dock:alert:${target.alert.id}`;
    case "incident":
      return `dock:incident:${target.incident.id}`;
    case "ask":
      return "dock:ask";
  }
}
