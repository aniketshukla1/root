// The title sequence's script: when each beat lands, and the generated
// material the beats are made of.
//
// Everything here is seeded, so the same estate opens the same way on every
// launch — a sequence that reshuffles itself reads as noise rather than as a
// title. Where the estate has something real to say, it says it: feeds are
// named after configured connections, the terminal mentions their kinds, and
// the machine's verdict on a connection is that connection's actual health.

/** Each upgrade can be switched off, so they can be judged one at a time. */
export interface SplashFeatures {
  /** A camera flying forward through planes of footage, rather than a fixed view. */
  camera: boolean;
  /** Footage that looks like footage: racks, aisles, a globe, CCTV framing. */
  imagery: boolean;
  /** The machine's search: a reticle hunting feed to feed, typing verdicts. */
  search: boolean;
  /** Every feed's line converging to draw the mark; the name decoding. */
  finale: boolean;
  /** A low swell, data chatter, a hit on the name. */
  sound: boolean;
}

export const ALL_FEATURES: SplashFeatures = {
  camera: true,
  imagery: true,
  search: true,
  finale: true,
  sound: true,
};

/** Beats, in milliseconds from the splash mounting. */
export const ACT = {
  /** A command line types itself in the dark. */
  command: 80,
  /** Footage starts arriving out of the dark, and the terminals wake. */
  feeds: 520,
  /** The machine starts hunting. */
  search: 1350,
  /** The hunt's last lock. */
  searchEnd: 3250,
  /** A scan crosses the screen; the footage falls back. */
  sweep: 3480,
  /** Every line runs to the centre. */
  converge: 3560,
  /** The mark draws itself from the root, as the first pulses reach it. */
  mark: 3960,
  /** The name. */
  title: 4460,
  /** The tagline. */
  tagline: 4920,
  /** What root found. */
  status: 5380,
} as const;

/** The whole sequence has landed by here; App's floor is set from it. */
export const SEQUENCE_MS = 6000;

export function rng(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => ((s = (Math.imul(s, 1664525) + 1013904223) >>> 0) / 2 ** 32);
}

const HEX = "0123456789abcdef";
export function hex(r: () => number, n: number): string {
  let out = "";
  for (let i = 0; i < n; i++) out += HEX[Math.floor(r() * 16)];
  return out;
}

// ------------------------------------------------------------------- terminal

export type Tone = "dim" | "bright" | "ok" | "warn" | "crit";

export interface TermLine {
  text: string;
  tone: Tone;
}

const FALLBACK_NAMES = ["edge-lb", "orders-api", "orders-db", "kafka-main", "auth", "ledger"];
const FALLBACK_KINDS = ["docker", "postgres", "kubernetes", "redis", "kafka", "http_endpoint"];
const METRICS = ["cpu", "mem_used", "p99_ms", "rx_bytes", "lag", "conns", "gc_pause", "iops"];

/**
 * A terminal's worth of root talking to an estate: attaching collectors,
 * probing ports, matching revisions, counting lines. Named after the real
 * connections when there are any.
 */
export function terminalLines(seed: number, names: string[], kinds: string[], count: number): TermLine[] {
  const r = rng(seed);
  const pick = <T,>(xs: T[]) => xs[Math.floor(r() * xs.length)];
  const n = names.length ? names : FALLBACK_NAMES;
  const k = kinds.length ? kinds : FALLBACK_KINDS;
  let clock = r() * 0.2;
  const ts = () => {
    clock += 0.004 + r() * 0.019;
    return `[${clock.toFixed(3).padStart(7, " ")}]`;
  };

  const lines: TermLine[] = [];
  for (let i = 0; i < count; i++) {
    const name = pick(n);
    const ms = (1 + r() * 380).toFixed(1);
    const roll = r();
    let text: string;
    let tone: Tone = "dim";
    switch (Math.floor(r() * 10)) {
      case 0:
        text = `${ts()} attach   ${pick(k)}://${name}`;
        break;
      case 1:
        text = `${ts()} probe    ${name}  tcp/${pick([5432, 6379, 9092, 443, 8080, 2375])}  syn→ack ${ms}ms`;
        break;
      case 2:
        text = `${ts()} GET /healthz 200 ${ms}ms  ${name}`;
        break;
      case 3:
        text = `${ts()} scan     10.${Math.floor(r() * 255)}.${Math.floor(r() * 255)}.0/24  ${Math.floor(r() * 250)} hosts`;
        break;
      case 4:
        text = `${ts()} match    ${name}  sha256:${hex(r, 8)}…${hex(r, 4)}`;
        break;
      case 5:
        text = `${ts()} metric   ${name}.${pick(METRICS)}  ${(r() * 100).toFixed(2)}`;
        break;
      case 6:
        text = `${ts()} stream   ${name}  +${Math.floor(r() * 900)} lines  ${Math.floor(r() * 4000)}/s`;
        break;
      case 7:
        text = `0x${hex(r, 8)}  ${Array.from({ length: 8 }, () => hex(r, 2)).join(" ")}`;
        break;
      case 8:
        text = `${ts()} trace    ${hex(r, 16)}  ${Math.floor(2 + r() * 40)} spans  ${ms}ms`;
        break;
      default:
        text = `${ts()} index    ${pick(k)}  ${Math.floor(r() * 64)} fields  ok`;
    }
    if (roll < 0.018) tone = "warn";
    else if (roll < 0.026) tone = "crit";
    else if (roll < 0.045) tone = "ok";
    else if (roll < 0.14) tone = "bright";
    lines.push({ text, tone });
  }
  return lines;
}

// ---------------------------------------------------------------------- feeds

/** Instruments: what a monitoring console draws. */
export type ChartArt = "line" | "bars" | "graph" | "log" | "heat" | "pips";
/** Footage: what a camera pointed at the estate would see. */
export type SceneArt = "rack" | "aisle" | "globe";
export type FeedArt = ChartArt | SceneArt;

const CHARTS: FeedArt[] = ["line", "bars", "graph", "log", "heat", "pips"];
/** Mostly scenes, with enough instruments left in that it reads as monitoring. */
const FOOTAGE: FeedArt[] = ["rack", "aisle", "line", "globe", "rack", "pips", "aisle", "log", "rack", "graph"];

/**
 * Depths of the planes the camera flies through, farthest first, in px from
 * the screen. The camera starts behind all of them and ends past the nearest
 * two, so those fly by on either side.
 */
export const PLANES = [-2700, -2000, -1400, -880, -380] as const;

export interface FeedPlan {
  key: string;
  art: FeedArt;
  seed: number;
  /** The connection this feed watches, when it watches a real one. */
  connectionId?: string;
  title: string;
  stamp: string;
  camera: string;

  // The camera's version: a fixed place on a plane, revealed as it passes.
  plane: number;
  /** Position on the plane as a fraction of the visible half-width, -1…1. */
  sx: number;
  sy: number;
  appear: number;

  // The fixed view's version: each feed carried by its own keyframes.
  x: number;
  y: number;
  rotateX: number;
  rotateY: number;
  delay: number;
  duration: number;
  approach: boolean;
  /** Brackets of its own, for when there is no search to lock it. */
  lock: boolean;
}

const FEEDS = 25;

/**
 * Twenty-five feeds. With the camera they sit on five planes of five, spread
 * by the golden angle so no two crowd one corner, the near planes keeping the
 * middle clear for the title; real connections go on the middle planes, where
 * they stay in view longest and the search can find them.
 */
export function feedPlan(
  connections: { id: string; name: string; kind?: string }[],
  seed: number,
  imagery: boolean,
): FeedPlan[] {
  const r = rng(seed);
  const real = connections.slice(0, 8);
  const arts = imagery ? FOOTAGE : CHARTS;
  let nextReal = 0;

  return Array.from({ length: FEEDS }, (_, i) => {
    const plane = i % PLANES.length;
    const slot = Math.floor(i / PLANES.length);
    const watches = (plane === 2 || plane === 3) && nextReal < real.length;
    const watched = watches ? real[nextReal++] : undefined;

    const angle = i * 2.39996 + r() * 0.4;
    // Near planes keep a clear middle; far ones may sit anywhere, small.
    const inner = plane >= 3 ? 0.42 : plane === 2 ? 0.3 : 0.12;
    const reach = inner + (1 - inner) * (0.25 + r() * 0.75);
    const minutes = 10 + Math.floor(r() * 49);
    const seconds = Math.floor(r() * 60);
    const fixedAngle = i * 2.39996 + r() * 0.3;
    const fixedRadius = 17 + (i % 4) * 7 + r() * 5;

    return {
      key: `feed-${i}`,
      art: watched ? arts[(i + 3) % arts.length] : arts[(i + slot) % arts.length],
      seed: seed + i * 977,
      connectionId: watched?.id,
      title: watched
        ? `${watched.kind ?? "node"} · ${watched.name}`
        : `feed ${String(i + 1).padStart(2, "0")} · ${hex(r, 4)}`,
      stamp: `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}.${hex(r, 3)}`,
      camera: `cam ${String(1 + Math.floor(r() * 48)).padStart(2, "0")}`,

      plane,
      sx: Math.cos(angle) * reach,
      sy: Math.sin(angle) * reach * 0.82,
      appear: ACT.feeds + slot * 210 + (PLANES.length - 1 - plane) * 36 + r() * 120,

      x: Math.cos(fixedAngle) * fixedRadius * 1.25,
      y: Math.sin(fixedAngle) * fixedRadius * 0.95,
      rotateX: (r() - 0.5) * 16,
      rotateY: (r() - 0.5) * 26,
      delay: ACT.feeds + i * 50 + r() * 36,
      duration: 1350 + r() * 700,
      approach: r() < 0.22,
      lock: Boolean(watched) || r() < 0.35,
    };
  });
}

// --------------------------------------------------------------------- search

/** When the machine locks, in order. */
export function lockTimes(): number[] {
  const count = 9;
  const span = ACT.searchEnd - ACT.search;
  return Array.from({ length: count }, (_, i) => ACT.search + (span * i) / (count - 1));
}

/** What the machine says about a feed it has no connection behind. */
export const VERDICTS = ["nominal", "indexed", "no anomaly", "watching", "baseline ok", "tracked"];

/** Characters a scrambled letter cycles through before it settles. */
// No shade blocks: at title size they read as solid tiles, not as a letter
// being worked out.
export const GLYPHS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789#$%&*+<>/=?";
