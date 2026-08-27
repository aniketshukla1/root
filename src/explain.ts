import type { Alert, LogEvent } from "./types";

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
    case "ask":
      return "";
  }
}

/** A stable conversation per target, so switching what you ask about starts clean. */
export function conversationFor(target: ExplainTarget): string {
  switch (target.kind) {
    case "log":
      return `dock:log:${target.event.id}`;
    case "alert":
      return `dock:alert:${target.alert.id}`;
    case "ask":
      return "dock:ask";
  }
}
