// The model's account of one incident, rendered so it can be argued with.
//
// Everything on this screen is arranged around one problem: a language model
// writing about an outage produces fluent, plausible prose whether or not it
// has understood anything, and fluent prose is exactly what an operator
// mid-incident is least able to evaluate. So the text is never the whole
// panel.
//
// Three things sit beside it, and each one is there to make the account
// checkable rather than readable:
//
//   - **What would disprove this.** The backend refuses an analysis without
//     it. An explanation nothing could falsify is a sentence, not a finding,
//     and this is the line that turns the rest into a claim.
//   - **Whether it disagrees with root.** `incidents.rs` already named a root
//     cause deterministically. When the model overrules that, the dissent is
//     the most valuable output on the page — either it saw something the rules
//     cannot encode, or it is wrong somewhere somebody can go and look.
//   - **What it was told.** Every fact, in order, behind a disclosure. Nobody
//     reads it most of the time; the point is that it is there the one time
//     the account looks wrong.

import { useState } from "react";

import { formatRelative } from "../format";
import type { Confidence, RootCause } from "../types";
import { Button, cx, Panel } from "./primitives";

/** How sure the model says it is. Not how sure root is — those are different. */
const CONFIDENCE: Record<Confidence, { label: string; color: string; hint: string }> = {
  high: {
    label: "confident",
    color: "var(--color-ok)",
    hint: "The model says the evidence points one way. It is still a claim, not a measurement.",
  },
  medium: {
    label: "likely",
    color: "var(--color-warn)",
    hint: "The model says this fits, without ruling the alternatives out.",
  },
  low: {
    label: "unsure",
    color: "var(--color-ink-4)",
    hint: "The model says the evidence does not settle it. The list of what to check is the useful part here.",
  },
};

export function RootCausePanel({
  cause,
  loading,
  error,
  onRetry,
}: {
  cause?: RootCause;
  loading: boolean;
  error?: string;
  onRetry: () => void;
}) {
  const [showGrounds, setShowGrounds] = useState(false);

  if (loading) {
    return (
      <Panel title="Why this happened" subtitle="reading the evidence">
        <p className="num text-2xs leading-relaxed text-ink-4">
          One call, on what root already collected — the alerts in order, what changed
          before them, the log shape across the window, and the declared dependencies.
        </p>
      </Panel>
    );
  }

  if (error) {
    return (
      <Panel
        title="Why this happened"
        actions={
          <Button variant="ghost" onClick={onRetry}>
            Try again
          </Button>
        }
      >
        <p className="num text-2xs leading-relaxed text-crit">{error}</p>
      </Panel>
    );
  }

  if (!cause) return null;

  const confidence = CONFIDENCE[cause.confidence];

  return (
    <Panel
      title="Why this happened"
      subtitle={`${cause.model} · ${formatRelative(cause.at)}`}
      actions={
        <Button variant="ghost" onClick={onRetry} title="Ask again on the current evidence">
          Ask again
        </Button>
      }
    >
      <div className="flex flex-col gap-3">
        <div className="flex items-start gap-2.5">
          <span
            className="num mt-1 shrink-0 text-2xs tracking-[.08em] uppercase"
            style={{ color: confidence.color }}
            title={confidence.hint}
          >
            {confidence.label}
          </span>
          <p className="min-w-0 flex-1 text-sm leading-snug text-ink">{cause.summary}</p>
        </div>

        {/* The dissent goes directly under the summary, not at the bottom.
            When the model overrules root's own grouping that is the headline,
            and burying it below three supporting lists would make it a
            footnote to an account it contradicts. */}
        {!cause.agrees && cause.dissent && (
          <div
            className="rounded-sm border px-2.5 py-2"
            style={{
              borderColor: "color-mix(in oklab, var(--color-warn) 45%, transparent)",
              background: "color-mix(in oklab, var(--color-warn) 8%, transparent)",
            }}
          >
            <p className="num text-2xs tracking-[.08em] text-warn uppercase">
              Disagrees with the grouping
            </p>
            <p className="mt-1 text-xs leading-relaxed text-ink-2">{cause.dissent}</p>
          </div>
        )}

        {cause.chain.length > 0 && (
          <Section label="In order">
            <ol className="flex flex-col gap-1">
              {cause.chain.map((step, i) => (
                <li key={i} className="flex gap-2">
                  <span className="num shrink-0 text-2xs text-ink-4 tabular-nums">
                    {i + 1}.
                  </span>
                  <span className="text-xs leading-snug text-ink-2">{step}</span>
                </li>
              ))}
            </ol>
          </Section>
        )}

        {cause.confirm.length > 0 && (
          <Section label="Check this first">
            {/* Ordered, because the model was asked for these most-decisive
                first and three bare lines do not say so. */}
            <ol className="flex flex-col gap-1">
              {cause.confirm.map((step, i) => (
                <li key={i} className="flex gap-2">
                  <span className="num shrink-0 text-2xs text-ink-4 tabular-nums">
                    {i + 1}.
                  </span>
                  <span className="text-xs leading-snug text-ink-2">{step}</span>
                </li>
              ))}
            </ol>
          </Section>
        )}

        {/* The field that makes the rest of it a claim rather than a story. */}
        <Section label="What would show this is wrong">
          <p className="text-xs leading-relaxed text-ink-2">{cause.disconfirm}</p>
        </Section>

        {cause.because.length > 0 && (
          <Section label="Built on">
            <ul className="flex flex-col gap-1">
              {cause.because.map((line, i) => (
                <li key={i} className="num text-2xs leading-relaxed text-ink-4">
                  {line}
                </li>
              ))}
            </ul>
          </Section>
        )}

        {cause.grounds.length > 0 && (
          <div className="border-t border-line-soft pt-2">
            <button
              type="button"
              onClick={() => setShowGrounds((v) => !v)}
              className="num text-2xs text-ink-4 transition-colors hover:text-accent"
            >
              {showGrounds ? "Hide" : "Show"} everything it was told (
              {cause.grounds.length} lines)
            </button>
            {showGrounds && (
              <pre
                className={cx(
                  "num mt-2 max-h-[260px] overflow-auto rounded-sm border border-line",
                  "bg-page px-2 py-1.5 text-2xs leading-relaxed whitespace-pre-wrap text-ink-4",
                )}
              >
                {cause.grounds.join("\n")}
              </pre>
            )}
          </div>
        )}
      </div>
    </Panel>
  );
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <p className="num text-2xs tracking-[.08em] text-ink-4 uppercase">{label}</p>
      <div className="mt-1.5">{children}</div>
    </div>
  );
}
