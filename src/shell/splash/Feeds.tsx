import { useMemo } from "react";

import { healthColor } from "../../catalog";
import type { Health } from "../../types";
import { rng, type FeedArt, type FeedPlan } from "./script";

/** What the splash knows about a connection that has answered. */
export interface Answer {
  health: Health;
  latencyMs: number;
  name: string;
}

/**
 * Monitoring footage, flying through depth: feeds rush out of the dark and
 * fall away behind the title the way surveillance stills do in a film's
 * opening. Each is a small, real-looking instrument rather than a picture —
 * drawn in code, so there is nothing to load and nothing licensed.
 *
 * The ones watching a real connection lock on with that connection's name,
 * and their brackets take its health the moment it answers.
 */
export function Feeds({
  plan,
  answers,
}: {
  plan: FeedPlan[];
  answers: Map<string, Answer>;
}) {
  return (
    <div className="splash-depth absolute inset-0 overflow-hidden" aria-hidden>
      {plan.map((feed) => (
        <Feed
          key={feed.key}
          feed={feed}
          answer={feed.connectionId ? answers.get(feed.connectionId) : undefined}
        />
      ))}
    </div>
  );
}

function Feed({ feed, answer }: { feed: FeedPlan; answer?: Answer }) {
  const tone = answer ? healthColor(answer.health) : "#ffffff";
  const style = {
    "--x": feed.x,
    "--y": feed.y,
    "--rx": `${feed.rotateX}deg`,
    "--ry": `${feed.rotateY}deg`,
    animationDelay: `${feed.delay}ms`,
    animationDuration: `${feed.duration}ms`,
  } as React.CSSProperties;

  return (
    <div className={`splash-feed ${feed.approach ? "a-approach" : "a-recede"}`} style={style}>
      <div className="flex items-center justify-between gap-3 font-mono text-[9px] tracking-[.16em] text-white/60 uppercase">
        <span className="truncate">{feed.title}</span>
        <span className="shrink-0 text-white/35">{feed.stamp}</span>
      </div>
      <Art art={feed.art} seed={feed.seed} />

      {feed.lock && (
        <span
          className="splash-lock"
          style={{ "--lock": tone, animationDelay: `${feed.delay + 170}ms` } as React.CSSProperties}
        >
          <i className="tl" />
          <i className="tr" />
          <i className="bl" />
          <i className="br" />
        </span>
      )}

      {feed.connectionId && (
        <span
          className="splash-tag font-mono text-[8px] tracking-[.18em] uppercase"
          style={{ color: tone, animationDelay: `${feed.delay + 300}ms` }}
        >
          {answer
            ? `${answer.health} · ${Math.round(answer.latencyMs)} ms`
            : "acquiring…"}
        </span>
      )}
    </div>
  );
}

const W = 216;
const H = 100;

/** One instrument, generated from the feed's seed. */
function Art({ art, seed }: { art: FeedArt; seed: number }) {
  const body = useMemo(() => draw(art, seed), [art, seed]);
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="mt-2 block h-[124px] w-[270px]" fill="none">
      {body}
    </svg>
  );
}

function draw(art: FeedArt, seed: number): React.ReactNode {
  const r = rng(seed);
  const ink = "rgba(255,255,255,0.72)";
  const faint = "rgba(255,255,255,0.12)";

  const grid = (
    <g stroke={faint} strokeWidth={0.5}>
      {[25, 50, 75].map((y) => (
        <line key={`h${y}`} x1={0} x2={W} y1={y} y2={y} />
      ))}
      {[54, 108, 162].map((x) => (
        <line key={`v${x}`} x1={x} x2={x} y1={0} y2={H} />
      ))}
    </g>
  );

  switch (art) {
    case "line": {
      let v = 40 + r() * 30;
      const points = Array.from({ length: 48 }, (_, i) => {
        v = Math.max(8, Math.min(92, v + (r() - 0.5) * 16));
        return `${(i / 47) * W},${v}`;
      });
      const spike = Math.floor(16 + r() * 24);
      return (
        <>
          {grid}
          <polygon points={`0,${H} ${points.join(" ")} ${W},${H}`} fill="rgba(255,255,255,0.05)" />
          <polyline points={points.join(" ")} stroke={ink} strokeWidth={1.1} />
          <line x1={(spike / 47) * W} x2={(spike / 47) * W} y1={0} y2={H} stroke="rgba(255,255,255,0.35)" strokeDasharray="2 3" />
        </>
      );
    }
    case "bars":
      return (
        <>
          {grid}
          {Array.from({ length: 30 }, (_, i) => {
            const h = 8 + r() * 80;
            const hot = r() < 0.07;
            return (
              <rect
                key={i}
                x={i * 7.2 + 1}
                y={H - h}
                width={4.6}
                height={h}
                fill={hot ? "var(--color-warn)" : "rgba(255,255,255,0.55)"}
              />
            );
          })}
        </>
      );
    case "graph": {
      const nodes = Array.from({ length: 8 }, () => [12 + r() * (W - 24), 10 + r() * (H - 20)] as const);
      return (
        <>
          {nodes.slice(1).map(([x, y], i) => {
            const [px, py] = nodes[Math.floor(r() * (i + 1))];
            return <line key={i} x1={px} y1={py} x2={x} y2={y} stroke="rgba(255,255,255,0.35)" strokeWidth={0.8} />;
          })}
          {nodes.map(([x, y], i) => (
            <circle
              key={i}
              cx={x}
              cy={y}
              r={i === 0 ? 4 : 2.6}
              fill={i === 0 ? "#fff" : "#000"}
              stroke={r() < 0.12 ? "var(--color-crit)" : ink}
              strokeWidth={1}
            />
          ))}
        </>
      );
    }
    case "log":
      return (
        <g fontFamily="IBM Plex Mono, monospace" fontSize={7.2}>
          {Array.from({ length: 8 }, (_, i) => {
            const level = r();
            const tag = level < 0.1 ? "ERROR" : level < 0.22 ? "WARN " : "INFO ";
            const fill = level < 0.1 ? "var(--color-crit)" : level < 0.22 ? "var(--color-warn)" : "rgba(255,255,255,0.6)";
            return (
              <text key={i} x={0} y={10 + i * 12} fill={fill}>
                {`${tag} ${Math.floor(r() * 9999).toString().padStart(4, "0")} ${["conn reset", "gc 41ms", "ack 200", "retry 2/5", "flush wal", "sync ok", "lease renew"][Math.floor(r() * 7)]}`}
              </text>
            );
          })}
        </g>
      );
    case "heat":
      return (
        <>
          {Array.from({ length: 6 }, (_, row) =>
            Array.from({ length: 18 }, (_, col) => {
              const v = r();
              return (
                <rect
                  key={`${row}-${col}`}
                  x={col * 12}
                  y={row * 16.5}
                  width={10.5}
                  height={15}
                  fill={v > 0.94 ? "var(--color-crit)" : `rgba(255,255,255,${(0.04 + v * 0.5).toFixed(2)})`}
                />
              );
            }),
          )}
        </>
      );
    case "pips":
      return (
        <>
          {Array.from({ length: 5 }, (_, row) =>
            Array.from({ length: 16 }, (_, col) => {
              const v = r();
              const fill = v < 0.05 ? "var(--color-crit)" : v < 0.12 ? "var(--color-warn)" : "rgba(95,196,140,0.35)";
              return (
                <rect
                  key={`${row}-${col}`}
                  x={col * 13.5 + 1}
                  y={row * 19 + 4}
                  width={11}
                  height={11}
                  rx={1.5}
                  fill={fill}
                  stroke={v < 0.12 ? "none" : "rgba(95,196,140,0.7)"}
                  strokeWidth={0.8}
                />
              );
            }),
          )}
        </>
      );
  }
}
