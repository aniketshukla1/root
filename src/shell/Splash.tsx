import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  motion,
  useIsPresent,
  useReducedMotion,
  type TargetAndTransition,
} from "motion/react";

import { healthColor } from "../catalog";
import type { Health, HealthSnapshot } from "../types";
import { Feeds, type Answer, type Lock } from "./splash/Feeds";
import { Converge, Decode } from "./splash/Finale";
import { grainUrl } from "./splash/grain";
import {
  ACT,
  ALL_FEATURES,
  VERDICTS,
  feedPlan,
  terminalLines,
  type SplashFeatures,
} from "./splash/script";
import { Search, type Point } from "./splash/Search";
import { playScore, readSplashSound } from "./splash/sound";
import { Terminal } from "./splash/Terminal";

/**
 * The boot screen, as a title sequence.
 *
 * root is named for the one who sees everything, and it opens the way a film
 * about that would. In the dark a command types itself. The camera flies
 * forward through planes of footage — racks, aisles, the estate's globe,
 * its instruments — while terminals run faster than anyone could read. The
 * machine hunts through it, locking onto feed after feed and typing its
 * verdict, each lock sending a line back to the centre. A scan crosses the
 * screen, every line converges, the mark draws itself where they meet, and
 * the name decodes out of noise.
 *
 * It is spectacle, but not invented spectacle. Feeds are named after the
 * connections actually configured, the machine's verdict on a connection is
 * its real health and latency, and the last line counts what answered. The
 * cinema is how the facts are shown, never a replacement for them.
 *
 * It leaves the way the app arrives: the mark flies to where the sidebar
 * keeps it, becoming the sidebar's mark on the way.
 *
 * Every upgrade over the first version can be switched off (`features`), so
 * each can be judged on its own; the app runs with all of them.
 */

/** A configured connection, by what the splash shows of it. */
export interface SplashConnection {
  id: string;
  name: string;
  kind?: string;
}

interface Arrival extends Answer {
  id: string;
  /** Milliseconds after the splash mounted. */
  at: number;
}

const LEAVE = [0.4, 0, 1, 1] as const;
/** The root of the mark, in its 24-unit box. */
const ROOT_DOT = { x: 5.6, y: 3.7 } as const;
const TAGLINE = "Infrastructure intelligence";
const COMMAND = "observe --everything";

export function Splash({
  workspaceLoaded,
  catalogLoaded,
  connections,
  snapshots,
  workspace,
  ready,
  onSkip,
  features,
}: {
  workspaceLoaded: boolean;
  catalogLoaded: boolean;
  /** Connections configured, in the order the config lists them. */
  connections: SplashConnection[];
  /** Snapshots returned so far. A connection has answered once it has one. */
  snapshots: HealthSnapshot[];
  workspace?: string;
  /** The app can already stand on its own, so a key or a click may end this. */
  ready: boolean;
  onSkip: () => void;
  /** Upgrades to switch off, for judging them one at a time. */
  features?: Partial<SplashFeatures>;
}) {
  const reduce = useReducedMotion() ?? false;
  const present = useIsPresent();
  const born = useRef(performance.now());
  const markBox = useRef<HTMLDivElement>(null);
  const [arrivals, setArrivals] = useState<Arrival[]>([]);
  const [locks, setLocks] = useState<Map<string, { at: number; point: Point }>>(new Map());

  // Fixed for the life of one splash: switching mid-sequence would only
  // produce a sequence nobody designed.
  const [f] = useState<SplashFeatures>(() => ({
    ...ALL_FEATURES,
    sound: readSplashSound(),
    ...features,
  }));

  const configured = useMemo(() => new Set(connections.map((c) => c.id)), [connections]);

  // Stamp each first answer as it lands.
  useEffect(() => {
    setArrivals((previous) => {
      const seen = new Set(previous.map((a) => a.id));
      const fresh = snapshots.filter(
        (s) => configured.has(s.profileId) && !seen.has(s.profileId),
      );
      if (fresh.length === 0) return previous;
      const at = performance.now() - born.current;
      return [
        ...previous,
        ...fresh.map((s) => ({
          id: s.profileId,
          name: s.profileName,
          health: s.health,
          latencyMs: s.latencyMs,
          at,
        })),
      ];
    });
  }, [snapshots, configured]);

  // Once the app can stand on its own, whoever is waiting may end the wait.
  useEffect(() => {
    if (!ready || !present) return;
    const skip = () => onSkip();
    window.addEventListener("keydown", skip);
    window.addEventListener("pointerdown", skip);
    return () => {
      window.removeEventListener("keydown", skip);
      window.removeEventListener("pointerdown", skip);
    };
  }, [ready, present, onSkip]);

  // The score, once, from the first frame; faded out if the splash leaves early.
  useEffect(() => {
    if (!f.sound) return;
    return playScore();
  }, [f.sound]);

  // The material, seeded by the estate itself so it opens the same way each
  // launch — and differently for a different estate.
  const names = useMemo(() => connections.map((c) => c.name), [connections]);
  const kinds = useMemo(
    () => [...new Set(connections.map((c) => c.kind).filter((k): k is string => Boolean(k)))],
    [connections],
  );
  const seed = useMemo(() => hash(names.join("|")) || 1979, [names]);
  const plan = useMemo(() => feedPlan(connections, seed, f.imagery), [connections, seed, f.imagery]);
  const byKey = useMemo(() => new Map(plan.map((p) => [p.key, p])), [plan]);
  const left = useMemo(() => terminalLines(seed + 1, names, kinds, 110), [seed, names, kinds]);
  const right = useMemo(() => terminalLines(seed + 2, names, kinds, 110), [seed, names, kinds]);
  const grain = useMemo(() => grainUrl(), []);

  const answers = useMemo(() => new Map(arrivals.map((a) => [a.id, a])), [arrivals]);
  const total = connections.length;
  const answered = arrivals.length;
  const allIn = total > 0 && answered >= total;

  // What the machine says about each feed it has locked. A real connection's
  // verdict is its health, and it updates if the answer lands after the lock.
  const verdicts = useMemo(() => {
    const out = new Map<string, Lock>();
    for (const [key, lock] of locks) {
      const feed = byKey.get(key);
      const name = feed?.title.split(" · ")[1] ?? key;
      const answer = feed?.connectionId ? answers.get(feed.connectionId) : undefined;
      out.set(
        key,
        feed?.connectionId
          ? answer
            ? {
                at: lock.at,
                verdict: `${answer.health} · ${Math.round(answer.latencyMs)} ms`,
                tone: healthColor(answer.health),
              }
            : { at: lock.at, verdict: `${name} · awaiting answer`, tone: "#ffffff" }
          : { at: lock.at, verdict: VERDICTS[hash(key) % VERDICTS.length], tone: "rgba(255,255,255,0.85)" },
      );
    }
    return out;
  }, [locks, byKey, answers]);

  const onLock = useCallback((key: string, point: Point) => {
    setLocks((previous) =>
      new Map(previous).set(key, { at: performance.now() - born.current, point }),
    );
  }, []);

  // Where everything runs to: the root itself — the dot the mark grows from —
  // not the middle of the box the mark sits in, which is beside the trunk.
  //
  // Worked out from the drawing's own coordinates rather than by measuring the
  // dot, which is scaled to nothing until the mark draws — and measures as the
  // corner of the drawing until then.
  const centre = useCallback((): Point => {
    const svg = markBox.current?.querySelector("svg")?.getBoundingClientRect();
    if (svg && svg.width > 0) {
      const unit = svg.width / 24;
      return { x: svg.left + ROOT_DOT.x * unit, y: svg.top + ROOT_DOT.y * unit };
    }
    const box = markBox.current?.getBoundingClientRect();
    return box && box.width > 0
      ? { x: box.left + box.width / 2, y: box.top + box.height / 2 }
      : { x: window.innerWidth / 2, y: window.innerHeight * 0.42 };
  }, []);

  const lockPoints = useRef<Point[]>([]);
  lockPoints.current = [...locks.values()].map((l) => l.point);

  const leave = (duration: number, delay = 0): TargetAndTransition => ({
    opacity: 0,
    transition: { duration, delay, ease: LEAVE },
  });

  return (
    <div
      className={present ? "fixed inset-0 z-[300]" : "pointer-events-none fixed inset-0 z-[300]"}
      aria-busy={!allIn}
      aria-label="root is starting"
    >
      {/* The dark, and everything that happens in it. Dissolves last, while
          the mark is in flight, so what shows through is the app. */}
      <motion.div
        className="absolute inset-0 overflow-hidden bg-black"
        exit={reduce ? leave(0.2) : leave(0.5, 0.18)}
      >
        {!reduce && (
          <>
            <div className="a-term absolute inset-0">
              <Terminal lines={left} side="left" seconds={6.2} />
              <Terminal lines={right} side="right" seconds={4.8} />
            </div>
            <div className="a-footage absolute inset-0" style={{ animationDelay: `${ACT.sweep}ms` }}>
              <Feeds
                plan={plan}
                answers={answers}
                locks={verdicts}
                camera={f.camera}
                imagery={f.imagery}
                search={f.search}
              />
            </div>
            <div className="splash-sweep" style={{ animationDelay: `${ACT.sweep}ms` }} />
          </>
        )}
        <div className="splash-flare" style={{ animationDelay: `${ACT.mark - 60}ms` }} />
        {!reduce && grain && (
          <div className="splash-grain" style={{ backgroundImage: `url(${grain})` }} aria-hidden />
        )}
        <div className="splash-scanlines" aria-hidden />
        <div className="splash-vignette" aria-hidden />
      </motion.div>

      {!reduce && f.search && (
        <motion.div className="absolute inset-0" exit={leave(0.15)}>
          {/* The hunt is part of the footage, and leaves with it. */}
          <div className="a-footage absolute inset-0" style={{ animationDelay: `${ACT.sweep}ms` }}>
            <Search plan={plan} onLock={onLock} centre={centre} />
          </div>
        </motion.div>
      )}
      {!reduce && f.finale && <Converge from={() => lockPoints.current} centre={centre} />}
      {!reduce && <Command />}

      <div className="relative z-10 flex h-full flex-col">
        <div className="flex min-h-0 flex-1 flex-col items-center justify-center px-6">
          <motion.div
            ref={markBox}
            className="grid h-[150px] w-[150px] place-items-center rounded-md text-white"
            style={{ backgroundColor: "rgba(0, 0, 0, 0)" }}
            exit="leave"
            variants={{ leave: () => flightTo(markBox.current, reduce) }}
          >
            <Mark />
          </motion.div>

          <motion.div exit={leave(0.22)} className="flex flex-col items-center">
            {f.finale && !reduce ? (
              <h1 className="mt-4 text-[104px] leading-[0.84] font-bold tracking-[-.05em] text-white">
                <Decode text="root" start={ACT.title} hold={260} each={110} />
              </h1>
            ) : (
              <h1
                aria-label="root"
                data-text="root"
                className="splash-glitch a-flicker mt-4 text-[104px] leading-[0.84] font-bold tracking-[-.05em] text-white"
                style={
                  { animationDelay: `${ACT.title}ms`, "--d": `${ACT.title}ms` } as React.CSSProperties
                }
              >
                root
              </h1>
            )}

            {f.finale && !reduce ? (
              <p className="mt-6 font-mono text-xs tracking-[.32em] text-white/70 uppercase">
                <Decode text={TAGLINE} start={ACT.tagline} hold={120} each={15} />
              </p>
            ) : (
              <>
                <p className="mt-6 font-mono text-xs tracking-[.32em] text-white/70 uppercase" aria-hidden>
                  {TAGLINE.split("").map((ch, i) => (
                    <span key={i} className="a-char" style={{ animationDelay: `${ACT.tagline + i * 16}ms` }}>
                      {ch}
                    </span>
                  ))}
                  <span
                    className="a-char splash-cursor"
                    style={{ animationDelay: `${ACT.tagline + TAGLINE.length * 16}ms` }}
                  />
                </p>
                <span className="sr-only">Infrastructure intelligence</span>
              </>
            )}

            <div
              className="a-wash mt-10 flex flex-col items-center gap-3"
              style={{ animationDelay: `${ACT.status}ms` }}
            >
              <Status
                total={total}
                answered={answered}
                allIn={allIn}
                arrivals={arrivals}
                loading={!workspaceLoaded || !catalogLoaded}
              />
              {total > 0 && <Squares connections={connections} answers={answers} />}
            </div>
          </motion.div>
        </div>

        <motion.div className="flex shrink-0 items-center px-6 pb-5" exit={leave(0.2)}>
          <span className="num text-2xs tracking-[.14em] text-white/30 uppercase">
            root {__APP_VERSION__}
          </span>
          {workspace && (
            <span className="num ml-auto text-2xs tracking-[.14em] text-white/30 uppercase">
              {workspace}
            </span>
          )}
        </motion.div>
      </div>
    </div>
  );
}

/**
 * Where the mark goes when the splash leaves: the sidebar's own mark, if the
 * screen underneath has one, becoming it on the way — the white line drawing
 * takes the accent tile and the tile's ink as it shrinks into place.
 *
 * Resolved when the exit starts, because the sidebar mounts in the same commit
 * that removes the splash and only has a position from then on.
 */
function flightTo(from: HTMLElement | null, reduce: boolean): TargetAndTransition {
  const target = document.querySelector<HTMLElement>("[data-splash-target]");
  const a = from?.getBoundingClientRect();
  const b = target?.getBoundingClientRect();
  if (reduce || !a || !b || b.width === 0) {
    return { opacity: 0, transition: { duration: 0.2, ease: LEAVE } };
  }
  const root = getComputedStyle(document.documentElement);
  const accent = root.getPropertyValue("--color-accent").trim() || "#4cc2ff";
  const ink = root.getPropertyValue("--color-on-accent").trim() || "#0a0a0c";
  return {
    x: b.left + b.width / 2 - (a.left + a.width / 2),
    y: b.top + b.height / 2 - (a.top + a.height / 2),
    scale: b.width / a.width,
    backgroundColor: accent,
    color: ink,
    transition: {
      type: "spring",
      duration: 0.72,
      bounce: 0.06,
      backgroundColor: { duration: 0.32, delay: 0.18 },
      color: { duration: 0.32, delay: 0.18 },
    },
  };
}

/** A string to a stable 31-bit number. */
function hash(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 16777619);
  return (h >>> 1) || 0;
}

// --------------------------------------------------------------------- pieces

/** The first thing in the dark: a command, typed. */
function Command() {
  return (
    <div className="pointer-events-none absolute inset-x-0 top-[44%] z-20 flex justify-center" aria-hidden>
      <p className="a-command font-mono text-[13px] tracking-[.04em] text-white/90">
        <span className="text-white/35">root ~ </span>
        {COMMAND.split("").map((ch, i) => (
          <span key={i} className="a-char" style={{ animationDelay: `${ACT.command + 120 + i * 24}ms` }}>
            {ch}
          </span>
        ))}
        <span className="splash-cursor" />
      </p>
    </div>
  );
}

/**
 * The mark, drawn in white at the point everything converged: the root, then
 * each branch reaching out, then what it found at the end of it.
 */
function Mark() {
  const t = ACT.mark;
  const stroke = (len: number, delay: number, duration: number) =>
    ({ "--len": len, animationDelay: `${t + delay}ms`, animationDuration: `${duration}ms` }) as React.CSSProperties;
  const pop = (delay: number) => ({ animationDelay: `${t + delay}ms` });

  return (
    <svg
      viewBox="0 0 24 24"
      width={92}
      height={92}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <circle className="a-pop" style={pop(0)} cx="5.6" cy="3.7" r="2" fill="currentColor" stroke="none" />
      <path className="a-draw" style={stroke(14, 60, 420)} d="M5.6 5.7V19.2" />
      <path className="a-draw" style={stroke(10, 260, 360)} d="M5.6 9.7h9.3" />
      <path className="a-draw" style={stroke(10, 320, 360)} d="M5.6 14.45h9.3" />
      <path className="a-draw" style={stroke(10, 380, 360)} d="M5.6 19.2h9.3" />
      <circle className="a-pop" style={pop(520)} cx="17.4" cy="9.7" r="1.7" strokeWidth={1.3} />
      <circle className="a-pop" style={pop(570)} cx="17.4" cy="14.45" r="1.7" strokeWidth={1.3} />
      <circle className="a-pop" style={pop(620)} cx="17.4" cy="19.2" r="1.7" strokeWidth={1.3} />
    </svg>
  );
}

const ORDER: Health[] = ["healthy", "degraded", "down", "unknown"];

/** What root found, in one line — and the line a screen reader hears. */
function Status({
  total,
  answered,
  allIn,
  arrivals,
  loading,
}: {
  total: number;
  answered: number;
  allIn: boolean;
  arrivals: Arrival[];
  loading: boolean;
}) {
  const counts: Record<Health, number> = { healthy: 0, degraded: 0, down: 0, unknown: 0 };
  for (const a of arrivals) counts[a.health] += 1;
  const systems = `${total} system${total === 1 ? "" : "s"}`;

  return (
    <p role="status" aria-live="polite" className="num text-2xs tracking-[.18em] text-white/55 uppercase">
      {loading ? (
        "loading workspace"
      ) : total === 0 ? (
        "no systems configured yet"
      ) : !allIn ? (
        <>
          observing {systems} <span className="text-white/30">·</span> {answered} answered
        </>
      ) : (
        <>
          observing {systems}
          {ORDER.filter((h) => counts[h] > 0).map((h) => (
            <span key={h}>
              <span className="text-white/30"> · </span>
              <span style={{ color: healthColor(h) }}>
                {counts[h]} {h}
              </span>
            </span>
          ))}
        </>
      )}
    </p>
  );
}

const SQUARES = 48;

/** The estate, one square each — hollow until it answers. */
function Squares({
  connections,
  answers,
}: {
  connections: SplashConnection[];
  answers: Map<string, Answer>;
}) {
  const shown = connections.slice(0, SQUARES);
  return (
    <div className="flex max-w-[420px] flex-wrap justify-center gap-[3px]" aria-hidden>
      {shown.map((c) => {
        const answer = answers.get(c.id);
        const color = answer ? healthColor(answer.health) : undefined;
        return (
          <span
            key={c.id}
            className="relative h-[7px] w-[7px] rounded-[1px]"
            style={{ boxShadow: "inset 0 0 0 1px rgba(255,255,255,0.22)" }}
          >
            {color && (
              <>
                <span
                  className="a-pop absolute inset-0 rounded-[1px]"
                  style={{ background: color, boxShadow: `inset 0 0 0 1px ${color}` }}
                />
                <span
                  className="a-ripple absolute inset-0 rounded-[1px]"
                  style={{ "--ripple": color } as React.CSSProperties}
                />
              </>
            )}
          </span>
        );
      })}
      {connections.length > SQUARES && (
        <span className="num ml-1 text-2xs leading-[7px] text-white/40">
          +{connections.length - SQUARES}
        </span>
      )}
    </div>
  );
}
