import { useCallback, useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import { formatHorizon } from "../format";
import type { View } from "../nav";
import type {
  Caveat,
  FamilyScore,
  RecallScore,
  Replay,
  RuleBehaviour,
  RuleScore,
  Scorecard as Card,
} from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Button, Empty, Panel, Select, Tag, cx } from "../ui/primitives";

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
  connections,
  onNavigate,
  onRefresh,
}: {
  /** Connections a replay can be run against. */
  connections: { id: string; name: string }[];
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
          <>
            <Panel title="Nothing judged yet">
              <Empty
                title="No ratings recorded"
                hint="Rate a finding on the overview or a connection's Config tab, and answer the question that appears on an incident. Both are needed: findings measure what root got right, incidents measure what it missed, and the second cannot be inferred from the first."
              />
              <div className="mt-3 flex gap-1.5">
                <Button onClick={() => onNavigate({ name: "overview" })}>Go to findings</Button>
              </div>
            </Panel>

            {/* Available from the first day, on purpose. A backtest is the one
                measurement that does not have to wait for ratings to
                accumulate — it reads history that is already there. Hiding it
                until somebody has rated something had it exactly backwards. */}
            <Backtest connections={connections} />
          </>
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

            <Backtest connections={connections} />

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

/**
 * Replay today's rules over stored history.
 *
 * Ratings arrive at the speed of incidents — a handful a month on one estate,
 * which is nowhere near enough to gate a decision on. The history is already
 * there, and the judgement record already says which incidents were missed, so
 * the most useful question can be asked immediately: **would the rules as they
 * stand now have caught the thing we recorded as a miss?**
 *
 * Run on demand rather than on a schedule. A replay reads weeks of data out of
 * somebody's production Prometheus, and doing that every few minutes because a
 * screen happens to be open is a poor way to treat infrastructure you are also
 * claiming to look after.
 */
function Backtest({ connections }: { connections: { id: string; name: string }[] }) {
  const [profileId, setProfileId] = useState(connections[0]?.id ?? "");
  const [days, setDays] = useState(14);
  const [replay, setReplay] = useState<Replay>();
  const [error, setError] = useState<string>();
  const [running, setRunning] = useState(false);

  const run = async () => {
    if (!profileId) return;
    setRunning(true);
    setError(undefined);
    try {
      setReplay(await api.runBacktest(profileId, days));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setRunning(false);
    }
  };

  const newlyCaught = replay?.coverage.filter((c) => c.newlyCaught) ?? [];

  return (
    <Panel
      title="Backtest"
      subtitle="today's rules over stored history"
      actions={
        <>
          <Select value={profileId} onChange={(e) => setProfileId(e.target.value)}>
            {connections.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </Select>
          <Select value={String(days)} onChange={(e) => setDays(Number(e.target.value))}>
            <option value="7">7 days</option>
            <option value="14">14 days</option>
            <option value="30">30 days</option>
          </Select>
          <Button onClick={run} disabled={running || !profileId}>
            {running ? "Replaying…" : "Replay"}
          </Button>
        </>
      }
    >
      {error ? (
        <Empty title="Could not replay" hint={error} />
      ) : !replay ? (
        <Empty
          title="Not run"
          hint="Replays the rules as they stand now across this connection's stored history, and scores the result against the incidents you have already judged. Reads from the estate's own Prometheus where one is declared, so it is run on demand rather than on a timer."
        />
      ) : (
        <div className="flex flex-col gap-3">
          {/* The headline, and the only reason this panel exists. */}
          {newlyCaught.length > 0 && (
            <div
              className="rounded-md border border-line bg-page p-3"
              style={{ borderLeftColor: "var(--color-ok)", borderLeftWidth: 2 }}
            >
              <div className="text-[13px] font-medium text-ink">
                {newlyCaught.length} recorded miss{newlyCaught.length === 1 ? "" : "es"} the
                current rules would catch
              </div>
              <div className="mt-2 flex flex-col gap-1">
                {newlyCaught.map((c) => (
                  <div key={c.incidentId} className="num flex items-baseline gap-2 text-2xs">
                    <span className="min-w-0 flex-1 truncate text-ink-2">{c.headline}</span>
                    <span className="shrink-0 text-ink-3">{c.rule}</span>
                    <span className="w-20 shrink-0 text-right" style={{ color: "var(--color-ok)" }}>
                      {c.leadSecs !== undefined ? `${formatHorizon(c.leadSecs)} ahead` : "—"}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}

          <div className="num flex flex-wrap gap-x-4 gap-y-1 text-2xs text-ink-4">
            <span>
              {replay.samples.toLocaleString()} samples over{" "}
              {Math.round(
                (Date.parse(replay.to) - Date.parse(replay.from)) / 3_600_000,
              )}
              h
            </span>
            <span>{replay.evaluations} evaluations</span>
            <span>{replay.episodes.length} episodes</span>
          </div>

          {replay.behaviour.length > 0 ? (
            <div className="flex flex-col gap-1">
              {replay.behaviour.map((b) => (
                <BehaviourRow key={b.rule} behaviour={b} />
              ))}
            </div>
          ) : (
            <p className="num text-2xs leading-relaxed text-ink-4">
              Nothing fired across the window. That is a statement about these rules on this
              history, not a clean bill of health — read it beside the caveats below.
            </p>
          )}

          {/* Never optional. A replay that does not say what it could not see
              is a number people would over-trust. */}
          <div className="flex flex-col gap-1 border-t border-line pt-2">
            {replay.caveats.map((caveat, i) => (
              <p key={i} className="num text-2xs leading-relaxed text-ink-4">
                {describeCaveat(caveat)}
              </p>
            ))}
          </div>
        </div>
      )}
    </Panel>
  );
}

function BehaviourRow({ behaviour }: { behaviour: RuleBehaviour }) {
  // Above a third of the window, a rule has stopped being a warning and become
  // part of the background — correct or not, nobody sees the one time it
  // matters.
  const wallpaper = behaviour.dutyCycle > 0.33;

  return (
    <div className="flex items-baseline gap-2 rounded-sm px-1 py-1">
      <span className="num min-w-0 flex-1 truncate text-2xs text-ink-2" title={behaviour.rule}>
        {behaviour.rule}
      </span>
      {wallpaper && <Tag color="var(--color-warn)">always on</Tag>}
      <span className="num w-20 shrink-0 text-right text-2xs text-ink-3">
        {behaviour.episodes} episode{behaviour.episodes === 1 ? "" : "s"}
      </span>
      <span
        className="num w-24 shrink-0 text-right text-2xs"
        style={{ color: wallpaper ? "var(--color-warn)" : "var(--color-ink-3)" }}
        title="Share of the window this rule spent firing"
      >
        {(behaviour.dutyCycle * 100).toFixed(1)}% of window
      </span>
    </div>
  );
}

function describeCaveat(caveat: Caveat): string {
  switch (caveat.kind) {
    case "configIsCurrent":
      return "Configuration has no history, so this measured past metrics against today's settings. If a limit was changed during the window, the result is wrong about everything before the change.";
    case "thinHistory":
      return `Only ${caveat.samples} samples. Too few for a trend, so the rules that project could not have fired whatever the data did — which is not the same as nothing being wrong.`;
    case "noHistorySource":
      return "This connection declares no long-range history source, so there was nothing to replay. Point it at a Prometheus in Settings to make this work.";
  }
}
