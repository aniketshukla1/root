import type { TermLine, Tone } from "./script";

const TONE: Record<Tone, string> = {
  dim: "rgba(255,255,255,0.2)",
  bright: "rgba(255,255,255,0.44)",
  ok: "var(--color-ok)",
  warn: "var(--color-warn)",
  crit: "var(--color-crit)",
};

/**
 * A terminal running far faster than anyone could read, down one edge of the
 * screen. The lines are written twice and the column scrolls by exactly one
 * copy, so it runs without a seam for as long as the splash is up.
 */
export function Terminal({
  lines,
  side,
  seconds,
}: {
  lines: TermLine[];
  side: "left" | "right";
  seconds: number;
}) {
  return (
    <div
      className={`splash-terminal absolute inset-y-0 w-[27%] overflow-hidden ${
        side === "left" ? "left-[2.5%]" : "right-[2.5%] text-right"
      }`}
      aria-hidden
    >
      <div className="a-scroll font-mono text-[10px] leading-[15px] whitespace-pre" style={{ animationDuration: `${seconds}s` }}>
        {[...lines, ...lines].map((line, i) => (
          <div key={i} className="truncate" style={{ color: TONE[line.tone] }}>
            {line.text}
          </div>
        ))}
      </div>
    </div>
  );
}
