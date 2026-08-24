// Assembles design/<Name>.dc.html from parts/tokens.css + parts/<name>.body.html.
// Bodies use marker comments (<!--ICON …-->, <!--RAIL …-->, …) which expand()
// replaces; anything left unexpanded fails the build rather than shipping.
import { readFileSync, writeFileSync, readdirSync } from "node:fs";

const tokens = readFileSync(new URL("./parts/tokens.css", import.meta.url), "utf8");

// ------------------------------------------------------------------- icons
const ICONS = {
  overview: '<rect x="3" y="3" width="7.5" height="7.5" rx="1"/><rect x="13.5" y="3" width="7.5" height="7.5" rx="1"/><rect x="3" y="13.5" width="7.5" height="7.5" rx="1"/><rect x="13.5" y="13.5" width="7.5" height="7.5" rx="1"/>',
  services: '<path d="M12 2.8 21 7.4 12 12 3 7.4z"/><path d="M3 12.2 12 16.8l9-4.6"/><path d="M3 16.6 12 21.2l9-4.6"/>',
  alerts: '<path d="M12 3.4a5.4 5.4 0 0 0-5.4 5.4v3.4L5 15.8h14l-1.6-3.6V8.8A5.4 5.4 0 0 0 12 3.4z"/><path d="M9.9 18.8a2.2 2.2 0 0 0 4.2 0"/>',
  metrics: '<path d="M3.5 20.2h17"/><path d="M6 16.2l3.9-5 3 3 4.8-7.2"/>',
  logs: '<path d="M4 6.2h16M4 10.4h11M4 14.6h16M4 18.8h8"/>',
  assistant: '<path d="M11.8 3.2l1.75 4.9 4.9 1.75-4.9 1.75-1.75 4.9-1.75-4.9-4.9-1.75 4.9-1.75z"/><path d="M18.4 15.4l.75 2.05 2.05.75-2.05.75-.75 2.05-.75-2.05-2.05-.75 2.05-.75z"/>',
  settings: '<path d="M3.6 7.6h9.2M17.4 7.6h3M3.6 16.4h3.4M11.6 16.4h8.8"/><circle cx="15.1" cy="7.6" r="2.3"/><circle cx="9.3" cy="16.4" r="2.3"/>',
  search: '<circle cx="10.8" cy="10.8" r="6.4"/><path d="M15.6 15.6l4.6 4.6"/>',
  refresh: '<path d="M20.2 12a8.2 8.2 0 1 1-2.7-6.1"/><path d="M20.4 4.2v5.1h-5.1"/>',
  right: '<path d="M9.5 5.2 16.3 12l-6.8 6.8"/>',
  left: '<path d="M14.5 5.2 7.7 12l6.8 6.8"/>',
  down: '<path d="M5.2 9.2 12 16l6.8-6.8"/>',
  up: '<path d="M5.2 14.8 12 8l6.8 6.8"/>',
  open: '<path d="M14.2 3.8h6v6"/><path d="M20.2 3.8 11 13"/><path d="M18 13.8v5.4a1.6 1.6 0 0 1-1.6 1.6H5.4a1.6 1.6 0 0 1-1.6-1.6V8.2a1.6 1.6 0 0 1 1.6-1.6h5.4"/>',
  clock: '<circle cx="12" cy="12" r="8.4"/><path d="M12 7.2V12l3.2 2"/>',
  check: '<path d="M4.6 12.6 9.4 17.4 19.4 6.8"/>',
  close: '<path d="M6 6l12 12M18 6 6 18"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  filter: '<path d="M3.6 5.6h16.8l-6.5 7.6v6l-3.8 2v-8z"/>',
  bolt: '<path d="M13.4 2.8 5.6 13.4h5.4l-.8 7.8 7.8-10.6h-5.4z"/>',
  copy: '<rect x="8.6" y="8.6" width="11.6" height="11.6" rx="1"/><path d="M15.4 5.4a2 2 0 0 0-2-2H5.8a2 2 0 0 0-2 2V13a2 2 0 0 0 2 2"/>',
  // The mark: a filesystem tree. One solid root, three branches, three open
  // leaves — root node, root cause, tree root.
  mark: '<circle cx="5.6" cy="3.7" r="2" fill="currentColor" stroke="none"/><path d="M5.6 5.7V19.2"/><path d="M5.6 9.7h9.3"/><path d="M5.6 14.45h9.3"/><path d="M5.6 19.2h9.3"/><circle cx="17.4" cy="9.7" r="1.7" stroke-width="1.35"/><circle cx="17.4" cy="14.45" r="1.7" stroke-width="1.35"/><circle cx="17.4" cy="19.2" r="1.7" stroke-width="1.35"/>',
};

function icon(name, size = 15, color = "currentColor", width = 1.6) {
  const body = ICONS[name];
  if (!body) throw new Error("unknown icon: " + name);
  return `<svg viewBox="0 0 24 24" width="${size}" height="${size}" fill="none" stroke="${color}" stroke-width="${width}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style="flex-shrink:0">${body}</svg>`;
}

// ----------------------------------------------------------- status atoms
// Colour AND silhouette both carry the state, so the signal survives
// colour-vision deficiency and a monochrome screenshot.
const HEALTH = {
  healthy: ["var(--ok)", "Healthy"],
  degraded: ["var(--warn)", "Degraded"],
  down: ["var(--crit)", "Down"],
  unknown: ["var(--faint)", "Unknown"],
};

function dot(state, size = 9) {
  const [c] = HEALTH[state];
  const open = `<svg width="${size}" height="${size}" viewBox="0 0 10 10" aria-hidden="true" style="flex-shrink:0;display:block">`;
  if (state === "degraded") return `${open}<path d="M5 .6 9.5 9.2H.5Z" fill="${c}"/></svg>`;
  if (state === "down") return `${open}<rect x=".6" y=".6" width="8.8" height="8.8" fill="${c}"/></svg>`;
  if (state === "unknown") return `${open}<circle cx="5" cy="5" r="3.4" fill="none" stroke="${c}" stroke-width="1.9"/></svg>`;
  return `${open}<circle cx="5" cy="5" r="4" fill="${c}"/></svg>`;
}

/** Status is never colour alone: the dot travels with the word where there is room. */
function badge(state, size = 9) {
  const [c, word] = HEALTH[state];
  return `<span style="display:inline-flex;align-items:center;gap:5px;white-space:nowrap">${dot(state, size)}<span class="mono" style="font-size:10px;letter-spacing:.05em;color:${c}">${word}</span></span>`;
}

/** Identity badge. The monogram is a supported outcome, not a missing asset —
 *  a collector this build has never heard of still gets a stable mark. */
function mark(text, color, size = 20) {
  return `<span style="display:inline-grid;place-items:center;flex-shrink:0;width:${size}px;height:${size}px;border-radius:2px;color:${color};box-shadow:inset 0 0 0 1px color-mix(in srgb, ${color} 45%, transparent);background:color-mix(in srgb, ${color} 15%, transparent)"><span class="mono" style="font-size:${(size * 0.4).toFixed(1)}px;font-weight:600;line-height:1">${text}</span></span>`;
}

/** The component matrix. Sharp pips, one per component, filled by health —
 *  at this size colour alone is not readable, so each carries its state in
 *  the title too. */
function cells(count, seed, bad) {
  const r = rng(seed);
  const states = Array.from({ length: count }, () => "healthy");
  const pool = ["down", "degraded", "degraded", "unknown"];
  for (let i = 0; i < bad; i += 1) states[Math.floor(r() * count)] = pool[Math.floor(r() * pool.length)];
  return states
    .map((st) => {
      const [c, word] = HEALTH[st];
      const fill = st === "healthy"
        ? `background:color-mix(in srgb, ${c} 26%, var(--panel));box-shadow:inset 0 0 0 1px color-mix(in srgb, ${c} 55%, transparent)`
        : `background:${c};box-shadow:inset 0 0 0 1px ${c}`;
      return `<span title="${word}" style="width:14px;height:14px;flex-shrink:0;${fill}"></span>`;
    })
    .join("");
}

// -------------------------------------------------------------- generators
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A series that wanders, with an optional late excursion. */
function series(n, seed, shape) {
  const r = rng(seed);
  const out = [];
  let v = 0.4 + r() * 0.25;
  for (let i = 0; i < n; i += 1) {
    v += (r() - 0.5) * 0.13;
    const t = i / (n - 1);
    if (shape === "rise") v += 0.011;
    if (shape === "fall") v -= 0.01;
    if (shape === "spike" && t > 0.74) v += 0.075;
    if (shape === "step" && t > 0.52) v += 0.03;
    v = Math.max(0.06, Math.min(0.95, v));
    out.push(v);
  }
  return out;
}

function spark(w, h, color, seed, shape, warn) {
  const n = 26;
  const pts = series(n, seed, shape);
  const x = (i) => (i / (n - 1)) * w;
  const y = (v) => h - 2 - v * (h - 4);
  const line = pts.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const id = `sp${seed}`;
  const threshold = warn
    ? `<line x1="0" y1="${(h - 2 - warn * (h - 4)).toFixed(1)}" x2="${w}" y2="${(h - 2 - warn * (h - 4)).toFixed(1)}" stroke="var(--warn)" stroke-width="1" stroke-dasharray="2 3" opacity=".55"/>`
    : "";
  return `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" aria-hidden="true" style="display:block;overflow:visible">
<defs><linearGradient id="${id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${color}" stop-opacity=".24"/><stop offset="1" stop-color="${color}" stop-opacity="0"/></linearGradient></defs>
${threshold}<polygon points="0,${h} ${line} ${w},${h}" fill="url(#${id})"/>
<polyline points="${line}" fill="none" stroke="${color}" stroke-width="1.4" stroke-linejoin="round" stroke-linecap="round"/>
<circle cx="${w}" cy="${y(pts[n - 1]).toFixed(1)}" r="1.8" fill="${color}"/></svg>`;
}

function area(w, h, color, seed, shape, deploy) {
  const n = 60;
  const pts = series(n, seed, shape);
  const x = (i) => (i / (n - 1)) * w;
  const y = (v) => h - v * (h - 6) - 3;
  const line = pts.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const id = `ar${seed}`;
  const dx = deploy ? (w * deploy).toFixed(1) : null;
  const marker = dx
    ? `<line x1="${dx}" y1="0" x2="${dx}" y2="${h}" stroke="var(--accent)" stroke-width="1" stroke-dasharray="3 3" opacity=".7"/><path d="M${Number(dx) - 3.5} 0 L${Number(dx) + 3.5} 0 L${dx} 5.5 Z" fill="var(--accent)"/>`
    : "";
  return `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" aria-hidden="true" style="display:block;width:100%">
<defs><linearGradient id="${id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${color}" stop-opacity=".28"/><stop offset="1" stop-color="${color}" stop-opacity=".02"/></linearGradient></defs>
<polygon points="0,${h} ${line} ${w},${h}" fill="url(#${id})"/>
<polyline points="${line}" fill="none" stroke="${color}" stroke-width="1.5" stroke-linejoin="round"/>${marker}</svg>`;
}

const LEVELS = [
  ["var(--lvl-debug)", 0.3],
  ["var(--lvl-info)", 0.44],
  ["var(--warn)", 0.16],
  ["var(--crit)", 0.1],
];

function hist(w, h, n, seed) {
  const r = rng(seed);
  const gap = 2;
  const bw = (w - (n - 1) * gap) / n;
  let out = "";
  for (let i = 0; i < n; i += 1) {
    const t = i / (n - 1);
    let total = 0.28 + r() * 0.34;
    const burst = t > 0.62 && t < 0.8;
    if (burst) total = Math.min(1, total + 0.42);
    if (t > 0.9) total = Math.min(1, total + 0.16);
    let acc = 0;
    const x = (i * (bw + gap)).toFixed(2);
    LEVELS.forEach(([color, share], li) => {
      let sh = share;
      if (burst && li >= 2) sh = share * 2.6;
      if (burst && li < 2) sh = share * 0.55;
      const seg = total * sh * (h - 1);
      if (seg < 0.6) return;
      acc += seg;
      out += `<rect x="${x}" y="${(h - acc).toFixed(2)}" width="${bw.toFixed(2)}" height="${seg.toFixed(2)}" fill="${color}"/>`;
    });
  }
  return `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" aria-hidden="true" style="display:block;width:100%">${out}</svg>`;
}

/** A state lane, drawn as continuous RUNS rather than per-sample cells — a
 *  period of state, not sixty samples. Nominal is quiet; absence is hatched,
 *  never merely grey, so a collector that stopped reporting can never be
 *  mistaken for a service that is fine. */
const LANE_FILL = { healthy: "var(--lane-ok)", degraded: "var(--warn)", down: "var(--crit)" };
let hatchSeq = 0;

function lane(w, bars, spec) {
  const states = new Array(bars).fill("healthy");
  for (const seg of spec.split(",")) {
    const [st, range] = seg.split(":");
    const [a, b] = range.split("-").map(Number);
    for (let i = a; i < b && i < bars; i += 1) states[i] = st;
  }
  const unit = w / bars;
  const id = `hatch${(hatchSeq += 1)}`;
  let out = "";
  let i = 0;
  let hatched = false;
  while (i < bars) {
    let j = i;
    while (j < bars && states[j] === states[i]) j += 1;
    const x = (i * unit).toFixed(2);
    const width = Math.max(1.6, (j - i) * unit - 1.2).toFixed(2);
    if (states[i] === "unknown") {
      hatched = true;
      out += `<rect x="${x}" y="0" width="${width}" height="13" fill="var(--cell-todo)"/>`;
      out += `<rect x="${x}" y="0" width="${width}" height="13" fill="url(#${id})"/>`;
    } else {
      out += `<rect x="${x}" y="0" width="${width}" height="13" fill="${LANE_FILL[states[i]]}"/>`;
    }
    i = j;
  }
  const defs = hatched
    ? `<defs><pattern id="${id}" width="5" height="5" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><line x1="0" y1="0" x2="0" y2="5" stroke="var(--faint)" stroke-width="1.6"/></pattern></defs>`
    : "";
  return `<svg width="${w}" height="13" viewBox="0 0 ${w} 13" preserveAspectRatio="none" aria-hidden="true" style="display:block;width:100%;height:13px">${defs}${out}</svg>`;
}

/** Ruler ticks — the motif that makes every plot read as an instrument. */
function ticks(w, count, major) {
  let out = "";
  for (let i = 0; i <= count; i += 1) {
    const x = ((i / count) * (w - 1) + 0.5).toFixed(2);
    const big = i % major === 0;
    out += `<line x1="${x}" y1="0" x2="${x}" y2="${big ? 6 : 3}" stroke="${big ? "var(--line-strong)" : "var(--line)"}" stroke-width="1"/>`;
  }
  return `<svg width="${w}" height="6" viewBox="0 0 ${w} 6" preserveAspectRatio="none" aria-hidden="true" style="display:block;width:100%;height:6px">${out}</svg>`;
}


// ------------------------------------------------------- chart vocabulary
// Everything below draws to the same rules: nominal is quiet, a status hue
// means a state, series hues carry identity only, and motion is intro-only.

/** Percentile fan — p99 band, p90 band, p50 line. What a developer reads when
 *  "the average is fine" and the tail is not. */
function fan(w, h, seed) {
  const n = 44;
  // Spread the three percentiles across the full height: nested bands at this
  // size collapse into one blue mass, so the fan is drawn as three separable
  // lines over a single faint envelope.
  const p50 = series(n, seed, "flat").map((v) => 0.06 + v * 0.1);
  const p90 = p50.map((v, i) => v + 0.22 + Math.abs(Math.sin(i / 5)) * 0.05);
  const p99 = p90.map((v, i) => v + 0.2 + (i > n * 0.68 ? ((i - n * 0.68) / n) * 2.1 : 0));
  const x = (i) => (i / (n - 1)) * w;
  const y = (v) => h - 4 - Math.min(0.97, v) * (h - 11);
  const pts = (a) => a.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const line = (a, sw, op, cls) =>
    `<polyline class="${cls}" style="${cls === "a-draw" ? `--len:${(w * 1.3).toFixed(0)};` : ""}--op:${op}" points="${pts(a)}" fill="none" stroke="var(--s1)" stroke-width="${sw}" stroke-linejoin="round" stroke-linecap="round" opacity="${op}"/>`;
  return `<svg viewBox="0 0 ${w} ${h}" width="${w}" height="${h}" preserveAspectRatio="none" aria-hidden="true" style="display:block;width:100%">
<polygon class="a-wash" style="--op:.07" points="${pts(p50)} ${pts(p99).split(" ").reverse().join(" ")}" fill="var(--s1)" opacity=".07"/>
${line(p99, 1.2, ".45", "a-wash")}${line(p90, 1.4, ".7", "a-wash")}${line(p50, 2, "1", "a-draw")}</svg>`;
}

/** Scatter — one dot per sample, outliers in the alarm hue. Latency against
 *  throughput answers "is it load, or is it us?". */
function scatter(w, h, seed, n) {
  const r = rng(seed);
  let out = "";
  for (let i = 0; i < n; i += 1) {
    const t = r();
    const bad = r() > 0.88;
    const cx = 6 + t * (w - 12);
    const base = 0.22 + t * 0.3 + (r() - 0.5) * 0.18;
    const cy = h - 5 - Math.min(0.95, bad ? base + 0.42 : base) * (h - 12);
        const op = bad ? ".95" : ".55";
    out += `<circle class="a-pop" style="animation-delay:${(i * 9) % 520}ms;--op:${op}" cx="${cx.toFixed(1)}" cy="${cy.toFixed(1)}" r="${bad ? 2.6 : 2}" fill="${bad ? "var(--crit)" : "var(--s1)"}" opacity="${op}"/>`;
  }
  return `<svg viewBox="0 0 ${w} ${h}" width="${w}" height="${h}" aria-hidden="true" style="display:block;width:100%">${out}</svg>`;
}

/** Bullet — a measure against its limit. The only honest way to show a
 *  utilisation number, because a percentage without its ceiling is a rumour. */
function bullet(pct, limit, tone, delay) {
  const c = tone === "warn" ? "var(--warn)" : tone === "crit" ? "var(--crit)" : "var(--s3)";
  return `<span style="position:relative;display:block;height:7px;background:var(--sunk);border-radius:1px;overflow:hidden">
      <span class="a-reach" style="animation-delay:${delay}ms;position:absolute;inset:0 auto 0 0;width:${pct}%;background:${c};border-radius:1px"></span>
      <span style="position:absolute;top:-2px;bottom:-2px;left:${limit}%;width:1px;background:var(--ink-2);opacity:.65"></span>
    </span>`;
}

/** Ranked horizontal bars — top-N by whatever is hurting. */
function hbars(items) {
  const max = Math.max(...items.map((i) => i[1]));
  return items
    .map(([label, val, tone], i) => {
      const c = tone === "crit" ? "var(--crit)" : tone === "warn" ? "var(--warn)" : "var(--s1)";
      return `<div style="display:grid;grid-template-columns:104px 1fr 52px;align-items:center;gap:10px">
        <span class="mono" style="font-size:10.5px;color:var(--ink-2);overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${label}</span>
        <span style="display:block;height:9px;background:var(--sunk);border-radius:1px;overflow:hidden"><span class="a-reach" style="animation-delay:${i * 70}ms;display:block;height:100%;width:${((val / max) * 100).toFixed(1)}%;background:${c};border-radius:1px"></span></span>
        <span class="mono" style="font-size:10.5px;color:var(--muted);text-align:right">${val.toLocaleString()}</span>
      </div>`;
    })
    .join("");
}

/** Error-budget arc. Management asks one question — how much room is left. */
function arc(size, pct, tone) {
  const c = tone === "warn" ? "var(--warn)" : tone === "crit" ? "var(--crit)" : "var(--ok)";
  const r = size / 2 - 7;
  const circ = 2 * Math.PI * r;
  return `<svg width="${size}" height="${size}" viewBox="0 0 ${size} ${size}" aria-hidden="true" style="display:block;transform:rotate(-90deg)">
<circle cx="${size / 2}" cy="${size / 2}" r="${r}" fill="none" stroke="var(--sunk)" stroke-width="9"/>
<circle class="a-sweep" style="--len:${circ.toFixed(1)}" cx="${size / 2}" cy="${size / 2}" r="${r}" fill="none" stroke="${c}" stroke-width="9" stroke-linecap="butt"
 stroke-dasharray="${(circ * pct / 100).toFixed(1)} ${circ.toFixed(1)}"/></svg>`;
}

/** Availability by month — the shape of a year, in one row. */
function stackbars(months) {
  return months
    .map(([label, up, deg, down], i) => {
      const seg = (v, c) => (v > 0 ? `<span style="display:block;height:${v}%;background:${c}"></span>` : "");
      return `<div style="display:flex;flex-direction:column;align-items:center;gap:6px;flex:1;min-width:0">
        <span class="a-rise" style="animation-delay:${i * 55}ms;display:flex;flex-direction:column-reverse;width:100%;height:74px;border-radius:1px;overflow:hidden;background:var(--sunk)">${seg(up, "var(--ok)")}${seg(deg, "var(--warn)")}${seg(down, "var(--crit)")}</span>
        <span class="mono" style="font-size:9px;color:var(--faint)">${label}</span>
      </div>`;
    })
    .join("");
}

/** Incident calendar — twelve weeks of days, darkest where it hurt most. */
function calendar(weeks, seed) {
  const r = rng(seed);
  let out = "";
  for (let w = 0; w < weeks; w += 1) {
    let col = "";
    for (let d = 0; d < 7; d += 1) {
      const v = r();
      const lvl = v > 0.94 ? 3 : v > 0.84 ? 2 : v > 0.66 ? 1 : 0;
      const fill = ["var(--cell-todo)", "color-mix(in srgb, var(--warn) 35%, var(--cell-todo))", "var(--warn)", "var(--crit)"][lvl];
      col += `<span class="a-pop" style="animation-delay:${(w * 7 + d) * 5}ms;width:11px;height:11px;background:${fill};border-radius:1px"></span>`;
    }
    out += `<div style="display:flex;flex-direction:column;gap:3px">${col}</div>`;
  }
  return `<div style="display:flex;gap:3px">${out}</div>`;
}

// -------------------------------------------------------------------- rail
const NAV = [
  ["overview", "Overview", ""],
  ["services", "Services", "12"],
  ["alerts", "Alerts", "4"],
  ["metrics", "Metrics", ""],
  ["logs", "Logs", ""],
  ["assistant", "Assistant", ""],
];

function rail(active) {
  const items = NAV.map(([key, label, count]) => {
    const on = key === active;
    const badgeEl = count
      ? `<span class="mono" style="margin-left:auto;font-size:10px;color:${key === "alerts" ? "var(--warn)" : "var(--faint)"}">${count}</span>`
      : "";
    return `      <div class="nav${on ? " on" : ""}">${icon(key, 15, on ? "var(--accent)" : "var(--faint)")}<span>${label}</span>${badgeEl}</div>`;
  }).join("\n");

  return `<nav style="width:214px;flex-shrink:0;display:flex;flex-direction:column;gap:2px;padding:14px 12px;border-right:1px solid var(--line);background:var(--panel)">
      <div style="display:flex;align-items:center;gap:9px;height:28px;padding:0 5px;margin-bottom:13px">
        <span style="display:grid;place-items:center;width:26px;height:26px;border-radius:2px;background:var(--accent);color:var(--on-accent);flex-shrink:0">${icon("mark", 16, "currentColor", 1.9)}</span>
        <span class="mono" style="font-size:11px;letter-spacing:.18em;text-transform:uppercase;color:var(--ink)">root</span>
        <span style="margin-left:auto;display:flex;align-items:center;gap:7px">
          <span style="width:6px;height:6px;background:var(--warn)"></span>
          <span style="color:var(--faint);display:flex">${icon("left", 13, "currentColor")}</span>
        </span>
      </div>
      <div class="field" style="margin-bottom:10px">
        ${icon("search", 13, "var(--faint)")}<span style="color:var(--faint)">Search</span>
        <span class="kbd" style="margin-left:auto">⌘K</span>
      </div>
${items}
      <div style="margin-top:auto;display:flex;flex-direction:column;gap:2px">
        <div class="nav${active === "settings" ? " on" : ""}">${icon("settings", 15, active === "settings" ? "var(--accent)" : "var(--faint)")}<span>Settings</span></div>
        <div style="display:flex;align-items:center;gap:9px;margin-top:10px;padding:8px 7px;border-top:1px solid var(--line)">
          <span style="display:grid;place-items:center;width:24px;height:24px;border-radius:2px;flex-shrink:0;color:var(--s6);box-shadow:inset 0 0 0 1px color-mix(in srgb, var(--s6) 45%, transparent);background:color-mix(in srgb, var(--s6) 15%, transparent)"><span class="mono" style="font-size:9.5px;font-weight:600;line-height:1">AC</span></span>
          <span style="flex:1;min-width:0">
            <span style="display:block;font-size:11.5px;color:var(--ink);overflow:hidden;text-overflow:ellipsis;white-space:nowrap">Acme Platform</span>
            <span class="mono" style="display:block;font-size:9.5px;color:var(--faint)">aniket · admin</span>
          </span>
          ${icon("down", 12, "var(--faint)")}
        </div>
      </div>
    </nav>`;
}


/** The assistant's handle on every screen. Context flows into the dock from
 *  whatever you were looking at, so an operator never has to leave the screen
 *  where the problem is to ask what it means. */
function ask() {
  return `<span class="btn" style="gap:7px">${icon("assistant", 13, "var(--accent)")}Ask<span class="kbd" style="margin-left:2px">⌘J</span></span>`;
}

// ------------------------------------------------------------------ expand
const TOPN = [
  ["orders-api", 2104, "crit"],
  ["payments-db", 318, "warn"],
  ["search-cluster", 96, "warn"],
  ["ci-pipeline", 41, ""],
  ["checkout-web", 18, ""],
];

const MONTHS = [
  ["Sep", 100, 0, 0], ["Oct", 96, 4, 0], ["Nov", 100, 0, 0], ["Dec", 91, 6, 3],
  ["Jan", 100, 0, 0], ["Feb", 98, 2, 0], ["Mar", 100, 0, 0], ["Apr", 94, 5, 1],
  ["May", 100, 0, 0], ["Jun", 99, 1, 0], ["Jul", 100, 0, 0], ["Aug", 88, 8, 4],
];

function expand(src) {
  return src
    .replace(/<!--RAIL (\w+)-->/g, (_, a) => rail(a))
    .replace(/<!--ASK-->/g, () => ask())
    .replace(/<!--ICON (\w+)(?: (\d+))?(?: (\S+))?-->/g, (_, n, s, c) =>
      icon(n, s ? Number(s) : 15, (c || "currentColor").trim()))
    .replace(/<!--SPARK (\d+) (\d+) (\S+) (\d+) (\w+)(?: ([\d.]+))?-->/g,
      (_, w, h, c, s, sh, wa) => spark(+w, +h, c, +s, sh, wa ? +wa : null))
    .replace(/<!--AREA (\d+) (\d+) (\S+) (\d+) (\w+)(?: ([\d.]+))?-->/g,
      (_, w, h, c, s, sh, d) => area(+w, +h, c, +s, sh, d ? +d : null))
    .replace(/<!--HIST (\d+) (\d+) (\d+) (\d+)-->/g, (_, w, h, n, s) => hist(+w, +h, +n, +s))
    .replace(/<!--LANE (\d+) (\d+) (\S+)-->/g, (_, w, b, sp) => lane(+w, +b, sp))
    .replace(/<!--TICKS (\d+) (\d+) (\d+)-->/g, (_, w, c, m) => ticks(+w, +c, +m))
    .replace(/<!--DOT (\w+)(?: (\d+))?-->/g, (_, st, sz) => dot(st, sz ? +sz : 9))
    .replace(/<!--BADGE (\w+)(?: (\d+))?-->/g, (_, st, sz) => badge(st, sz ? +sz : 9))
    .replace(/<!--MARK (\S+) (\S+)(?: (\d+))?-->/g, (_, t, c, sz) => mark(t, c, sz ? +sz : 20))
    .replace(/<!--CELLS (\d+) (\d+) (\d+)-->/g, (_, c, s, b) => cells(+c, +s, +b))
    .replace(/<!--FAN (\d+) (\d+) (\d+)-->/g, (_, w, h, s) => fan(+w, +h, +s))
    .replace(/<!--SCATTER (\d+) (\d+) (\d+) (\d+)-->/g, (_, w, h, s, n) => scatter(+w, +h, +s, +n))
    .replace(/<!--BULLET ([\d.]+) ([\d.]+) (\w+) (\d+)-->/g, (_, p, l, t, d) => bullet(+p, +l, t, +d))
    .replace(/<!--ARC (\d+) ([\d.]+) (\w+)-->/g, (_, sz, p, t) => arc(+sz, +p, t))
    .replace(/<!--CALENDAR (\d+) (\d+)-->/g, (_, w, s) => calendar(+w, +s))
    .replace(/<!--HBARS-->/g, () => hbars(TOPN))
    .replace(/<!--STACKBARS-->/g, () => stackbars(MONTHS));
}

function wrap(body, w, h) {
  return `<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <script src="./support.js"></script>
</head>
<body>
<x-dc>
<helmet>
  <style>
${tokens}
  </style>
</helmet>
${body}
</x-dc>
<script data-dc-script data-props='{"$preview":{"width":${w},"height":${h}}}'>
class Component extends DCLogic {
  renderVals() {
    return {};
  }
}
</script>
</body>
</html>
`;
}

const SIZES = JSON.parse(readFileSync(new URL("./sizes.json", import.meta.url), "utf8"));

function emit(name, body) {
  const size = SIZES[name];
  if (!size) throw new Error("no size entry for " + name);
  const leftover = body.match(/<!--(ICON|RAIL|SPARK|AREA|HIST|DOT|BADGE|MARK|CELLS|LANE|TICKS)[^>]*-->/g);
  if (leftover) throw new Error(`${name}: unexpanded markers -> ${[...new Set(leftover)].join(", ")}`);
  writeFileSync(new URL(`./${name}.dc.html`, import.meta.url), wrap(body, size[0], size[1]));
}

let n = 0;
for (const file of readdirSync(new URL("./parts", import.meta.url))) {
  const m = /^(.+)\.body\.html$/.exec(file);
  if (!m) continue;
  const name = m[1];
  const body = expand(readFileSync(new URL(`./parts/${file}`, import.meta.url), "utf8").trim());
  emit(name, body);
  n += 1;
  // Every screen gets its light twin from the same markup plus one class, so
  // the two palettes can never drift apart.
  if (SIZES[name + "Light"]) {
    emit(name + "Light", body.replace('<div style="width:1440px', '<div class="light" style="width:1440px'));
    n += 1;
  }
}

console.log(`built ${n} artboards`);
