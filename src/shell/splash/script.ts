// The title sequence's script: when each beat lands, and the generated
// material the beats are made of.
//
// Everything here is seeded, so the same estate opens the same way on every
// launch — a sequence that reshuffles itself reads as noise rather than as a
// title. Where the estate has something real to say, it says it: feeds are
// named after configured connections, the terminal mentions their kinds, and
// a connection's lock-on takes its health the moment it answers.

/** Beats, in milliseconds from the splash mounting. */
export const ACT = {
  /** A command line types itself in the dark. */
  command: 80,
  /** Feeds start arriving out of the dark, and the terminals wake. */
  feeds: 420,
  /** A scan crosses the screen; feeds and terminals fall back. */
  sweep: 1640,
  /** The mark draws itself where everything converged. */
  mark: 1760,
  /** The wordmark. */
  title: 2160,
  /** The tagline types. */
  tagline: 2480,
  /** What root found. */
  status: 2920,
} as const;

/** The whole sequence has landed by here; App's floor is set from it. */
export const SEQUENCE_MS = 3300;

export function rng(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => ((s = (Math.imul(s, 1664525) + 1013904223) >>> 0) / 2 ** 32);
}

const HEX = "0123456789abcdef";
function hex(r: () => number, n: number): string {
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
    // A few lines stand out, the way a real stream has the odd line worth
    // reading in it.
    if (roll < 0.018) tone = "warn";
    else if (roll < 0.026) tone = "crit";
    else if (roll < 0.045) tone = "ok";
    else if (roll < 0.14) tone = "bright";
    lines.push({ text, tone });
  }
  return lines;
}

// ---------------------------------------------------------------------- feeds

export type FeedArt = "line" | "bars" | "graph" | "log" | "heat" | "pips";
const ARTS: FeedArt[] = ["line", "bars", "graph", "log", "heat", "pips"];

export interface FeedPlan {
  key: string;
  /** Offset from centre, in vw and vh. */
  x: number;
  y: number;
  rotateX: number;
  rotateY: number;
  delay: number;
  duration: number;
  /** Most feeds fall away; a few rush past instead. */
  approach: boolean;
  art: FeedArt;
  seed: number;
  /** The connection this feed watches, when it watches a real one. */
  connectionId?: string;
  title: string;
  stamp: string;
  /** Whether the machine's brackets lock onto it. */
  lock: boolean;
}

const FEEDS = 24;

/**
 * Twenty feeds around the centre, spread by the golden angle so no two crowd
 * the same corner, with the middle left clear for the title. The first few
 * real connections each get one of their own.
 */
export function feedPlan(
  connections: { id: string; name: string; kind?: string }[],
  seed: number,
): FeedPlan[] {
  const r = rng(seed);
  const real = connections.slice(0, 7);
  return Array.from({ length: FEEDS }, (_, i) => {
    const angle = i * 2.39996 + r() * 0.3;
    const radius = 17 + (i % 4) * 7 + r() * 5;
    const watched = i % 3 === 1 ? real[Math.floor(i / 3)] : undefined;
    const minutes = 10 + Math.floor(r() * 49);
    const seconds = Math.floor(r() * 60);
    return {
      key: `feed-${i}`,
      x: Math.cos(angle) * radius * 1.25,
      y: Math.sin(angle) * radius * 0.95,
      rotateX: (r() - 0.5) * 16,
      rotateY: (r() - 0.5) * 26,
      delay: ACT.feeds + i * 50 + r() * 36,
      duration: 1350 + r() * 700,
      approach: r() < 0.22,
      art: watched ? ARTS[(i + 2) % ARTS.length] : ARTS[i % ARTS.length],
      seed: seed + i * 977,
      connectionId: watched?.id,
      title: watched
        ? `${watched.kind ?? "node"} · ${watched.name}`
        : `feed ${String(i + 1).padStart(2, "0")} · ${hex(r, 4)}`,
      stamp: `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}.${hex(r, 3)}`,
      lock: Boolean(watched) || r() < 0.35,
    };
  });
}
