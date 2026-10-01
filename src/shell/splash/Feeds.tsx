import { useMemo } from "react";

import { healthColor } from "../../catalog";
import type { Health } from "../../types";
import { PLANES, rng, type FeedArt, type FeedPlan } from "./script";

/** What the splash knows about a connection that has answered. */
export interface Answer {
  health: Health;
  latencyMs: number;
  name: string;
}

/** The machine's judgement on one feed: when it locked, and what it said. */
export interface Lock {
  /** Milliseconds after the splash mounted. */
  at: number;
  verdict: string;
  tone: string;
}

/**
 * The footage.
 *
 * With the camera, feeds sit still on five planes at different depths and the
 * camera flies forward through them: near planes sweep past at the edges,
 * far ones grow out of the haze, and the parallax between them is what makes
 * it read as space rather than as cards sliding about. Without it, each feed
 * is carried by its own keyframes past a fixed view.
 *
 * With imagery, a feed is framed like a security camera — its number, a
 * recording light, a timecode, a band rolling down the picture — and mostly
 * shows what such a camera would: racks, aisles, the estate's globe.
 */
export function Feeds({
  plan,
  answers,
  locks,
  camera,
  imagery,
  search,
}: {
  plan: FeedPlan[];
  answers: Map<string, Answer>;
  locks: Map<string, Lock>;
  camera: boolean;
  imagery: boolean;
  search: boolean;
}) {
  // Plane positions are worked out once, against the window as it opened.
  const viewport = useMemo(
    () => ({ w: window.innerWidth || 1280, h: window.innerHeight || 800 }),
    [],
  );

  const render = (feed: FeedPlan, placed?: { x: number; y: number }) => (
    <Feed
      key={feed.key}
      feed={feed}
      answer={feed.connectionId ? answers.get(feed.connectionId) : undefined}
      lock={search ? locks.get(feed.key) : undefined}
      search={search}
      imagery={imagery}
      placed={placed}
    />
  );

  if (!camera) {
    return (
      <div className="splash-depth absolute inset-0 overflow-hidden" aria-hidden>
        {plan.map((feed) => render(feed))}
      </div>
    );
  }

  return (
    <div className="splash-depth absolute inset-0 overflow-hidden" aria-hidden>
      <div className="splash-camera a-camera">
        {PLANES.map((z, p) => {
          // How much wider than the screen this plane's visible area is, from
          // where the camera starts — so a feed placed at the edge of the
          // frame starts at the edge of the frame.
          const spread = (900 - (z - 400)) / 900;
          return (
            <div
              key={z}
              className="splash-plane"
              style={{
                transform: `translateZ(${z}px)`,
                opacity: [0.36, 0.55, 0.78, 0.92, 1][p],
                // Depth of field: the far plane is soft, the rest are sharp.
                filter: p === 0 ? "blur(1.6px)" : undefined,
              }}
            >
              {plan
                .filter((feed) => feed.plane === p)
                .map((feed) =>
                  render(feed, {
                    x: feed.sx * (viewport.w / 2) * spread * 0.78,
                    y: feed.sy * (viewport.h / 2) * spread * 0.78,
                  }),
                )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Feed({
  feed,
  answer,
  lock,
  search,
  imagery,
  placed,
}: {
  feed: FeedPlan;
  answer?: Answer;
  lock?: Lock;
  search: boolean;
  imagery: boolean;
  placed?: { x: number; y: number };
}) {
  const tone = answer ? healthColor(answer.health) : "#ffffff";

  const style = placed
    ? ({
        "--px": `${placed.x}px`,
        "--py": `${placed.y}px`,
        animationDelay: `${feed.appear}ms`,
      } as React.CSSProperties)
    : ({
        "--x": feed.x,
        "--y": feed.y,
        "--rx": `${feed.rotateX}deg`,
        "--ry": `${feed.rotateY}deg`,
        animationDelay: `${feed.delay}ms`,
        animationDuration: `${feed.duration}ms`,
      } as React.CSSProperties);

  const motion = placed ? "a-appear" : feed.approach ? "a-approach" : "a-recede";

  return (
    <div
      data-feed={feed.key}
      className={`splash-feed ${motion} ${imagery ? "splash-cctv" : ""}`}
      style={style}
    >
      {imagery ? (
        <div className="flex items-center justify-between gap-3 font-mono text-[9px] tracking-[.16em] text-white/70 uppercase splash-aberrate">
          <span className="truncate">
            {feed.camera} · {feed.connectionId ? feed.title.split(" · ")[1] : feed.title.split(" · ")[1]}
          </span>
          <span className="flex shrink-0 items-center gap-1.5 text-white/45">
            <i className="splash-rec" /> rec {feed.stamp}
          </span>
        </div>
      ) : (
        <div className="flex items-center justify-between gap-3 font-mono text-[9px] tracking-[.16em] text-white/60 uppercase">
          <span className="truncate">{feed.title}</span>
          <span className="shrink-0 text-white/35">{feed.stamp}</span>
        </div>
      )}

      <Art art={feed.art} seed={feed.seed} />

      {imagery && <span className="splash-band" style={{ animationDelay: `${(feed.seed % 7) * 230}ms` }} aria-hidden />}

      {search ? (
        lock && (
          <>
            <span className="splash-lock" style={{ "--lock": lock.tone } as React.CSSProperties}>
              <i className="tl" />
              <i className="tr" />
              <i className="bl" />
              <i className="br" />
            </span>
            <Verdict lock={lock} />
          </>
        )
      ) : (
        <>
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
              className="splash-tag font-mono text-[9px] tracking-[.18em] uppercase"
              style={{ color: tone, animationDelay: `${feed.delay + 300}ms` }}
            >
              {answer ? `${answer.health} · ${Math.round(answer.latencyMs)} ms` : "acquiring…"}
            </span>
          )}
        </>
      )}
    </div>
  );
}

/**
 * The machine thinking out loud: "assessing", then its verdict typed over it.
 * Mounted at the moment of the lock, so every delay here is counted from it.
 */
function Verdict({ lock }: { lock: Lock }) {
  return (
    <span className="splash-verdict font-mono text-[9px] tracking-[.18em] uppercase">
      <span className="splash-assessing text-white/70">assessing…</span>
      <span className="absolute left-0 top-0 whitespace-nowrap" style={{ color: lock.tone }}>
        {lock.verdict.split("").map((ch, i) => (
          <span key={i} className="a-char" style={{ animationDelay: `${380 + i * 22}ms` }}>
            {ch}
          </span>
        ))}
      </span>
    </span>
  );
}

const W = 216;
const H = 100;

/** One picture, generated from the feed's seed. */
function Art({ art, seed }: { art: FeedArt; seed: number }) {
  const body = useMemo(() => draw(art, seed), [art, seed]);
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="mt-2 block h-[124px] w-[270px] overflow-hidden" fill="none">
      {body}
    </svg>
  );
}

const INK = "rgba(255,255,255,0.72)";
const FAINT = "rgba(255,255,255,0.12)";

function draw(art: FeedArt, seed: number): React.ReactNode {
  const r = rng(seed);

  const grid = (
    <g stroke={FAINT} strokeWidth={0.5}>
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
          <polyline points={points.join(" ")} stroke={INK} strokeWidth={1.1} />
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
              <rect key={i} x={i * 7.2 + 1} y={H - h} width={4.6} height={h} fill={hot ? "var(--color-warn)" : "rgba(255,255,255,0.55)"} />
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
            <circle key={i} cx={x} cy={y} r={i === 0 ? 4 : 2.6} fill={i === 0 ? "#fff" : "#000"} stroke={r() < 0.12 ? "var(--color-crit)" : INK} strokeWidth={1} />
          ))}
        </>
      );
    }
    case "log":
      return (
        <g fontFamily="ui-monospace, SF Mono, Menlo, monospace" fontSize={7.2}>
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
                <rect key={`${row}-${col}`} x={col * 12} y={row * 16.5} width={10.5} height={15} fill={v > 0.94 ? "var(--color-crit)" : `rgba(255,255,255,${(0.04 + v * 0.5).toFixed(2)})`} />
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
                <rect key={`${row}-${col}`} x={col * 13.5 + 1} y={row * 19 + 4} width={11} height={11} rx={1.5} fill={fill} stroke={v < 0.12 ? "none" : "rgba(95,196,140,0.7)"} strokeWidth={0.8} />
              );
            }),
          )}
        </>
      );
    case "rack":
      return <Rack r={r} />;
    case "aisle":
      return <Aisle r={r} />;
    case "globe":
      return <Globe r={r} />;
  }
}

/** A light on a front panel, blinking at its own rate. */
function Led({ x, y, r }: { x: number; y: number; r: () => number }) {
  const v = r();
  const color = v < 0.04 ? "var(--color-crit)" : v < 0.12 ? "var(--color-warn)" : "var(--color-ok)";
  return (
    <circle
      className={r() < 0.6 ? "splash-led" : undefined}
      cx={x}
      cy={y}
      r={1.15}
      fill={color}
      style={{ animationDelay: `${Math.floor(r() * 900)}ms`, animationDuration: `${300 + Math.floor(r() * 900)}ms` }}
    />
  );
}

/** A rack seen face on, its neighbours half in frame either side. */
function Rack({ r }: { r: () => number }) {
  const rack = (x0: number, width: number, dim: number) => (
    <g opacity={dim}>
      <rect x={x0} y={3} width={width} height={94} stroke="rgba(255,255,255,0.55)" strokeWidth={0.8} />
      {Array.from({ length: 9 }, (_, u) => {
        const y = 7 + u * 10;
        const tall = r() < 0.18;
        return (
          <g key={u}>
            <rect x={x0 + 3} y={y} width={width - 6} height={tall ? 8.5 : 7.5} stroke="rgba(255,255,255,0.4)" strokeWidth={0.6} fill="rgba(255,255,255,0.03)" />
            {Array.from({ length: Math.floor((width - 30) / 9) }, (_, k) => (
              <line key={k} x1={x0 + 22 + k * 9} x2={x0 + 22 + k * 9} y1={y + 1.5} y2={y + 6} stroke="rgba(255,255,255,0.2)" strokeWidth={0.6} />
            ))}
            <Led x={x0 + 8} y={y + 3.8} r={r} />
            <Led x={x0 + 12.5} y={y + 3.8} r={r} />
            {r() < 0.5 && <Led x={x0 + 17} y={y + 3.8} r={r} />}
          </g>
        );
      })}
    </g>
  );
  return (
    <>
      {rack(-38, 64, 0.45)}
      {rack(70, 76, 1)}
      {rack(190, 64, 0.45)}
      <text x={72} y={-1} fill="rgba(255,255,255,0.5)" fontFamily="ui-monospace, SF Mono, Menlo, monospace" fontSize={6}>
        {`B${String(1 + Math.floor(r() * 30)).padStart(2, "0")}`}
      </text>
    </>
  );
}

/** A data-centre aisle down the middle of the frame, racks either side. */
function Aisle({ r }: { r: () => number }) {
  const vx = 108;
  const vy = 46;
  // Rack faces either side: verticals converging on the vanishing point.
  const depths = [0, 0.32, 0.54, 0.69, 0.79, 0.86, 0.91];
  const side = (dir: -1 | 1) => (
    <g>
      {depths.map((t, i) => {
        const x = dir < 0 ? 0 + (vx - 0) * t : W - (W - vx) * t;
        const top = 0 + (vy - 0) * t;
        const bottom = H - (H - vy) * t;
        return (
          <g key={i}>
            <line x1={x} y1={top} x2={x} y2={bottom} stroke="rgba(255,255,255,0.4)" strokeWidth={0.7} />
            {i < depths.length - 1 &&
              Array.from({ length: 5 }, (_, k) => {
                const y = top + ((bottom - top) * (k + 0.6)) / 5.5;
                return <Led key={k} x={x + dir * -2.5 * (1 - t)} y={y} r={r} />;
              })}
          </g>
        );
      })}
      <line x1={dir < 0 ? 0 : W} y1={0} x2={vx} y2={vy} stroke="rgba(255,255,255,0.35)" strokeWidth={0.7} />
      <line x1={dir < 0 ? 0 : W} y1={H} x2={vx} y2={vy} stroke="rgba(255,255,255,0.35)" strokeWidth={0.7} />
    </g>
  );
  return (
    <>
      {side(-1)}
      {side(1)}
      {/* Floor tiles and the cable tray overhead. */}
      {[0.35, 0.58, 0.74, 0.85].map((t) => (
        <line key={t} x1={vx - (vx - 30) * (1 - t)} x2={vx + (W - 30 - vx) * (1 - t)} y1={H - (H - vy) * t} y2={H - (H - vy) * t} stroke="rgba(255,255,255,0.14)" strokeWidth={0.6} />
      ))}
      <line x1={70} y1={0} x2={vx - 6} y2={vy - 4} stroke="rgba(255,255,255,0.22)" strokeWidth={0.6} />
      <line x1={146} y1={0} x2={vx + 6} y2={vy - 4} stroke="rgba(255,255,255,0.22)" strokeWidth={0.6} />
      <rect x={vx - 7} y={vy - 5} width={14} height={10} fill="rgba(255,255,255,0.08)" stroke="rgba(255,255,255,0.3)" strokeWidth={0.5} />
    </>
  );
}

/** The estate's globe: lit points turning under a graticule, arcs between them. */
function Globe({ r }: { r: () => number }) {
  const cx = 108;
  const cy = 50;
  const R = 44;
  const id = `g${Math.floor(r() * 1e9)}`;
  // A strip twice the globe's width, scrolled under a circular window: at
  // this size it reads as the globe turning.
  const dots = Array.from({ length: 150 }, () => {
    const lon = r();
    const lat = (r() - 0.5) * 1.7;
    return { x: lon * 4 * R, y: cy + Math.sin(lat) * R * 0.95, lit: r() < 0.08 };
  });
  return (
    <>
      <defs>
        <clipPath id={id}>
          <circle cx={cx} cy={cy} r={R} />
        </clipPath>
        <radialGradient id={`${id}s`} cx="38%" cy="34%" r="70%">
          <stop offset="0%" stopColor="rgba(255,255,255,0.10)" />
          <stop offset="100%" stopColor="rgba(255,255,255,0)" />
        </radialGradient>
      </defs>
      <circle cx={cx} cy={cy} r={R} fill={`url(#${id}s)`} stroke="rgba(255,255,255,0.55)" strokeWidth={0.8} />
      <g clipPath={`url(#${id})`}>
        <g className="splash-spin">
          {[0, 2 * R].map((offset) =>
            dots.map((d, i) => (
              <circle
                key={`${offset}-${i}`}
                cx={cx - 2 * R + d.x + offset}
                cy={d.y}
                r={d.lit ? 1.5 : 0.8}
                fill={d.lit ? "var(--color-accent)" : "rgba(255,255,255,0.45)"}
              />
            )),
          )}
        </g>
        {[-0.6, -0.3, 0, 0.3, 0.6].map((t) => (
          <ellipse key={t} cx={cx} cy={cy + t * R} rx={R * Math.cos(Math.asin(t))} ry={3.2} stroke="rgba(255,255,255,0.14)" strokeWidth={0.5} />
        ))}
        {[0.25, 0.55, 0.85].map((t) => (
          <ellipse key={t} cx={cx} cy={cy} rx={R * t} ry={R} stroke="rgba(255,255,255,0.14)" strokeWidth={0.5} />
        ))}
      </g>
      {Array.from({ length: 3 }, (_, i) => {
        const a = { x: cx + (r() - 0.5) * R * 1.4, y: cy + (r() - 0.5) * R * 1.2 };
        const b = { x: cx + (r() - 0.5) * R * 1.4, y: cy + (r() - 0.5) * R * 1.2 };
        const lift = 18 + r() * 18;
        return (
          <path
            key={i}
            className="splash-arc"
            d={`M${a.x},${a.y} Q${(a.x + b.x) / 2},${Math.min(a.y, b.y) - lift} ${b.x},${b.y}`}
            stroke="var(--color-accent)"
            strokeWidth={0.9}
            strokeDasharray="4 3"
            style={{ animationDelay: `${i * 260}ms` }}
          />
        );
      })}
      <text x={4} y={96} fill="rgba(255,255,255,0.45)" fontFamily="ui-monospace, SF Mono, Menlo, monospace" fontSize={6}>
        {`${(r() * 90).toFixed(4)}N ${(r() * 180).toFixed(4)}W`}
      </text>
    </>
  );
}
