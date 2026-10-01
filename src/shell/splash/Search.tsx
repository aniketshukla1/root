import { useEffect, useRef, useState } from "react";
import { motion } from "motion/react";

import { ACT, lockTimes, type FeedPlan } from "./script";

/** A point on screen, in px. */
export interface Point {
  x: number;
  y: number;
}

interface Target {
  key: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

/**
 * The machine hunting.
 *
 * A reticle on top of everything, in screen space, jumping from feed to feed:
 * it finds where a feed is at that instant, snaps around it, and hands over to
 * the feed's own brackets, which travel on with the feed through depth. Each
 * lock sends a line back to the centre — the place the name will form, so the
 * finale is already being drawn while the hunt goes on.
 *
 * Real connections are hunted first. A feed out of frame, or too small to
 * read, is passed over rather than locked where nobody can see it.
 */
export function Search({
  plan,
  onLock,
  centre,
}: {
  plan: FeedPlan[];
  /** A feed has been locked; `at` is where it was on screen. */
  onLock: (key: string, at: Point) => void;
  /** Where the lines run to: the mark's centre, or near it. */
  centre: () => Point;
}) {
  const [target, setTarget] = useState<Target | null>(null);
  const [pings, setPings] = useState<{ id: number; from: Point; to: Point }[]>([]);
  const [count, setCount] = useState(0);
  const locked = useRef(new Set<string>());
  const lastLock = useRef(onLock);
  lastLock.current = onLock;

  useEffect(() => {
    const order = [
      ...plan.filter((f) => f.connectionId),
      ...plan.filter((f) => !f.connectionId && f.plane >= 3),
      ...plan.filter((f) => !f.connectionId && f.plane === 2),
    ];

    const timers = lockTimes().map((at, n) =>
      window.setTimeout(() => {
        const found = pick(order, locked.current);
        if (!found) return;
        locked.current.add(found.key);
        setTarget(found);
        setCount((c) => c + 1);
        const from = { x: found.x + found.w / 2, y: found.y + found.h / 2 };
        setPings((p) => [...p.slice(-5), { id: n, from, to: centre() }]);
        // The reticle is in flight for a beat; the feed's brackets take over
        // as it arrives.
        window.setTimeout(() => lastLock.current(found.key, from), 120);
      }, at),
    );
    const done = window.setTimeout(() => setTarget(null), ACT.searchEnd + 260);
    return () => {
      timers.forEach((t) => window.clearTimeout(t));
      window.clearTimeout(done);
    };
    // The plan and the centre are fixed for the life of one splash.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="pointer-events-none absolute inset-0 z-[5]" aria-hidden>
      <svg className="absolute inset-0 h-full w-full overflow-visible">
        {pings.map((p) => {
          const length = Math.hypot(p.to.x - p.from.x, p.to.y - p.from.y);
          return (
            <line
              key={p.id}
              className="a-ping-line"
              x1={p.from.x}
              y1={p.from.y}
              x2={p.to.x}
              y2={p.to.y}
              stroke="rgba(255,255,255,0.55)"
              strokeWidth={0.8}
              style={{ "--len": length } as React.CSSProperties}
            />
          );
        })}
      </svg>

      {target && (
        <motion.div
          className="splash-reticle"
          initial={false}
          animate={{
            x: target.x - 10,
            y: target.y - 10,
            width: target.w + 20,
            height: target.h + 20,
            opacity: 1,
          }}
          transition={{ type: "spring", stiffness: 520, damping: 38, mass: 0.6 }}
        >
          <i className="tl" />
          <i className="tr" />
          <i className="bl" />
          <i className="br" />
          <span className="splash-reticle-label font-mono text-[9px] tracking-[.18em] text-white/80 uppercase">
            target {target.key.replace("feed-", "0x")} · {Math.round(target.x)},{Math.round(target.y)}
          </span>
        </motion.div>
      )}

      <div
        className="a-wash absolute top-6 left-6 font-mono text-[10px] leading-[16px] tracking-[.18em] text-white/55 uppercase"
        style={{ animationDelay: `${ACT.search - 200}ms` }}
      >
        <div className="text-white/80">root // observation</div>
        <div>feeds&nbsp;&nbsp;&nbsp;&nbsp;{String(plan.length).padStart(2, "0")}</div>
        <div>
          locked&nbsp;&nbsp;&nbsp;<span className="text-white/85">{String(count).padStart(2, "0")}</span>
        </div>
      </div>
    </div>
  );
}

/** The next feed worth locking: not locked yet, in frame, and big enough to read. */
function pick(order: FeedPlan[], locked: Set<string>): Target | null {
  const w = window.innerWidth;
  const h = window.innerHeight;
  for (const feed of order) {
    if (locked.has(feed.key)) continue;
    const el = document.querySelector<HTMLElement>(`[data-feed="${feed.key}"]`);
    if (!el) continue;
    const r = el.getBoundingClientRect();
    const visible = r.left > 8 && r.top > 8 && r.right < w - 8 && r.bottom < h - 40;
    // How bright it really is: its own fade times its plane's depth fog.
    const plane = el.closest<HTMLElement>(".splash-plane");
    const seen =
      Number(getComputedStyle(el).opacity) * (plane ? Number(getComputedStyle(plane).opacity) : 1);
    if (!visible || r.width < 110 || seen < 0.6) continue;
    return { key: feed.key, x: r.left, y: r.top, w: r.width, h: r.height };
  }
  return null;
}
