import { useEffect, useState } from "react";

import { ACT, GLYPHS, rng } from "./script";
import type { Point } from "./Search";

/** A space that keeps its width inside an inline-block. */
const SPACE = "\u00a0";

/**
 * Everything coming home: a pulse from every place the machine locked, and
 * from all round the edge of the frame, each running a gentle curve into the
 * root — the dot the mark grows from — and quickening as it is pulled in.
 *
 * Pulses rather than lines. A line drawn edge to centre stays on screen as a
 * web after its moment has passed; a pulse is only ever where it is now, and
 * the screen is empty again by the time the mark has drawn.
 */
export function Converge({ from, centre }: { from: () => Point[]; centre: () => Point }) {
  const [streaks, setStreaks] = useState<{ d: string; length: number; delay: number; duration: number }[]>([]);
  const [home, setHome] = useState<Point | null>(null);

  useEffect(() => {
    const t = window.setTimeout(() => {
      const to = centre();
      const w = window.innerWidth;
      const h = window.innerHeight;
      const r = rng(7);
      const ring = Array.from({ length: 12 }, (_, i) => {
        const a = (i / 12) * Math.PI * 2 + r() * 0.35;
        return { x: w / 2 + Math.cos(a) * w * 0.56, y: h / 2 + Math.sin(a) * h * 0.6 };
      });
      setHome(to);
      setStreaks(
        [...from(), ...ring].map((p, i) => {
          const dx = to.x - p.x;
          const dy = to.y - p.y;
          const straight = Math.hypot(dx, dy) || 1;
          // Bend each path a little to one side, so they arrive like a
          // current rather than like spokes.
          const bend = (r() - 0.5) * 0.45 * straight;
          const c = { x: (p.x + to.x) / 2 - (dy / straight) * bend, y: (p.y + to.y) / 2 + (dx / straight) * bend };
          return {
            d: `M${p.x},${p.y} Q${c.x},${c.y} ${to.x},${to.y}`,
            length: curveLength(p, c, to),
            delay: Math.round((i % 7) * 46 + r() * 70),
            duration: Math.round(440 + r() * 220),
          };
        }),
      );
    }, ACT.converge);
    return () => window.clearTimeout(t);
    // Fixed for the life of one splash.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <svg className="pointer-events-none absolute inset-0 z-[6] h-full w-full overflow-visible" aria-hidden>
      <g className="splash-glow">
        {streaks.map((s, i) => (
          <path
            key={i}
            className="a-streak"
            d={s.d}
            fill="none"
            stroke="#ffffff"
            strokeWidth={1.5}
            strokeLinecap="round"
            strokeDasharray={`26 ${Math.ceil(s.length + 60)}`}
            style={
              {
                "--from": 26,
                "--to": -Math.ceil(s.length),
                animationDelay: `${s.delay}ms`,
                animationDuration: `${s.duration}ms`,
              } as React.CSSProperties
            }
          />
        ))}
      </g>
      {/* The root answering: a ring for each wave that reaches it. */}
      {home &&
        [380, 560, 760].map((delay) => (
          <circle
            key={delay}
            className="a-ring"
            cx={home.x}
            cy={home.y}
            r={9}
            fill="none"
            stroke="rgba(255,255,255,0.85)"
            strokeWidth={1}
            style={{ animationDelay: `${delay}ms` }}
          />
        ))}
    </svg>
  );
}

/** A quadratic curve's length, by walking it — exact enough for a dash. */
function curveLength(a: Point, c: Point, b: Point): number {
  let length = 0;
  let prev = a;
  for (let i = 1; i <= 24; i++) {
    const t = i / 24;
    const u = 1 - t;
    const next = {
      x: u * u * a.x + 2 * u * t * c.x + t * t * b.x,
      y: u * u * a.y + 2 * u * t * c.y + t * t * b.y,
    };
    length += Math.hypot(next.x - prev.x, next.y - prev.y);
    prev = next;
  }
  return length;
}

/**
 * Text that arrives scrambled and resolves into itself, left to right.
 *
 * Each character holds its final width the whole time — the scramble is drawn
 * over a hidden copy of the real letter — so a proportional face does not
 * shuffle the word about while it decodes.
 */
export function Decode({
  text,
  start,
  each,
  hold,
  className,
  charClassName,
}: {
  text: string;
  /** Milliseconds after mount when the scramble appears. */
  start: number;
  /** Delay between one character settling and the next. */
  each: number;
  /** How long the first character scrambles before it settles. */
  hold: number;
  className?: string;
  charClassName?: string;
}) {
  const [now, setNow] = useState(-1);

  useEffect(() => {
    const born = performance.now();
    const end = start + hold + each * text.length + 60;
    let timer = 0;
    const tick = () => {
      const t = performance.now() - born;
      setNow(t);
      if (t < end) timer = window.setTimeout(tick, 42);
    };
    timer = window.setTimeout(tick, start);
    return () => window.clearTimeout(timer);
  }, [text, start, each, hold]);

  return (
    <span className={className} aria-label={text}>
      {text.split("").map((ch, i) => {
        const shown = now >= start;
        const settled = now >= start + hold + i * each;
        // A new glyph every tick, but the same one for everyone watching the
        // same tick — keyed on time and position, not Math.random.
        const glyph = ch === " " ? SPACE : GLYPHS[Math.floor(Math.abs(Math.sin((Math.floor(now / 42) + 1) * (i + 3) * 12.9898)) * GLYPHS.length)];
        return (
          <span
            key={i}
            aria-hidden
            className={`relative inline-block ${charClassName ?? ""} ${settled ? "" : "splash-unsettled"}`}
            style={{ opacity: shown ? 1 : 0 }}
          >
            <span style={{ visibility: settled ? "visible" : "hidden" }}>{ch === " " ? SPACE : ch}</span>
            {!settled && shown && (
              <span className="absolute inset-0 flex items-center justify-center">{glyph}</span>
            )}
          </span>
        );
      })}
    </span>
  );
}
