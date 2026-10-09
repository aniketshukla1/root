import { useState } from "react";

import { formatBytes, formatHorizon, formatMetricValue, formatRelative } from "../format";
import type {
  Called,
  ConfigDocument,
  Evidence,
  Finding,
  FixVerdict,
  Recall,
  ProblemVerdict,
  Provenance,
  RecordedVerdict,
  Redaction,
  Severity,
  Verdict,
  Withheld,
} from "../types";
import { Button, Chip, cx, Empty, Panel, Tag } from "../ui/primitives";

/**
 * Configuration, and what it means read against telemetry.
 *
 * Two things share this surface because they are one answer. A config file on
 * its own is a document nobody reads; a metric on its own is a number with no
 * action attached. `maxmemory 2gb` plus `noeviction` plus a memory series
 * climbing toward it is a deadline.
 *
 * Findings come first for that reason — the documents are the evidence, and
 * evidence goes underneath the conclusion it supports.
 */

export const SEVERITY: Record<Severity, { label: string; color: string; rank: number }> = {
  now: { label: "now", color: "var(--color-crit)", rank: 3 },
  soon: { label: "soon", color: "var(--color-warn)", rank: 2 },
  advisory: { label: "advisory", color: "var(--color-ink-4)", rank: 1 },
};

export function FindingList({
  findings,
  verdicts,
  onJudge,
  onOpenService,
}: {
  findings: Finding[];
  /** Latest judgement per finding id, so a rated card shows as rated. */
  verdicts?: Record<string, RecordedVerdict>;
  onJudge?: (verdict: Verdict) => void;
  onOpenService?: (profileId: string) => void;
}) {
  if (findings.length === 0) {
    return (
      <Empty
        title="Nothing to report"
        hint="Either no configuration is declared for this connection, or the rules had nothing to say. Absence here is not a clean bill of health — it is the rules that exist finding nothing, and there are not rules for everything."
      />
    );
  }

  return (
    <div className="flex flex-col gap-2.5">
      {findings.map((finding) => (
        <FindingCard
          key={finding.id}
          finding={finding}
          verdict={verdicts?.[finding.id]}
          onJudge={onJudge}
          onOpenService={onOpenService}
        />
      ))}
    </div>
  );
}

function FindingCard({
  finding,
  verdict,
  onJudge,
  onOpenService,
}: {
  finding: Finding;
  verdict?: RecordedVerdict;
  onJudge?: (verdict: Verdict) => void;
  onOpenService?: (profileId: string) => void;
}) {
  const [showEvidence, setShowEvidence] = useState(false);
  const severity = SEVERITY[finding.severity];

  return (
    <div
      className="rounded-md border border-line bg-page p-3"
      style={{ borderLeftColor: severity.color, borderLeftWidth: 2 }}
    >
      <div className="flex items-baseline gap-2">
        <span className="text-base font-medium text-ink">{finding.title}</span>
        <Tag color={severity.color}>{severity.label}</Tag>
        {/* A rule cannot invent a setting and the assistant can, so which one
            is talking is part of how much to believe it. Rules carry no tag:
            they are the default, and labelling both would make neither
            stand out. */}
        {source(finding.provenance) && <Tag>{source(finding.provenance)}</Tag>}
        {onOpenService && (
          <button
            type="button"
            onClick={() => onOpenService(finding.profileId)}
            className="num ml-auto text-2xs text-ink-4 underline-offset-2 hover:underline"
          >
            {finding.profileName}
          </button>
        )}
      </div>

      <p className="mt-1.5 max-w-[92ch] text-xs leading-relaxed text-ink-2">{finding.detail}</p>

      {finding.suggestion && (
        <div className="mt-2.5 rounded-sm border border-line-soft bg-panel p-2.5">
          <div className="num flex flex-wrap items-baseline gap-1.5 text-2xs">
            <span className="text-ink-3">{finding.suggestion.setting}</span>
            <span className="text-ink-4">{finding.suggestion.current}</span>
            <span className="text-ink-4">→</span>
            <span className="text-ink">{finding.suggestion.proposed}</span>
          </div>
          {/* Text, for a human to run. Nothing in the app executes it, and a
              button that looked like it might would be a lie about what this
              tool does. */}
          {finding.suggestion.apply && (
            <pre className="num mt-1.5 overflow-x-auto rounded-sm bg-page p-2 text-2xs leading-relaxed text-ink-3">
              {finding.suggestion.apply}
            </pre>
          )}
        </div>
      )}

      {/* Collapsed by default, present always. A recommendation whose reasoning
          cannot be checked is one people learn to ignore. */}
      <button
        type="button"
        onClick={() => setShowEvidence((v) => !v)}
        className="num mt-2 text-2xs text-ink-4 underline-offset-2 hover:underline"
      >
        {showEvidence ? "Hide" : "Show"} what this rests on ({finding.evidence.length})
      </button>

      {showEvidence && (
        <div className="mt-1.5 flex flex-col gap-1 border-t border-line-soft pt-1.5">
          {finding.evidence.map((item, i) => (
            <EvidenceRow key={i} evidence={item} />
          ))}
        </div>
      )}

      {/* Last, because rating something is what you do after reading it. */}
      {onJudge && <VerdictStrip finding={finding} verdict={verdict} onJudge={onJudge} />}
    </div>
  );
}

/**
 * Two questions, not a thumb.
 *
 * A single rating cannot say whether the detection or the remedy failed, and
 * those get fixed in completely different places — one is a rule that fires
 * wrongly, the other is a rule that is right about the problem and wrong about
 * what to do. Collapsing them loses the only part that tells you what to
 * change.
 *
 * "Already knew" is a correct answer, not a bad one: root was right, and being
 * second to know is a usefulness problem rather than an accuracy one. It is
 * separate from "real" so the difference stays visible — a tool that is always
 * right and always late has a specific, fixable failure.
 */
function VerdictStrip({
  finding,
  verdict,
  onJudge,
}: {
  finding: Finding;
  verdict?: RecordedVerdict;
  onJudge: (verdict: Verdict) => void;
}) {
  const [problem, setProblem] = useState<ProblemVerdict | undefined>(verdict?.problem);
  const [fix, setFix] = useState<FixVerdict | undefined>(verdict?.fix);
  const [note, setNote] = useState(verdict?.note ?? "");
  const [open, setOpen] = useState(false);

  const send = (next: { problem?: ProblemVerdict; fix?: FixVerdict; note?: string }) => {
    const decided = next.problem ?? problem;
    if (!decided) return;
    onJudge({
      findingId: finding.id,
      profileId: finding.profileId,
      profileName: finding.profileName,
      // The finding's own, not an assumed rule: filing a model claim as a
      // rule verdict would credit the rules for work they did not do and
      // leave the model's column empty, which is the failure this whole
      // measurement exists to avoid.
      provenance: finding.provenance,
      snapshot: finding,
      problem: decided,
      fix: next.fix ?? fix,
      note: (next.note ?? note).trim() || undefined,
    });
  };

  return (
    <div className="mt-2 border-t border-line-soft pt-2">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <span className="num shrink-0 text-2xs text-ink-4">Real problem?</span>
        <Choice
          value={problem}
          options={[
            ["real", "yes"],
            ["alreadyKnew", "yes, knew"],
            ["notAProblem", "no"],
            ["unsure", "unsure"],
          ]}
          onChange={(v) => {
            setProblem(v);
            send({ problem: v });
          }}
        />

        {/* Only askable when there is a fix to ask about. */}
        {finding.suggestion && (
          <>
            <span className="num shrink-0 text-2xs text-ink-4">Right fix?</span>
            <Choice
              value={fix}
              options={[
                ["right", "yes"],
                ["partlyRight", "nearly"],
                ["wrong", "no"],
                ["unsure", "unsure"],
              ]}
              onChange={(v) => {
                setFix(v);
                send({ fix: v });
              }}
            />
          </>
        )}

        {verdict && (
          <span className="num ml-auto shrink-0 text-2xs text-ink-4">
            rated {formatRelative(verdict.at)}
          </span>
        )}
      </div>

      {problem && (
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="num mt-1.5 text-2xs text-ink-4 underline-offset-2 hover:underline"
        >
          {open ? "Hide note" : note ? "Edit note" : "Add a note"}
        </button>
      )}

      {/* Free text, because the four buttons cannot hold the reason a finding
          was wrong, and the reason is what makes the next rule better. */}
      {open && (
        <div className="mt-1.5 flex gap-1.5">
          <input
            value={note}
            onChange={(e) => setNote(e.target.value)}
            placeholder="Why? — this is what turns a score into a change"
            className="h-7 min-w-0 flex-1 rounded-md border border-line bg-sunk px-2 font-mono text-xs text-ink placeholder:text-ink-4 focus:border-accent focus:outline-none"
          />
          <Button
            variant="ghost"
            onClick={() => {
              send({ note });
              setOpen(false);
            }}
          >
            Save
          </Button>
        </div>
      )}
    </div>
  );
}

/**
 * Did root call this one?
 *
 * The half of the measurement that thumbs on findings structurally cannot
 * produce. A rating on what root *said* measures precision — of the things it
 * raised, how many were real. It is silent on the failure that matters most:
 * root never mentioned the thing that actually broke, so there was no card to
 * rate and no signal that anything was missed.
 *
 * Asked here, at the incident, because this is the one moment somebody has the
 * answer in their head and is already looking at the evidence. A question
 * asked anywhere else gets guessed at or skipped.
 *
 * Asked once. A judged incident does not ask again, because a question that
 * reappears gets clicked through rather than considered.
 */
export function RecallStrip({
  incident,
  findings,
  judged,
  onJudge,
}: {
  incident: { id: string; headline: string; profileIds: string[]; startedAt: string };
  /** Findings for the connections involved, as candidate answers. */
  findings: Finding[];
  judged: boolean;
  onJudge: (recall: Recall) => void;
}) {
  const [note, setNote] = useState("");
  const [pending, setPending] = useState<Called>();

  if (judged) return null;

  const send = (called: Called, findingId?: string) =>
    onJudge({
      incidentId: incident.id,
      headline: incident.headline,
      profileIds: incident.profileIds,
      startedAt: incident.startedAt,
      called,
      findingId,
      note: note.trim() || undefined,
    });

  const candidates = findings.filter((f) => incident.profileIds.includes(f.profileId));

  return (
    // No panel of its own: this renders inside the incident panel, under the
    // claim it asks about.
    <div className="flex flex-col gap-2 px-4 py-2.5">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <span className="text-xs text-ink-2">Did root see this coming?</span>
        <Choice
          value={pending}
          options={[
            ["yes", "yes"],
            ["partly", "partly"],
            ["no", "no"],
          ]}
          onChange={(called) => {
            setPending(called);
            // "Yes" with exactly one candidate needs no follow-up — there is
            // only one thing it can have been.
            if (called === "no" || (called === "yes" && candidates.length === 1)) {
              send(called, called === "yes" ? candidates[0]?.id : undefined);
            }
          }}
        />
        <span className="num text-2xs text-ink-4">
          {/* Says why the question is being asked at all, because "rate this"
              with no reason reads as telemetry collection. */}
          the only way to measure what root misses
        </span>
      </div>

      {/* Which finding called it — so a rule can be credited by name, and the
          ones that never catch anything become visible. */}
      {pending && pending !== "no" && candidates.length > 1 && (
        <div className="flex flex-col gap-1.5">
          <span className="num text-2xs text-ink-4">Which one?</span>
          <div className="flex flex-wrap gap-1">
            {candidates.map((finding) => (
              <Chip key={finding.id} onClick={() => send(pending, finding.id)}>
                {finding.title}
              </Chip>
            ))}
            <Chip onClick={() => send(pending)}>none of these</Chip>
          </div>
        </div>
      )}

      {pending === "no" && (
        <div className="flex gap-1.5">
          <input
            value={note}
            onChange={(e) => setNote(e.target.value)}
            placeholder="What would have shown it? — this is the next rule"
            className="h-7 min-w-0 flex-1 rounded-md border border-line bg-sunk px-2 font-mono text-xs text-ink placeholder:text-ink-4 focus:border-accent focus:outline-none"
          />
          <Button variant="ghost" onClick={() => send("no")}>
            Save
          </Button>
        </div>
      )}
    </div>
  );
}

function Choice<T extends string>({
  value,
  options,
  onChange,
}: {
  value?: T;
  options: [T, string][];
  onChange: (value: T) => void;
}) {
  return (
    <div className="flex flex-wrap gap-1">
      {options.map(([option, label]) => (
        <Chip key={option} selected={value === option} onClick={() => onChange(option)}>
          {label}
        </Chip>
      ))}
    </div>
  );
}

/**
 * What to call the thing that produced a finding.
 *
 * Empty for a rule, because rules are the baseline and a tag on everything is
 * a tag on nothing. "Assistant" marks a claim that has not been checked by any
 * rule; "assistant · rule" marks one restating a rule, which is worth knowing
 * because it is the case where the assistant adds confidence rather than
 * information.
 */
function source(provenance: Provenance): string | undefined {
  if ("model" in provenance) return "assistant";
  if ("modelCitingRule" in provenance) return "assistant · rule";
  return undefined;
}

function EvidenceRow({ evidence }: { evidence: Evidence }) {
  switch (evidence.kind) {
    case "setting":
      return (
        <div className="num flex items-baseline gap-2 text-2xs">
          <span className="w-[26px] shrink-0 text-ink-4">set</span>
          <span className="text-ink-3">{evidence.key}</span>
          <span className="text-ink">{evidence.value}</span>
          <span className="ml-auto truncate text-ink-4" title={evidence.source}>
            {evidence.source}
          </span>
        </div>
      );
    case "metric":
      return (
        <div className="num flex items-baseline gap-2 text-2xs">
          <span className="w-[26px] shrink-0 text-ink-4">is</span>
          <span className="text-ink-3">{evidence.label}</span>
          <span className="text-ink">{formatMetricValue(evidence.value, evidence.unit)}</span>
          {evidence.limit !== undefined && (
            <span className="text-ink-4">
              of {formatMetricValue(evidence.limit, evidence.unit)}
            </span>
          )}
        </div>
      );
    case "projection":
      return (
        <div className="num flex items-baseline gap-2 text-2xs">
          <span className="w-[26px] shrink-0 text-ink-4">in</span>
          <span className="text-ink">{formatHorizon(evidence.secondsToLimit)}</span>
          <span className="ml-auto text-ink-4">
            R²={evidence.fit.toFixed(2)} over {Math.round(evidence.windowSecs / 60)}m
          </span>
        </div>
      );
  }
}

// ------------------------------------------------------------------ documents

export function DocumentList({ documents }: { documents: ConfigDocument[] }) {
  const [openId, setOpenId] = useState<string>();

  if (documents.length === 0) {
    return (
      <Empty
        title="No configuration declared"
        hint="Add a config source on this connection to point root at its files. Some connections also report their running configuration directly — those appear here without any setup."
      />
    );
  }

  return (
    <div className="flex flex-col gap-2">
      {documents.map((doc) => (
        <Document
          key={doc.id}
          document={doc}
          open={openId === doc.id}
          onToggle={() => setOpenId((id) => (id === doc.id ? undefined : doc.id))}
        />
      ))}
    </div>
  );
}

function Document({
  document,
  open,
  onToggle,
}: {
  document: ConfigDocument;
  open: boolean;
  onToggle: () => void;
}) {
  const live = document.origin.kind === "live";
  const where = document.origin.kind === "live" ? document.origin.via : document.origin.path;
  const redactions = document.redactions ?? [];

  return (
    <div className="rounded-md border border-line bg-page">
      <button
        type="button"
        onClick={onToggle}
        disabled={!document.body}
        className={cx(
          "flex w-full items-baseline gap-2 px-2.5 py-2 text-left",
          document.body && "row-hover",
        )}
      >
        <span className="text-xs text-ink">{document.label}</span>
        {/* Live vs file is the distinction that finds the commonest config bug
            there is: an edit nobody reloaded. */}
        <Tag>{live ? "running" : "file"}</Tag>
        <span className="num min-w-0 flex-1 truncate text-2xs text-ink-4" title={where}>
          {where}
        </span>
        {redactions.length > 0 && (
          <span
            className="num shrink-0 text-2xs text-ink-4"
            title={redactions.map(describeRedaction).join("\n")}
          >
            {redactions.length} redacted
          </span>
        )}
        {document.modified && (
          <span className="num shrink-0 text-2xs text-ink-4">
            {formatRelative(document.modified)}
          </span>
        )}
        {document.body && (
          <span className="num shrink-0 text-2xs text-ink-4">{formatBytes(document.bytes)}</span>
        )}
      </button>

      {document.withheld && <WithheldNote withheld={document.withheld} />}

      {open && document.body && (
        <pre className="num max-h-[420px] overflow-auto border-t border-line-soft px-2.5 py-2 text-2xs leading-relaxed text-ink-2">
          {document.body}
        </pre>
      )}
    </div>
  );
}

/**
 * Why there is no body.
 *
 * Rendered as a statement rather than an error. "There is a .env here and it
 * was not opened" is the single most reassuring thing this screen can say, and
 * hiding it would waste that.
 */
function WithheldNote({ withheld }: { withheld: Withheld }) {
  const text = (() => {
    switch (withheld.kind) {
      case "secret":
        return "Not opened. Every byte of a file like this is a credential, so root names it and leaves it alone — redacting it carefully would be weaker than not reading it.";
      case "tooLarge":
        return `Not read — ${formatBytes(withheld.bytes)} is past the limit for a config file. A path this size is usually pointed at a log or a data file rather than configuration.`;
      case "binary":
        return "Not read — this is not text.";
      case "unreadable":
        return withheld.message;
    }
  })();

  return (
    <p className="num border-t border-line-soft px-2.5 py-2 text-2xs leading-relaxed text-ink-4">
      {text}
    </p>
  );
}

function describeRedaction(redaction: Redaction): string {
  const reason = {
    keyName: "a key that holds credentials",
    privateKey: "a private key",
    jwt: "a JSON web token",
    urlCredentials: "a password inside a URL",
    providerToken: "a provider token",
    authorizationHeader: "an authorization header",
  }[redaction.reason];

  const where = redaction.line > 0 ? `line ${redaction.line}` : "a nested value";
  return redaction.key ? `${where}: ${redaction.key} — ${reason}` : `${where}: ${reason}`;
}

// ------------------------------------------------------------------- the tab

export function ConfigTab({
  documents,
  findings,
  loading,
  onRefresh,
}: {
  documents: ConfigDocument[];
  findings: Finding[];
  loading: boolean;
  onRefresh: () => void;
}) {
  const withheld = documents.filter((d) => d.withheld).length;
  const redacted = documents.reduce((n, d) => n + (d.redactions?.length ?? 0), 0);

  return (
    <div className="flex flex-col gap-3">
      <Panel
        title="Findings"
        subtitle={
          findings.length > 0
            ? "config read against this connection's own telemetry"
            : "nothing the rules recognise"
        }
        actions={
          <Button variant="ghost" onClick={onRefresh} disabled={loading}>
            {loading ? "Reading…" : "Re-read"}
          </Button>
        }
      >
        <FindingList findings={findings} />
      </Panel>

      <Panel
        title="Configuration"
        subtitle={[
          `${documents.length} document${documents.length === 1 ? "" : "s"}`,
          redacted > 0 && `${redacted} value${redacted === 1 ? "" : "s"} redacted`,
          withheld > 0 && `${withheld} not opened`,
        ]
          .filter(Boolean)
          .join(" · ")}
      >
        <DocumentList documents={documents} />
        {documents.length > 0 && (
          <p className="num mt-2.5 border-t border-line pt-2 text-2xs leading-relaxed text-ink-4">
            Credentials are removed before anything leaves this machine — passwords, private
            keys, tokens, and the password half of connection URLs. What you see here is what
            the assistant sees.
          </p>
        )}
      </Panel>
    </div>
  );
}
