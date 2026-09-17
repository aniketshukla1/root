import { motion } from "motion/react";

import { healthColor, healthLabel } from "../catalog";
import type { Health } from "../types";
import { drawLine, EASE_OUT, enter, reachBar } from "./motion";
import { cx } from "./primitives";

/* ══════════════════════════════════════════════════════════════════ ruler
 * The motif that makes every plot read as an instrument rather than a
 * decoration: minor ticks at the sample interval, majors at a labelled one.
 */
export function Ticks({ count, major = 10 }: { count: number; major?: number }) {
  return (
    <svg
      viewBox={`0 0 ${count} 6`}
      preserveAspectRatio="none"
      className="block h-1.5 w-full"
      aria-hidden
    >
      {Array.from({ length: count + 1 }, (_, i) => (
        <line
          key={i}
          x1={i}
          x2={i}
          y1={0}
          y2={i % major === 0 ? 6 : 3}
          stroke={i % major === 0 ? "var(--color-line-strong)" : "var(--color-line)"}
          strokeWidth={0.6}
          vectorEffect="non-scaling-stroke"
        />
      ))}
    </svg>
  );
}

/* ══════════════════════════════════════════════════════════════════ lanes
 * One row per connection, the window left to right, drawn as continuous RUNS
 * rather than per-sample cells — a period of state, not N samples.
 *
 * Nominal is deliberately quiet and absence is HATCHED rather than merely
 * grey, so a collector that stopped reporting can never be mistaken for a
 * service that is fine. Reading across a lane gives one service's window;
 * reading down a column gives what everything was doing at one moment.
 */
export interface Run {
  health: Health;
  /** Fractions of the window, 0 at the left edge and 1 at now. */
  from: number;
  to: number;
}

export interface Lane {
  id: string;
  name: string;
  health: Health;
  runs: Run[];
}

/** Nominal recedes; only a fault spends saturation. */
function runFill(health: Health): string {
  return health === "healthy" ? "var(--color-lane-ok)" : healthColor(health);
}

export function Lanes({
  lanes,
  labelWidth = 142,
  rowHeight = 13,
  deployAt,
  scrubAt,
  scrubLabel,
  footer,
}: {
  lanes: Lane[];
  labelWidth?: number;
  rowHeight?: number;
  /** Fraction of the window where a deploy landed. */
  deployAt?: number;
  /** Fraction of the window the scrub line sits at. */
  scrubAt?: number;
  scrubLabel?: string;
  footer?: React.ReactNode;
}) {
  return (
    <div className="relative flex min-w-0 flex-col gap-1">
      {/* The overlay spans exactly the strip column, so the deploy marker and
          the scrub line land on the same axis the lanes use. */}
      {(deployAt !== undefined || scrubAt !== undefined) && (
        <div
          className="pointer-events-none absolute top-0 right-0 z-10"
          style={{ left: labelWidth + 10, bottom: footer ? 22 : 0 }}
        >
          {deployAt !== undefined && (
            <div
              className="absolute top-0 bottom-0 border-l border-dashed border-accent opacity-60"
              style={{ left: `${deployAt * 100}%` }}
            />
          )}
          {scrubAt !== undefined && (
            <>
              <div
                className="absolute top-0 bottom-0 w-px bg-accent opacity-70"
                style={{ left: `${scrubAt * 100}%` }}
              />
              {scrubLabel && (
                <span
                  className="num absolute -bottom-5 -translate-x-1/2 rounded-sm bg-accent px-1.5 py-px text-2xs font-semibold text-on-accent"
                  style={{ left: `${scrubAt * 100}%` }}
                >
                  {scrubLabel}
                </span>
              )}
            </>
          )}
        </div>
      )}

      {/* Deliberately not animated.
          These rows carried a staggered fade, and in some subtrees the
          animation never started — the row kept its `initial` opacity of 0 and
          the strip simply never painted. Both the variant form and an explicit
          initial/animate pair failed the same way here while animating
          correctly elsewhere in this file, so the cause is not understood.
          It does not need to be. A lane strip that silently fails to paint is
          this component's worst possible failure: a blank strip is not a
          missing flourish, it is the claim "nothing happened here", and it is
          the wrong one. motion.ts states the rule — motion explains where a
          value came from, it never decorates — and a fade on a row explains
          nothing. The strip's own segments already carry the meaning. */}
      <div className="flex flex-col gap-1">
        {lanes.map((lane) => (
          <div
            key={lane.id}
            className="flex items-center gap-2.5"
          >
            <span
              className="flex shrink-0 items-center gap-1.5"
              style={{ width: labelWidth }}
              title={`${lane.name} — ${healthLabel(lane.health)}`}
            >
              <LaneDot health={lane.health} />
              <span
                className={cx(
                  "num truncate text-2xs",
                  lane.health === "healthy" ? "text-ink-3" : "text-ink",
                )}
              >
                {lane.name}
              </span>
            </span>
            <span className="min-w-0 flex-1">
              <LaneStrip runs={lane.runs} height={rowHeight} />
            </span>
          </div>
        ))}
      </div>

      {footer && (
        <div className="mt-1.5 flex items-center gap-2.5">
          <span className="shrink-0" style={{ width: labelWidth }} />
          <div className="min-w-0 flex-1">{footer}</div>
        </div>
      )}
    </div>
  );
}

export function LaneStrip({ runs, height }: { runs: Run[]; height: number }) {
  const hatch = `hatch-${runs.length}-${Math.round((runs[0]?.from ?? 0) * 1000)}`;
  const needsHatch = runs.some((r) => r.health === "unknown");

  return (
    <svg
      viewBox={`0 0 100 ${height}`}
      preserveAspectRatio="none"
      className="block w-full"
      style={{ height }}
      aria-hidden
    >
      {needsHatch && (
        <defs>
          {/* Absence is drawn, not coloured. Hatching is the one convention
              every reader already knows for "no data". */}
          <pattern
            id={hatch}
            width="5"
            height="5"
            patternUnits="userSpaceOnUse"
            patternTransform="rotate(45)"
          >
            <line x1="0" y1="0" x2="0" y2="5" stroke="var(--color-ink-4)" strokeWidth="1.6" />
          </pattern>
        </defs>
      )}
      {runs.map((run, i) => {
        const x = run.from * 100;
        const w = Math.max(0.4, (run.to - run.from) * 100 - 0.25);
        return run.health === "unknown" ? (
          <g key={i}>
            <rect x={x} y={0} width={w} height={height} fill="var(--color-cell-todo)" />
            <rect x={x} y={0} width={w} height={height} fill={`url(#${hatch})`} />
          </g>
        ) : (
          <rect key={i} x={x} y={0} width={w} height={height} fill={runFill(run.health)} />
        );
      })}
    </svg>
  );
}

function LaneDot({ health }: { health: Health }) {
  const c = healthColor(health);
  return (
    <svg width="7" height="7" viewBox="0 0 10 10" className="shrink-0" aria-hidden>
      {health === "degraded" ? (
        <path d="M5 .6 9.5 9.2H.5Z" fill={c} />
      ) : health === "down" ? (
        <rect x=".6" y=".6" width="8.8" height="8.8" fill={c} />
      ) : health === "unknown" ? (
        <circle cx="5" cy="5" r="3.4" fill="none" stroke={c} strokeWidth="1.9" />
      ) : (
        <circle cx="5" cy="5" r="4" fill={c} />
      )}
    </svg>
  );
}

/* ══════════════════════════════════════════════════════════════════ bullet
 * A measure against its limit. The only honest way to show a utilisation
 * number, because a percentage without its ceiling is a rumour.
 */
export function Bullet({
  value,
  max,
  warnAbove,
  height = 7,
}: {
  value: number;
  max: number;
  warnAbove?: number;
  height?: number;
}) {
  const pct = Math.max(0, Math.min(1, value / max));
  const tone =
    warnAbove === undefined
      ? pct >= 0.9
        ? "var(--color-crit)"
        : "var(--color-series-3)"
      : value >= warnAbove
        ? value >= max * 0.95
          ? "var(--color-crit)"
          : "var(--color-warn)"
        : "var(--color-series-3)";

  return (
    <span className="relative block overflow-hidden rounded-sm bg-sunk" style={{ height }}>
      <motion.span
        variants={reachBar}
        {...enter}
        className="absolute inset-y-0 left-0 origin-left rounded-sm"
        style={{ width: `${pct * 100}%`, background: tone }}
      />
      {warnAbove !== undefined && warnAbove < max && (
        <span
          className="absolute -top-0.5 -bottom-0.5 w-px bg-ink-2 opacity-65"
          style={{ left: `${(warnAbove / max) * 100}%` }}
          title={`Threshold ${warnAbove}`}
        />
      )}
    </span>
  );
}

/* ═══════════════════════════════════════════════════════════ ranked bars
 * Top-N by whatever is hurting. Staggered in reading order.
 */
export function RankBars({
  items,
  format = (v) => v.toLocaleString(),
}: {
  items: { label: string; value: number; tone?: "crit" | "warn" }[];
  format?: (value: number) => string;
}) {
  const max = Math.max(1, ...items.map((i) => i.value));
  return (
    <div className="flex flex-col gap-2.5">
      {items.map((item, i) => (
        <div
          key={item.label}
          className="grid items-center gap-2.5"
          style={{ gridTemplateColumns: "104px minmax(0,1fr) 52px" }}
        >
          <span className="num truncate text-2xs text-ink-2">{item.label}</span>
          <span className="block h-2.5 overflow-hidden rounded-sm bg-sunk">
            <motion.span
              initial={{ scaleX: 0 }}
              animate={{ scaleX: 1 }}
              transition={{ ...EASE_OUT, delay: 0.05 + i * 0.06 }}
              className="block h-full origin-left rounded-sm"
              style={{
                width: `${(item.value / max) * 100}%`,
                background:
                  item.tone === "crit"
                    ? "var(--color-crit)"
                    : item.tone === "warn"
                      ? "var(--color-warn)"
                      : "var(--color-series-1)",
              }}
            />
          </span>
          <span className="num text-right text-2xs text-ink-3">{format(item.value)}</span>
        </div>
      ))}
    </div>
  );
}

/* ════════════════════════════════════════════════════════════════════ arc
 * An error budget, or any single proportion that has a ceiling. Management
 * asks one question of it — how much room is left.
 */
export function Arc({
  pct,
  size = 128,
  stroke = 9,
  tone,
  children,
}: {
  pct: number;
  size?: number;
  stroke?: number;
  tone?: "ok" | "warn" | "crit";
  children?: React.ReactNode;
}) {
  const r = size / 2 - stroke / 2 - 2;
  const circ = 2 * Math.PI * r;
  const color =
    tone === "crit" ? "var(--color-crit)" : tone === "warn" ? "var(--color-warn)" : "var(--color-ok)";

  return (
    <div className="relative" style={{ width: size, height: size }}>
      <svg width={size} height={size} className="-rotate-90 block" aria-hidden>
        <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="var(--color-sunk)" strokeWidth={stroke} />
        <motion.circle
          variants={drawLine}
          {...enter}
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke={color}
          strokeWidth={stroke}
          strokeDasharray={`${((circ * pct) / 100).toFixed(1)} ${circ.toFixed(1)}`}
        />
      </svg>
      {children && <div className="absolute inset-0 grid place-items-center">{children}</div>}
    </div>
  );
}

/* ═══════════════════════════════════════════════════════ stacked columns
 * A year of availability, or a year of spend, in one row.
 */
export function StackBars({
  columns,
  height = 74,
}: {
  columns: { label: string; parts: { value: number; tone: "ok" | "warn" | "crit" }[] }[];
  height?: number;
}) {
  const tones = {
    ok: "var(--color-ok)",
    warn: "var(--color-warn)",
    crit: "var(--color-crit)",
  } as const;
  return (
    <div className="flex items-end gap-1.5">
      {columns.map((col, i) => {
        const total = Math.max(1, col.parts.reduce((sum, p) => sum + p.value, 0));
        return (
          <div key={col.label} className="flex min-w-0 flex-1 flex-col items-center gap-1.5">
            <motion.span
              initial={{ scaleY: 0 }}
              animate={{ scaleY: 1 }}
              transition={{ ...EASE_OUT, delay: 0.05 + i * 0.05 }}
              className="flex w-full origin-bottom flex-col-reverse overflow-hidden rounded-sm bg-sunk"
              style={{ height }}
            >
              {col.parts.map((part, i) =>
                part.value > 0 ? (
                  <span
                    key={i}
                    className="block w-full"
                    style={{ height: `${(part.value / total) * 100}%`, background: tones[part.tone] }}
                  />
                ) : null,
              )}
            </motion.span>
            <span className="num text-2xs text-ink-4">{col.label}</span>
          </div>
        );
      })}
    </div>
  );
}

/* ═════════════════════════════════════════════════════════════ calendar
 * Incidents by day. Darkest is worst; a quiet day is the ground colour, not
 * an absence.
 */
export function Calendar({
  weeks,
  cell = 11,
}: {
  /** Column-major: one array per week, seven levels 0..3. */
  weeks: number[][];
  cell?: number;
}) {
  const fill = [
    "var(--color-cell-todo)",
    "color-mix(in srgb, var(--color-warn) 35%, var(--color-cell-todo))",
    "var(--color-warn)",
    "var(--color-crit)",
  ];
  return (
    <div className="flex gap-[3px]">
      {weeks.map((week, w) => (
        <div key={w} className="flex flex-col gap-[3px]">
          {week.map((level, d) => (
            <motion.span
              key={d}
              initial={{ scale: 0, opacity: 0 }}
              animate={{ scale: 1, opacity: 1 }}
              transition={{
                duration: 0.34,
                ease: [0.3, 1.4, 0.5, 1],
                delay: (w * 7 + d) * 0.006,
              }}
              className="block rounded-xs"
              style={{ width: cell, height: cell, background: fill[Math.min(3, level)] }}
            />
          ))}
        </div>
      ))}
    </div>
  );
}
