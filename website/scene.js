/*
 * root's scene: the anatomy of an outage, told by one cloud of particles.
 *
 * | Section   | Shape                                   | Motion                         | Arrival |
 * | hero      | the root mark, its top node lit         | sways, breathes                | drift   |
 * | estate    | the component matrix: small boxes,      | the failing boxes pulse        | scatter |
 * |           | a few of them down                      |                                |         |
 * | incident  | a dependency chain                      | scrolling spreads the red      | stream  |
 * |           |                                         | along the chain, in order      |         |
 * | config    | a metric climbing to its ceiling        | scrolling draws the line, the  | sweep   |
 * |           |                                         | forecast meets the limit       |         |
 * | graph     | the estate graph's fan                  | light travels the links        | spiral  |
 * | explore   | a latency heatmap, a slow stretch lit   | columns breathe                | sweep   |
 * | assistant | ✦, the one glyph every AI affordance    | turns, breathes                | spiral  |
 * |           | in root carries                         |                                |         |
 * | run       | a laptop and a rack: the two ways       | sways, the rack's lights blink | drift   |
 * | close     | the root mark again                     | sways                          | flock   |
 */
// One column: a shape waiting on screen would sit over the text, so it scrolls away with its anchor.
if (window.matchMedia('(max-width: 860px)').matches) document.querySelectorAll('[data-shape]').forEach((a) => { a.dataset.hold = 'anchor'; });

window.MURMURATION_SCENE = (kit) => {
  const { random, gauss, points, svg } = kit;

  // A straight run of particles, thickened a little so a link reads as a line of light.
  const along = (o, ax, ay, bx, by, w) => {
    const u = random();
    o[0] = ax + (bx - ax) * u + gauss() * 0.004;
    o[1] = ay + (by - ay) * u + gauss() * 0.004;
    o[2] = gauss() * 0.012;
    o[3] = w;
  };
  // A node: a filled disc with a soft edge.
  const node = (o, x, y, r, w) => {
    const a = random() * Math.PI * 2;
    const d = Math.sqrt(random()) * r;
    o[0] = x + Math.cos(a) * d;
    o[1] = y + Math.sin(a) * d;
    o[2] = gauss() * 0.012;
    o[3] = w;
  };

  // The small boxes from the Overview: one row per connection, a few cells down.
  const COLS = 36, ROWS = 15;
  const DOWN = new Set(['2:0', '2:1', '2:2', '2:3', '2:4', '5:0', '5:1', '9:11', '9:12', '12:27']);
  const cellsAt = (i, count) => {
    const cell = Math.floor((i / count) * COLS * ROWS);
    return { c: cell % COLS, r: Math.floor(cell / COLS) };
  };

  // The chain: orders-api turned first, then what depends on it.
  const CHAIN = [[-0.96, -0.1], [-0.56, 0.22], [-0.16, -0.16], [0.24, 0.26], [0.26, -0.34], [0.66, 0.04], [0.98, 0.32], [0.98, -0.26]];
  const LINKS = [[0, 1], [1, 2], [2, 3], [2, 4], [3, 5], [5, 6], [5, 7]];

  // The fan: what links to the focus on the left, what it links to on the right.
  const curve = (from, to, u) => {
    const s = u * u * (3 - 2 * u);
    return [from[0] + (to[0] - from[0]) * u, from[1] + (to[1] - from[1]) * s];
  };
  const LEFT = [0.62, 0.36, 0.1, -0.16, -0.42, -0.68], RIGHT = [0.7, 0.46, 0.22, -0.02, -0.26, -0.5, -0.74];

  return {
    // [base, accent, alpha, glow, fog colour, fog amount]. Accents are the app's own
    // status colours and carry the same meaning here: red is down, amber is close to it.
    palettes: {
      mark: { light: ['#d6d6de', '#ededf0', 0.95, 0.9, '#3a4a66', 0.07] },
      matrix: { light: ['#4f8a6c', '#e0705a', 0.95, 0.85, '#2b4c3b', 0.07] },
      crit: { light: ['#6f6f7a', '#e0705a', 0.95, 0.9, '#5a2a22', 0.1] },
      warn: { light: ['#86a8cc', '#d9a84e', 0.95, 0.9, '#5a4620', 0.09] },
      data: { light: ['#5f7591', '#86a8cc', 0.95, 0.9, '#3a4a66', 0.08] },
      ai: { light: ['#ededf0', '#86a8cc', 0.95, 0.9, '#3a4a66', 0.08] },
    },

    motions: {
      // The red spreads along the chain as the section is read: where it reached, it stays.
      spreading: { k: [0, 0, 0, 0], glsl: `
        float front = mix(-1.25, 1.35, clamp(prog * 1.5 - 0.08, 0.0, 1.0));
        acc *= smoothstep(front + 0.02, front - 0.22, p.x);
        lit *= 1.0 + 0.5 * acc * sin(p.x * 9.0 - t * 3.0);` },
      // The line draws itself; part 1 is the limit, always there.
      climbing: { k: [0, 0, 0, 0], glsl: `
        float part = floor(w / 2.0);
        if (part < 0.5 || part > 1.5) {
          float front = mix(-1.0, 1.08, clamp(prog * 1.5, 0.0, 1.0));
          lit *= smoothstep(front + 0.04, front - 0.04, p.x);
        }` },
      // Light travelling out along the links.
      travelling: { k: [0, 0, 0, 0], glsl: 'lit *= 0.7 + 0.45 * sin(abs(p.x) * 12.0 - t * 2.2 + r.x * 0.6);' },
    },

    shapes: {
      mark: {
        build: svg({
          view: 24,
          stroke: 1.9,
          paths: [
            'M5.6 5.7V19.2M5.6 9.7h9.3M5.6 14.45h9.3M5.6 19.2h9.3',
            'M15.7 9.7a1.7 1.7 0 1 0 3.4 0a1.7 1.7 0 1 0 -3.4 0',
            'M15.7 14.45a1.7 1.7 0 1 0 3.4 0a1.7 1.7 0 1 0 -3.4 0',
            'M15.7 19.2a1.7 1.7 0 1 0 3.4 0a1.7 1.7 0 1 0 -3.4 0',
          ],
          accent: [{ cx: 5.6, cy: 3.7, r: 2 }],
        }),
        motion: [{ use: 'sway', k: [0.2, 0.42] }, { use: 'breathe', k: [0.02, 7] }],
        palette: 'mark',
      },

      matrix: {
        build: points((i, o, _r, count) => {
          const { c, r } = cellsAt(i, count);
          const pitch = 2 / COLS, size = pitch * 0.7;
          o[0] = -1 + (c + 0.5) * pitch + (random() - 0.5) * size;
          o[1] = (ROWS / COLS) * (1 - (2 * (r + 0.5)) / ROWS) + (random() - 0.5) * size;
          o[2] = gauss() * 0.006;
          o[3] = DOWN.has(`${r}:${c}`) ? 1 : 0;
        }),
        motion: [{ use: 'pulse', k: [1.6, 0.6] }, { use: 'twinkle', k: [0.16, 0.7] }, { use: 'sway', k: [0.1, 0.35] }],
        view: [-0.32, 0.3],
        palette: 'matrix',
        size: 0.9,
      },

      spread: {
        build: points((i, o) => {
          if (random() < 0.4) {
            const n = CHAIN[Math.floor(random() * CHAIN.length)];
            node(o, n[0], n[1], 0.05, 1);
          } else {
            const [a, b] = LINKS[Math.floor(random() * LINKS.length)];
            along(o, CHAIN[a][0], CHAIN[a][1], CHAIN[b][0], CHAIN[b][1], 1);
          }
        }),
        motion: ['spreading', { use: 'sway', k: [0.08, 0.4] }],
        palette: 'crit',
      },

      ceiling: {
        build: points((i, o) => {
          const roll = random();
          if (roll < 0.17) {
            // The limit, part 1.
            o[0] = -1 + random() * 2.06;
            o[1] = 0.56 + gauss() * 0.004;
            o[2] = gauss() * 0.008;
            o[3] = 2 + 1;
          } else if (roll < 0.34) {
            // The forecast, part 2: the same climb, drawn in the accent, until it meets the limit.
            const u = 0.66 + random() * 0.26;
            const x = -1 + 2 * u;
            o[0] = x + gauss() * 0.003;
            o[1] = -0.62 + 1.18 * Math.pow(u / 0.92, 1.9) + gauss() * 0.005;
            o[2] = gauss() * 0.008;
            o[3] = 4 + 1;
          } else {
            // The metric so far, part 0, with some weather in it.
            const u = random() * 0.66;
            const x = -1 + 2 * u;
            const weather = Math.sin(u * 37) * 0.018 + Math.sin(u * 91 + 1) * 0.012;
            o[0] = x;
            o[1] = -0.62 + 1.18 * Math.pow(u / 0.92, 1.9) + weather + gauss() * 0.007;
            o[2] = gauss() * 0.01;
            o[3] = 0;
          }
        }),
        motion: ['climbing', { use: 'sway', k: [0.07, 0.35] }],
        palette: 'warn',
      },

      fan: {
        build: points((i, o) => {
          const roll = random();
          const left = random() < 0.46;
          const rows = left ? LEFT : RIGHT;
          const k = Math.floor(random() * rows.length);
          const end = [left ? -0.98 : 0.98, rows[k]];
          const bad = (left && k === 1) || (!left && (k === 3 || k === 4));
          if (roll < 0.09) node(o, 0, 0, 0.075, 1);
          else if (roll < 0.34) node(o, end[0], end[1], 0.026, bad ? 1 : 0);
          else {
            const [x, y] = curve(end, [0, 0], random());
            o[0] = x + gauss() * 0.003;
            o[1] = y + gauss() * 0.003;
            o[2] = gauss() * 0.01;
            o[3] = bad ? 1 : 0;
          }
        }),
        motion: ['travelling', { use: 'sway', k: [0.12, 0.3] }],
        palette: 'crit',
      },

      heat: {
        build: points((i, o, _r, count) => {
          const cols = 46, rows = 13;
          const cell = Math.floor((i / count) * cols * rows);
          const c = cell % cols, r = Math.floor(cell / cols);
          const pw = 2 / cols, ph = 1.1 / rows;
          o[0] = -1 + (c + 0.5) * pw + (random() - 0.5) * pw * 0.84;
          o[1] = -0.55 + (r + 0.5) * ph + (random() - 0.5) * ph * 0.84;
          o[2] = gauss() * 0.006;
          // Most requests low and fast; a slow stretch two-thirds along pushes them up.
          const slow = c > cols * 0.6 && c < cols * 0.78;
          const peak = slow ? 8.5 : 3.2 + Math.sin(c * 0.7) * 0.5;
          const hot = Math.exp(-Math.pow((r - peak) / (slow ? 2.4 : 1.7), 2));
          o[3] = hot;
        }),
        motion: [{ use: 'twinkle', k: [0.14, 0.6] }, { use: 'sway', k: [0.1, 0.32] }],
        view: [-0.28, 0.26],
        palette: 'data',
        size: 0.9,
      },

      spark: {
        build: svg({
          view: 24,
          paths: ['M12 1.5C12.7 7.6 16.4 11.3 22.5 12C16.4 12.7 12.7 16.4 12 22.5C11.3 16.4 7.6 12.7 1.5 12C7.6 11.3 11.3 7.6 12 1.5Z'],
        }),
        motion: [{ use: 'breathe', k: [0.05, 5] }, { use: 'sway', k: [0.32, 0.5] }],
        palette: 'ai',
      },

      pair: {
        build: svg({
          view: 64,
          stroke: 1.3,
          paths: [
            // The laptop.
            'M6 16h24a2 2 0 0 1 2 2v17H4V18a2 2 0 0 1 2-2Z',
            'M1 39h34',
            // The rack: three units.
            'M42 12h20v9H42Z',
            'M42 24h20v9H42Z',
            'M42 36h20v9H42Z',
          ],
          accent: [{ cx: 46, cy: 16.5, r: 1.2 }, { cx: 46, cy: 28.5, r: 1.2 }, { cx: 46, cy: 40.5, r: 1.2 }],
        }),
        motion: [{ use: 'pulse', k: [1.2, 0.7] }, { use: 'sway', k: [0.16, 0.36] }],
        palette: 'ai',
      },
    },
  };
};
