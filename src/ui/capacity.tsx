import { formatHorizon, formatRate } from "../format";
import type { MetricUnit, Trend } from "../types";

/**
 * The capacity readouts, in one place because they are one answer.
 *
 * Every metric that declares a limit gets a fitted line through its history
 * (see forecast.rs), and these render the two things that line can say: how
 * fast it is moving, and when it arrives.
 *
 * Both refuse to speak when the fit is poor, and that is the whole design.
 * A straight line through a sawtoothing metric still has a slope, and both
 * readouts will happily dress it up — as a rate no percentage could
 * sustain, or as a date. Neither survives contact with someone who checks. The first
 * projection that turns out to be nonsense teaches people to ignore the rest,
 * so a number that cannot be defended is not shown.
 *
 * What IS always shown is the present tense: a metric over its limit reads
 * "now" however ragged its history, because that is a reading rather than a
 * projection and it does not depend on any line fitting anything.
 */

/**
 * Below this the line explains too little of the movement to quote either its
 * slope or its destination. Matches MIN_FIT in forecast.rs — the backend
 * withholds `secondsToLimit` at the same threshold, and having the two drift
 * would mean a row with a horizon but no rate, or the reverse.
 */
const RELIABLE_FIT = 0.25;

/** A fit under this is real but loose — shown, and marked approximate. */
const ROUGH_FIT = 0.5;

/** Inside a week is a this-sprint problem; past it, a planning one. */
const SOON_SECS = 7 * 86_400;

/**
 * When this reaches its limit.
 *
 * Four answers, and they are four different facts rather than one absence
 * rendered four ways:
 *
 *   —        no history to fit yet
 *   unclear  fitted, but too erratic to extrapolate from
 *   steady   fitted, and not heading toward the limit
 *   6d       fitted, heading there, and this is when
 */
export function Horizon({ trend, unit }: { trend?: Trend; unit: MetricUnit }) {
  if (!trend) {
    return (
      <span className="text-ink-4" title="Not enough history yet to project a trend">
        —
      </span>
    );
  }

  if (trend.secondsToLimit !== undefined) {
    const soon = trend.secondsToLimit < SOON_SECS;
    return (
      <span
        style={{ color: soon ? "var(--color-warn)" : "var(--color-ink-3)" }}
        title={describe(trend, unit)}
      >
        {trend.fit < ROUGH_FIT ? "≈" : ""}
        {formatHorizon(trend.secondsToLimit)}
      </span>
    );
  }

  // No projection, and the two reasons are not interchangeable. Reporting
  // "steady" for an erratic metric was wrong in the worst direction: an error
  // rate climbing 39% a day read as holding still.
  if (trend.fit < RELIABLE_FIT) {
    return (
      <span className="text-ink-4" title={describe(trend, unit)}>
        unclear
      </span>
    );
  }

  return (
    <span className="text-ink-4" title={describe(trend, unit)}>
      steady
    </span>
  );
}

/**
 * How fast, over the period the fit covers.
 *
 * This replaces the first-sample-to-last-sample delta that used to sit on
 * metric cards. That number moves with whichever two samples happen to land
 * at the ends of the window — one spike at either edge and a flat metric
 * reports a swing. The fitted slope uses every sample, and is shown only when
 * the line it comes from explains enough of the movement to be worth quoting.
 */
export function Rate({ trend, unit }: { trend?: Trend; unit: MetricUnit }) {
  if (!trend || trend.fit < RELIABLE_FIT) return null;
  return (
    <span className="text-ink-4" title={describe(trend, unit)}>
      {formatRate(trend.perSecond, unit, trend.windowSecs)}
    </span>
  );
}

/**
 * The evidence behind a readout, for the tooltip.
 *
 * The rate is omitted below the fit threshold rather than quoted with a
 * caveat: a number in a tooltip gets read as a number, and the caveat does
 * not travel with it once somebody repeats it in a channel.
 */
function describe(trend: Trend, unit: MetricUnit): string {
  const evidence = [
    `fitted over ${Math.round(trend.windowSecs / 60)}m`,
    `${trend.samples} samples`,
    `R²=${trend.fit.toFixed(2)}`,
  ];

  if (trend.fit < RELIABLE_FIT) {
    return `Too erratic to project from — ${evidence.join(" · ")}`;
  }
  return [formatRate(trend.perSecond, unit, trend.windowSecs), ...evidence].join(" · ");
}
