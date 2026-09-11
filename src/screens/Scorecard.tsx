import { useCallback, useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import type { View } from "../nav";
import type { FamilyScore, RecallScore, RuleScore, Scorecard as Card } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Button, Empty, Panel, Tag, cx } from "../ui/primitives";

/**
 * Is root any good yet?
 *
 * The question every step of this product's roadmap turns on, and the reason
 * the judgement record exists. It has two halves that come from different
 * places and fail in opposite directions:
 *
 * **Precision** — of the things root raised, how many were real. Comes from
 * rating findings. Easy to collect, and on its own deeply misleading: a tool
 * that says almost nothing scores perfectly.
 *
 * **Recall** — of the things that actually broke, how many root saw coming.
 * Comes from the incident side. Harder to collect and the one that decides
 * whether any of this is worth running, because a monitor graded only on the
 * times it spoke looks excellent right up until the outage it had nothing to
 * say about.
 *
 * Both are shown, neither is combined into a single number, and the sample
 * size is always visible — because at the volumes a single estate produces,
 * the honest answer for a long while is "not enough data yet", and a
 * confident-looking percentage over nine judgements would be worse than no
 * screen at all.
 */

/** Below this, a percentage is theatre. Named so the number has a reason. */
const ENOUGH_TO_MEAN_SOMETHING = 20;

export function Scorecard({
  onNavigate,
  onRefresh,
}: {
  onNavigate: (view: View) => void;
  onRefresh: () => void;
}) {
  const [card, setCard] = useState<Card>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setCard(await api.getScorecard());
      setError(undefined);
    } catch (e) {
      // The journal not opening is a real state with a real cause, not a
      // blank screen: an unwritable config directory, a locked file.
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const judged = card?.byFamily.reduce((n, f) => n + f.judged, 0) ?? 0;

  return (
    <>
      <ScreenHeader
        title="Scorecard"
        meta={
          <>
            <span>{judged} judged</span>
            <span>·</span>
            <span>{card?.recall.incidents ?? 0} incidents rated</span>
            {card && (
              <>
                <span>·</span>
                <span>rules v{card.rulesVersion}</span>
              </>
            )}
          </>
        }
        onRefresh={() => {
          load();
          onRefresh();
        }}
      />

      <Screen className="flex flex-col gap-3">
        {error ? (
          <Panel title="No judgement record">
            <Empty
              title="The record could not be opened"
              hint={error}
            />
          </Panel>
        ) : loading && !card ? (
          <Panel>
            <Empty title="Reading" hint="Opening the judgement record." />
          </Panel>
        ) : judged === 0 && (card?.recall.incidents ?? 0) === 0 ? (
          <Panel title="Nothing judged yet">
            <Empty
              title="No ratings recorded"
              hint="Rate a finding on the overview or a connection's Config tab, and answer the question that appears on an incident. Both are needed: findings measure what root got right, incidents measure what it missed, and the second cannot be inferred from the first."
            />
            <div className="mt-3 flex gap-1.5">
              <Button onClick={() => onNavigate({ name: "overview" })}>Go to findings</Button>
            </div>
          </Panel>
        ) : (
          <>
            <div className="grid gap-3 @4xl:grid-cols-2">
              <Panel
                title="Precision"
                subtitle="of what root raised, how much was real"
              >
                {card!.byFamily.length === 0 ? (
                  <Empty
                    title="No findings rated"
                    hint="Precision needs findings judged. Rate a few on the overview."
                  />
                ) : (
                  <div className="flex flex-col gap-2.5">
                    {card!.byFamily.map((family) => (
                      <FamilyRow key={family.family} family={family} />
                    ))}
                  </div>
                )}
              </Panel>

              <Panel
                title="Recall"
                subtitle="of what actually broke, how much root saw coming"
              >
                <RecallPanel recall={card!.recall} />
              </Panel>
            </div>

            {/* The actionable half. A family score says whether to trust a
                class of finding; a rule score says which one to go and fix. */}
            <Panel
              title="By rule"
              subtitle={
                card!.byRule.length > 0
                  ? "noisiest first — this is the list of what to fix"
                  : "nothing rated yet"
              }
            >
              {card!.byRule.length === 0 ? (
                <Empty
                  title="No rule has been rated"
                  hint="Once findings are judged, the rules that fire wrongly show up here by name."
                />
              ) : (
                <div className="flex flex-col gap-1">
                  {card!.byRule.map((rule) => (
                    <RuleRow key={rule.rule} rule={rule} />
                  ))}
                </div>
              )}
            </Panel>

            <p className="num max-w-[92ch] text-2xs leading-relaxed text-ink-4">
              Precision and recall are kept apart on purpose and never averaged into one
              number. They move in opposite directions — a tool that raises less scores
              better on the first and worse on the second — and a single score would let one
              hide the other. A gate for letting an agent act should be set on both, and per
              action, before anyone is impatient enough to want it moved.
            </p>
          </>
        )}
      </Screen>
    </>
  );
}

function FamilyRow({ family }: { family: FamilyScore }) {
  const decided = family.real + family.notAProblem + family.alreadyKnew;
  const correct = family.real + family.alreadyKnew;
  const precision = decided > 0 ? correct / decided : undefined;
  const thin = decided < ENOUGH_TO_MEAN_SOMETHING;

  return (
    <div className="rounded-md border border-line bg-page p-2.5">
      <div className="flex items-baseline gap-2">
        <span className="num text-xs text-ink">{label(family.family)}</span>
        <span className="num ml-auto text-[17px] text-ink">
          {precision === undefined ? "—" : `${Math.round(precision * 100)}%`}
        </span>
      </div>

      <Bar
        segments={[
          { value: family.real, color: "var(--color-ok)" },
          { value: family.alreadyKnew, color: "var(--color-accent)" },
          { value: family.notAProblem, color: "var(--color-warn)" },
          { value: family.unsure, color: "var(--color-line)" },
        ]}
      />

      <div className="num mt-1.5 flex flex-wrap gap-x-3 gap-y-0.5 text-2xs text-ink-4">
        <Count label="real" value={family.real} color="var(--color-ok)" />
        <Count label="knew" value={family.alreadyKnew} color="var(--color-accent)" />
        <Count label="not a problem" value={family.notAProblem} color="var(--color-warn)" />
        {family.unsure > 0 && <Count label="unsure" value={family.unsure} />}
      </div>

      {/* Fix quality is tracked apart from detection quality, because a rule
          that finds the right problem and prescribes the wrong remedy is a
          different bug from one that finds nothing real. */}
      {family.fixRight + family.fixWrong + family.fixPartly > 0 && (
        <div className="num mt-1.5 flex flex-wrap gap-x-3 border-t border-line-soft pt-1.5 text-2xs text-ink-4">
          <span className="text-ink-3">fixes</span>
          <Count label="right" value={family.fixRight} color="var(--color-ok)" />
          <Count label="nearly" value={family.fixPartly} color="var(--color-accent)" />
          <Count label="wrong" value={family.fixWrong} color="var(--color-warn)" />
        </div>
      )}

      {thin && (
        <p className="num mt-1.5 text-2xs leading-relaxed text-ink-4">
          {decided} judged. Too few to read as a rate — at this sample one more rating moves
          it by {decided > 0 ? Math.round(100 / decided) : 0} points.
        </p>
      )}
    </div>
  );
}

function RecallPanel({ recall }: { recall: RecallScore }) {
  if (recall.incidents === 0) {
    return (
      <Empty
        title="No incidents rated"
        hint="When something breaks, root asks whether it called it. That question is the only way to measure what it misses — a rating on what it said cannot tell you about the thing it never mentioned."
      />
    );
  }

  const score = (recall.called + recall.partly * 0.5) / recall.incidents;
  const thin = recall.incidents < ENOUGH_TO_MEAN_SOMETHING;

  return (
    <div className="rounded-md border border-line bg-page p-2.5">
      <div className="flex items-baseline gap-2">
        <span className="num text-xs text-ink">
          across {recall.incidents} incident{recall.incidents === 1 ? "" : "s"}
        </span>
        <span className="num ml-auto text-[17px] text-ink">{Math.round(score * 100)}%</span>
      </div>

      <Bar
        segments={[
          { value: recall.called, color: "var(--color-ok)" },
          { value: recall.partly, color: "var(--color-accent)" },
          { value: recall.missed, color: "var(--color-down)" },
        ]}
      />

      <div className="num mt-1.5 flex flex-wrap gap-x-3 gap-y-0.5 text-2xs text-ink-4">
        <Count label="called" value={recall.called} color="var(--color-ok)" />
        <Count label="partly" value={recall.partly} color="var(--color-accent)" />
        <Count label="missed" value={recall.missed} color="var(--color-down)" />
      </div>

      {/* Misses are the point of this panel, so they are stated rather than
          left to be read off a bar. */}
      {recall.missed > 0 && (
        <p className="num mt-1.5 text-2xs leading-relaxed text-ink-4">
          {recall.missed} thing{recall.missed === 1 ? "" : "s"} broke that root said nothing
          about. Each one is a rule that does not exist yet.
        </p>
      )}

      {thin && (
        <p className="num mt-1.5 text-2xs leading-relaxed text-ink-4">
          {recall.incidents} incident{recall.incidents === 1 ? "" : "s"}. Far too few to gate a
          decision on — a single estate produces these slowly, which is why backtesting against
          stored history matters more here than waiting.
        </p>
      )}
    </div>
  );
}

function RuleRow({ rule }: { rule: RuleScore }) {
  const decided = rule.real + rule.notAProblem;
  const noisy = decided > 0 && rule.notAProblem > rule.real;

  return (
    <div className="flex items-baseline gap-2 rounded-sm px-1 py-1">
      <span className="num min-w-0 flex-1 truncate text-2xs text-ink-2" title={rule.rule}>
        {rule.rule}
      </span>
      {noisy && <Tag color="var(--color-warn)">noisy</Tag>}
      <span className="num w-12 shrink-0 text-right text-2xs" style={{ color: "var(--color-ok)" }}>
        {rule.real} real
      </span>
      <span
        className="num w-16 shrink-0 text-right text-2xs"
        style={{ color: rule.notAProblem > 0 ? "var(--color-warn)" : "var(--color-ink-4)" }}
      >
        {rule.notAProblem} noise
      </span>
      <span className="num w-14 shrink-0 text-right text-2xs text-ink-3">
        {decided > 0 ? `${Math.round((rule.real / decided) * 100)}%` : "—"}
      </span>
    </div>
  );
}

function Count({ label, value, color }: { label: string; value: number; color?: string }) {
  return (
    <span className="flex items-center gap-1">
      {color && (
        <span
          className="h-[3px] w-2.5 shrink-0 rounded-full"
          style={{ background: color }}
          aria-hidden
        />
      )}
      <span style={color ? { color } : undefined}>{value}</span>
      <span>{label}</span>
    </span>
  );
}

/**
 * The composition, not just the headline rate.
 *
 * A bar rather than a number because the shape carries information the
 * percentage drops: "mostly real with a few unsure" and "half real, half
 * noise" can produce the same figure and mean very different things.
 */
function Bar({ segments }: { segments: { value: number; color: string }[] }) {
  const total = segments.reduce((n, s) => n + s.value, 0);
  if (total === 0) return null;

  return (
    <div className="mt-1.5 flex h-1.5 overflow-hidden rounded-full bg-sunk">
      {segments
        .filter((s) => s.value > 0)
        .map((segment, i) => (
          <span
            key={i}
            className={cx("h-full", i > 0 && "border-l border-page")}
            style={{ width: `${(segment.value / total) * 100}%`, background: segment.color }}
          />
        ))}
    </div>
  );
}

function label(family: string): string {
  switch (family) {
    case "rule":
      return "Deterministic rules";
    case "model":
      return "The assistant";
    case "model+rule":
      return "Assistant citing a rule";
    default:
      return family;
  }
}
