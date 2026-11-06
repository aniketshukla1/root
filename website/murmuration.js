/*
 * Murmuration: one cloud of particles, fixed behind the page, that takes a different shape
 * for each section and moves between them as you scroll.
 *
 * The engine has no shapes of its own. Each site writes a scene file (see scene.js) that
 * defines window.MURMURATION_SCENE: its palettes, its shapes (from SVG, text or a points
 * function), the idle motion of each shape (built-in motions or the site's own GLSL) and how
 * particles travel into each section. Load the scene first, then this file:
 *   <script src="scene.js" defer></script><script src="murmuration.js" defer></script>
 *
 * Anchors: any element with data-shape is an anchor and belongs to the <section> it sits in.
 *   data-shape    a key of the scene's shapes
 *   data-palette  a key of the scene's palettes (defaults to the shape's palette, then the first)
 *   data-fit      contain | width | height | cover | long (how the shape is sized to the anchor;
 *                 long turns a shape vertical when the box is tall)
 *   data-scale    multiplier on that size
 *   data-hold     "anchor" keeps the shape on its anchor (it scrolls away with it); the default
 *                 waits on screen while its section is visible
 *   data-enter    how particles travel into this shape: drift (default), sweep, scatter, stream,
 *                 spiral, flock
 * While the edge between two sections is on screen, each keeps its own shape and particles hop
 * across in proportion to how much of the next section shows; each hop takes about 0.6 s, so
 * nothing is left hanging when scrolling stops. A shape in play stays inside the visible part
 * of its section. Particles dim behind headings, .lead text and [data-quiet] blocks.
 * ?debug-scene in the URL puts the current pair, frames and sections on window.__scene.
 *
 * Page contract: <div class="scene-layer" data-scene><canvas class="scene-fog"></canvas>
 * <canvas class="scene-dots"></canvas></div> as the LAST child of <body>, position: fixed;
 * z-index: 0, with section content at z-index: 1. Needs WebGL2. The page must be complete
 * without this file; it adds .has-scene to <html> once the first frame is on screen.
 */
(() => {
  'use strict';

  const root = document.documentElement;
  const layer = document.querySelector('[data-scene]');
  // With reduced motion the shapes still appear, held still, and change only as you scroll.
  const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (!layer || typeof window.MURMURATION_SCENE === 'undefined') return;
  const dots = layer.querySelector('.scene-dots');
  const fogCanvas = layer.querySelector('.scene-fog');
  const anchorEls = Array.from(document.querySelectorAll('[data-shape]'));
  if (!dots || anchorEls.length === 0) return;
  const gl = dots.getContext('webgl2', { alpha: true, antialias: false, premultipliedAlpha: true, powerPreference: 'high-performance' });
  if (!gl) return;

  const small = Math.min(window.innerWidth, window.innerHeight) < 700 || (navigator.hardwareConcurrency || 8) <= 4;
  const N = small ? 6500 : 14000;
  const TEX_W = 1024;
  const ROWS = Math.ceil(N / TEX_W);

  let seed = 20240917;
  const random = () => {
    seed = (seed * 16807) % 2147483647;
    return (seed - 1) / 2147483646;
  };
  const gauss = () => {
    let u = 0;
    let v = 0;
    while (u === 0) u = random();
    while (v === 0) v = random();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  };

  /* ---------- The kit: what a scene builds its shapes with ---------- */

  // Samples the filled pixels of a canvas drawing into particles, centred and scaled so the
  // drawing's longer side spans -1..1. accent(sx, sy) marks particles that take the accent colour.
  function sample(draw, w, h, depth, accent) {
    const canvas = document.createElement('canvas');
    canvas.width = w;
    canvas.height = h;
    const g = canvas.getContext('2d', { willReadFrequently: true });
    draw(g);
    const px = g.getImageData(0, 0, w, h).data;
    const filled = [];
    let x0 = w;
    let x1 = 0;
    let y0 = h;
    let y1 = 0;
    for (let i = 0; i < w * h; i++) {
      if (px[i * 4 + 3] < 140) continue;
      filled.push(i);
      const x = i % w;
      const y = (i / w) | 0;
      if (x < x0) x0 = x;
      if (x > x1) x1 = x;
      if (y < y0) y0 = y;
      if (y > y1) y1 = y;
    }
    const cx = (x0 + x1 + 1) / 2;
    const cy = (y0 + y1 + 1) / 2;
    const span = Math.max(x1 - x0 + 1, y1 - y0 + 1) / 2 || 1;
    return (out, start, count) => {
      for (let i = 0; i < count; i++) {
        const o = (start + i) * 4;
        if (!filled.length) { out[o] = out[o + 1] = out[o + 2] = out[o + 3] = 0; continue; }
        const f = filled[(random() * filled.length) | 0];
        const sx = (f % w) + random();
        const sy = ((f / w) | 0) + random();
        out[o] = (sx - cx) / span;
        out[o + 1] = -(sy - cy) / span;
        out[o + 2] = (random() - 0.5) * 2 * depth;
        out[o + 3] = accent ? accent(sx, sy) : 0;
      }
    };
  }

  const kit = {
    N,
    random,
    gauss,
    // SVG paths in a view box, filled (or stroked, with stroke > 0). Circles in `accent`
    // ({ cx, cy, r } in view units) are drawn too and take the accent colour.
    svg({ view = 48, paths = [], accent = [], stroke = 0, depth = 0.08, resolution = 256 } = {}) {
      const k = resolution / view;
      return sample((g) => {
        g.scale(k, k);
        g.lineWidth = stroke;
        g.lineCap = 'round';
        g.lineJoin = 'round';
        paths.forEach((d) => (stroke > 0 ? g.stroke(new Path2D(d)) : g.fill(new Path2D(d))));
        accent.forEach((c) => { g.beginPath(); g.arc(c.cx, c.cy, c.r, 0, Math.PI * 2); g.fill(); });
      }, resolution, resolution, depth, (sx, sy) => (accent.some((c) => (sx / k - c.cx) ** 2 + (sy / k - c.cy) ** 2 < c.r * c.r) ? 1 : 0));
    },
    // A word or two in a font (after the page's fonts load).
    text({ text = '', font = '700 140px system-ui, sans-serif', depth = 0.05 } = {}) {
      const probe = document.createElement('canvas').getContext('2d');
      probe.font = font;
      const w = Math.ceil(probe.measureText(text).width) + 20;
      const h = Math.ceil(parseFloat(font.match(/(\d+(\.\d+)?)px/)?.[1] || 140) * 1.4);
      return sample((g) => {
        g.font = font;
        g.textBaseline = 'middle';
        g.fillText(text, 10, h / 2);
      }, w, h, depth, null);
    },
    // Any shape at all: fn(i, out, random) sets out[0..3] to x, y, z (about -1..1) and w for
    // particle i. w's whole part / 2 is the particle's part (see mix); its rest is the accent.
    points(fn) {
      const v = [0, 0, 0, 0];
      return (out, start, count) => {
        for (let i = 0; i < count; i++) {
          v[0] = v[1] = v[2] = v[3] = 0;
          fn(i, v, random, count);
          out.set(v, (start + i) * 4);
        }
      };
    },
    // A builder moved and scaled: for placing one part beside another inside a mix.
    move(build, { x = 0, y = 0, z = 0, scale = 1 } = {}) {
      return (out, start, count) => {
        build(out, start, count);
        for (let i = 0; i < count; i++) {
          const o = (start + i) * 4;
          out[o] = out[o] * scale + x;
          out[o + 1] = out[o + 1] * scale + y;
          out[o + 2] = out[o + 2] * scale + z;
        }
      };
    },
    // Several builders in one shape: [builder, share] pairs. Part k's particles get 2k added
    // to w, so a motion can move one part and not another (see "part" on a motion).
    mix(...parts) {
      return (out, start, count) => {
        const total = parts.reduce((s, [, share]) => s + share, 0);
        let at = 0;
        parts.forEach(([build, share], k) => {
          const n = k === parts.length - 1 ? count - at : Math.round((count * share) / total);
          build(out, start + at, n);
          for (let i = 0; i < n; i++) out[(start + at + i) * 4 + 3] += 2 * k;
          at += n;
        });
      };
    },
  };

  /* ---------- Motions: how a shape moves while it is held ---------- */

  // Each motion is GLSL that may change p (the particle, in shape units), lit (brightness) and
  // acc (accent mix), given w (the particle's tag), r (four random numbers), t (seconds), k (the
  // motion's four numbers) and prog (0 as the section enters the screen, 1 as it leaves).
  const MOTIONS = {
    sway: { k: [0.25, 0.6, 0, 0], glsl: 'p = rotY(p, sin(t * k.y) * k.x); p = rotX(p, sin(t * k.y * 0.71 + 1.3) * k.x * 0.4);' },
    spin: { k: [0.15, 0, 0, 0], glsl: 'p = rotY(p, t * k.x); p = rotX(p, t * k.y); p.xy = rot2(p.xy, t * k.z);' },
    breathe: { k: [0.12, 8, 0, 0], glsl: 'p *= 1.0 + k.x * sin(6.28318 * t / k.y);' },
    flow: { k: [-1, 0, 0, 0.25], glsl: `
      vec3 d = normalize(k.xyz + vec3(1e-5));
      float s = dot(p, d);
      float n = wrap(s + t * k.w);
      p += d * (n - s);
      lit *= smoothstep(1.0, 0.82, abs(n));` },
    drift: { k: [0, 1, 0, 0.2], glsl: `
      vec3 d = normalize(k.xyz + vec3(1e-5));
      float s = dot(p, d);
      float n = wrap(s + t * k.w * (0.5 + r.y));
      p += d * (n - s);
      p += normalize(cross(d, vec3(0.0, 0.0, 1.0)) + vec3(1e-4)) * sin(t * 1.3 + r.x * 40.0) * 0.025;
      lit *= smoothstep(1.0, 0.75, abs(n));` },
    orbit: { k: [0.03, 1, 0, 0], glsl: 'p += vec3(cos(t * k.y + r.x * 6.28318), sin(t * k.y * 1.3 + r.z * 6.28318), 0.5 * sin(t * k.y * 0.7 + r.y * 6.28318)) * k.x;' },
    swell: { k: [3, 0.8, 0.06, 0], glsl: 'p.y += sin(p.x * k.x + t * k.y) * k.z + sin(p.z * k.x * 1.7 - t * k.y * 0.8) * k.z * 0.5;' },
    flutter: { k: [0.04, 1.2, 3, 0], glsl: 'p += vec3(sin(p.y * k.z + t * k.y + r.x * 6.0), sin(p.z * k.z + t * k.y * 1.1 + r.y * 6.0), sin(p.x * k.z + t * k.y * 0.9 + r.z * 6.0)) * k.x;' },
    twinkle: { k: [0.45, 1, 0, 0], glsl: 'lit *= 1.0 - k.x + k.x * (0.5 + 0.5 * sin(t * k.y * (1.0 + r.y * 2.0) + r.y * 40.0));' },
    pulse: { k: [2.5, 0.5, 0, 0], glsl: 'lit *= 1.0 - k.y * acc * (0.5 - 0.5 * sin(6.28318 * t / k.x + r.y * 1.5));' },
    lift: { k: [-0.3, 0.2, 0, 0], glsl: 'p.y += mix(k.x, k.y, clamp(prog * 1.6 - 0.3, 0.0, 1.0));' },
  };
  // How particles travel into a shape (data-enter on the destination anchor).
  const ENTERS = ['drift', 'sweep', 'scatter', 'stream', 'spiral', 'flock'];

  /* ---------- The scene ---------- */

  const hex = (h) => [parseInt(h.slice(1, 3), 16) / 255, parseInt(h.slice(3, 5), 16) / 255, parseInt(h.slice(5, 7), 16) / 255];
  const scene = typeof window.MURMURATION_SCENE === 'function' ? null : window.MURMURATION_SCENE;

  function start(def) {
    const palettes = {};
    Object.entries(def.palettes || {}).forEach(([name, p]) => {
      const one = (a) => ({ base: hex(a[0]), accent: hex(a[1]), alpha: a[2], glow: a[3], fog: hex(a[4]), fogAmount: a[5] });
      palettes[name] = { light: one(p.light), dark: one(p.dark || p.light) };
    });
    const paletteNames = Object.keys(palettes);
    if (!paletteNames.length) return;

    Object.entries(def.motions || {}).forEach(([name, m]) => { MOTIONS[name] = { k: m.k || [0, 0, 0, 0], glsl: m.glsl }; });
    const motionNames = Object.keys(MOTIONS);

    const shapeNames = Object.keys(def.shapes || {});
    if (!shapeNames.length) return;
    const shapes = shapeNames.map((name) => {
      const s = def.shapes[name];
      const list = [].concat(s.motion || []).slice(0, 3).map((m) => {
        const spec = typeof m === 'string' ? { use: m } : m;
        const id = motionNames.indexOf(spec.use) + 1;
        const k = (MOTIONS[spec.use] ? MOTIONS[spec.use].k : [0, 0, 0, 0]).slice();
        (spec.k || []).forEach((v, i) => { k[i] = v; });
        return { id: id > 0 ? id : 0, k, part: spec.part === undefined ? -1 : spec.part };
      });
      while (list.length < 3) list.push({ id: 0, k: [0, 0, 0, 0], part: -1 });
      return {
        name,
        build: s.build,
        motions: list,
        view: s.view || [0, 0],
        size: s.size || 1,
        palette: s.palette,
        settle: s.settle !== false,
        reach: s.reach,
      };
    });

    // Build every shape into one float texture: shape k's particles fill rows k*ROWS onward.
    const data = new Float32Array(TEX_W * ROWS * shapes.length * 4);
    const one = new Float32Array(TEX_W * ROWS * 4);
    shapes.forEach((shape, k) => {
      one.fill(0);
      try { shape.build(one, 0, N); } catch (error) { console.warn('murmuration: shape', shape.name, error); }
      if (!shape.reach) {
        const ys = [];
        for (let i = 0; i < N; i += 7) ys.push(one[i * 4 + 1]);
        ys.sort((a, b) => a - b);
        shape.reach = [ys[Math.floor(ys.length * 0.01)] || -1, ys[Math.floor(ys.length * 0.99)] || 1];
      }
      data.set(one, k * TEX_W * ROWS * 4);
    });

    const motionGlsl = motionNames.map((name, i) => `void m${i + 1}(inout vec3 p, inout float lit, inout float acc, float w, vec4 r, float t, vec4 k, float prog) {\n${MOTIONS[name].glsl}\n}`).join('\n');
    const dispatch = motionNames.map((_, i) => `  else if (id < ${i + 1}.5) m${i + 1}(p, lit, acc, w, r, t, k, prog);`).join('\n');

    const VERT = `#version 300 es
precision highp float;
precision highp int;
uniform highp sampler2D uShapes;
in vec4 aRand;
in float aHop;
uniform vec2 uRes; uniform float uDpr; uniform float uTime; uniform float uSize;
uniform float uFrom; uniform float uTo; uniform float uEnter;
uniform vec4 uQuiet[4];
uniform vec3 uFromFrame; uniform vec3 uToFrame; uniform vec2 uRot;
uniform vec3 uFromBase; uniform vec3 uFromAccent; uniform vec3 uToBase; uniform vec3 uToAccent;
uniform vec2 uAlpha; uniform vec2 uTilt; uniform vec2 uPointer;
uniform vec3 uFromMotion; uniform vec3 uFromPart; uniform vec4 uFromK[3]; uniform vec2 uFromView; uniform float uFromSize; uniform float uFromProg;
uniform vec3 uToMotion; uniform vec3 uToPart; uniform vec4 uToK[3]; uniform vec2 uToView; uniform float uToSize; uniform float uToProg;
out vec3 vColor; out float vAlpha;

vec3 rotX(vec3 p, float a) { float c = cos(a), s = sin(a); return vec3(p.x, c * p.y - s * p.z, s * p.y + c * p.z); }
vec3 rotY(vec3 p, float a) { float c = cos(a), s = sin(a); return vec3(c * p.x + s * p.z, p.y, -s * p.x + c * p.z); }
vec2 rot2(vec2 p, float a) { float c = cos(a), s = sin(a); return vec2(c * p.x - s * p.y, s * p.x + c * p.y); }
float hash(float n) { return fract(sin(n) * 43758.5453); }
float wrap(float x) { return mod(x + 1.0, 2.0) - 1.0; }

${motionGlsl}

void motion(float id, inout vec3 p, inout float lit, inout float acc, float w, vec4 r, float t, vec4 k, float prog) {
  if (id < 0.5) return;
${dispatch}
}

vec4 shapeAt(float shape) {
  int id = gl_VertexID;
  return texelFetch(uShapes, ivec2(id % ${TEX_W}, int(shape + 0.5) * ${ROWS} + id / ${TEX_W}), 0);
}

// A shape's particle, moved by up to three motions, then turned to the shape's view.
vec3 pose(vec4 s, vec3 ids, vec3 parts, vec4 k0, vec4 k1, vec4 k2, vec2 view, float rz, float prog, out float lit, out float acc) {
  vec3 p = s.xyz;
  float part = floor(s.w / 2.0);
  lit = 1.0;
  acc = clamp(mod(s.w, 2.0), 0.0, 1.0);
  if (parts.x < -0.5 || abs(parts.x - part) < 0.5) motion(ids.x, p, lit, acc, s.w, aRand, uTime, k0, prog);
  if (parts.y < -0.5 || abs(parts.y - part) < 0.5) motion(ids.y, p, lit, acc, s.w, aRand, uTime, k1, prog);
  if (parts.z < -0.5 || abs(parts.z - part) < 0.5) motion(ids.z, p, lit, acc, s.w, aRand, uTime, k2, prog);
  p.xy = rot2(p.xy, rz);
  p = rotY(p, view.x + uTilt.x);
  return rotX(p, view.y + uTilt.y);
}

void main() {
  vec4 r = aRand;
  float m = smoothstep(0.0, 1.0, aHop);
  float litA; float accA; float litB; float accB;
  vec4 sa = shapeAt(uFrom);
  vec4 sb = shapeAt(uTo);
  vec3 a = pose(sa, uFromMotion, uFromPart, uFromK[0], uFromK[1], uFromK[2], uFromView, uRot.x, uFromProg, litA, accA);
  vec3 b = pose(sb, uToMotion, uToPart, uToK[0], uToK[1], uToK[2], uToView, uRot.y, uToProg, litB, accB);

  // The way particles travel between the two shapes: mm is how far along, off a detour.
  float mm = m;
  vec3 off = vec3(0.0);
  float arc = sin(m * 3.14159);
  if (uEnter < 0.5) {
    off = arc * vec3(sin(r.x * 61.0 + uTime * 0.7), cos(r.x * 47.0 + uTime * 0.6), sin(r.x * 29.0)) * 0.45;
  } else if (uEnter < 1.5) {
    float d = clamp(b.x * 0.5 + 0.5, 0.0, 1.0) * 0.55;
    mm = smoothstep(d, d + 0.45, m);
    off = vec3(0.0, sin(mm * 3.14159) * 0.14, 0.0);
  } else if (uEnter < 2.5) {
    vec3 mid = mix(a, b, m);
    off = normalize(mid + vec3(sin(r.x * 31.0), cos(r.y * 37.0), sin(r.z * 17.0)) * 0.35 + vec3(1e-4)) * arc * 0.95;
  } else if (uEnter < 3.5) {
    mm = smoothstep(r.x * 0.38, r.x * 0.38 + 0.62, m);
    off = vec3(0.0, sin(mm * 3.14159) * 0.75, sin(r.y * 23.0) * 0.12 * sin(mm * 3.14159));
  } else if (uEnter < 4.5) {
    vec3 mid = mix(a, b, m);
    off.xy = rot2(mid.xy, arc * 3.14159 * (0.6 + r.y * 0.8)) - mid.xy;
  } else {
    mm = smoothstep(0.08, 0.92, m);
    float u = mm * 4.712 + r.x * 2.4;
    vec3 flock = vec3(sin(u) * 1.1, sin(2.0 * u) * 0.35 + (r.y - 0.5) * 0.12, cos(u) * 0.6);
    off = (flock - mix(a, b, mm)) * sin(mm * 3.14159) * 0.85;
  }
  vec3 p = mix(a, b, mm) + off;
  // A few particles drift loosely around every shape.
  p += step(0.96, r.z) * vec3(sin(uTime * 0.31 + r.x * 50.0), cos(uTime * 0.27 + r.x * 70.0), sin(uTime * 0.21 + r.x * 90.0)) * 0.22;

  vec3 frame = mix(uFromFrame, uToFrame, mm);
  float persp = 2.8 / (2.8 - clamp(p.z, -2.0, 1.6));
  vec2 screen = frame.xy + vec2(p.x, -p.y) * frame.z * persp;
  vec2 d = screen - uPointer;
  screen += normalize(d + vec2(0.0001)) * exp(-dot(d, d) / 9000.0) * 26.0;
  gl_Position = vec4(screen.x / uRes.x * 2.0 - 1.0, 1.0 - screen.y / uRes.y * 2.0, 0.0, 1.0);

  float acc = mix(accA, accB, mm);
  gl_PointSize = uSize * uDpr * persp * (0.6 + r.w * 0.8) * mix(uFromSize, uToSize, mm) * (1.0 + acc * 0.7);
  vColor = mix(mix(uFromBase, uFromAccent, accA), mix(uToBase, uToAccent, accB), mm);
  // Particles fade behind text, so they never compete with reading.
  float hush = 1.0;
  for (int q = 0; q < 4; q++) {
    vec4 box = uQuiet[q];
    vec2 inside = min(screen - box.xy, box.zw - screen);
    hush = min(hush, mix(1.0, 0.28, smoothstep(-24.0, 16.0, min(inside.x, inside.y))));
  }
  vAlpha = clamp(mix(uAlpha.x * litA, uAlpha.y * litB, mm), 0.0, 1.0) * (0.6 + 0.4 * r.y) * hush;
}`;

    const FRAG = `#version 300 es
precision mediump float;
in vec3 vColor; in float vAlpha;
uniform float uGlow;
out vec4 outColor;
void main() {
  vec2 c = gl_PointCoord - 0.5;
  float d = dot(c, c) * 4.0;
  // Glowing particles fade softly; ink particles on a light sky keep a crisp edge.
  float soft = 1.0 - smoothstep(0.0, 1.0, d);
  float a = mix(1.0 - smoothstep(0.45, 1.0, d), soft * soft, uGlow) * vAlpha;
  if (a < 0.003) discard;
  // Always valid premultiplied colour (colour never brighter than alpha). A GPU compositor
  // drops pixels whose alpha is below their colour, so glow comes from additive blending
  // inside the canvas instead (see draw).
  outColor = vec4(vColor * a, a);
}`;

    let program;
    try {
      program = compile(gl, VERT, FRAG);
    } catch (error) {
      console.warn('murmuration: shader', error.message);
      return;
    }
    gl.useProgram(program);

    const texture = gl.createTexture();
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA32F, TEX_W, ROWS * shapes.length, 0, gl.RGBA, gl.FLOAT, data);

    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    const rand = new Float32Array(N * 4);
    for (let i = 0; i < rand.length; i++) rand[i] = random();
    const randBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, randBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, rand, gl.STATIC_DRAW);
    const randLoc = gl.getAttribLocation(program, 'aRand');
    gl.enableVertexAttribArray(randLoc);
    gl.vertexAttribPointer(randLoc, 4, gl.FLOAT, false, 0, 0);

    // Each particle's place between the two shapes in play: 0 is the first, 1 the second.
    // It moves toward its side over about 0.6 s, so particles finish their hop and rest in a
    // shape instead of hanging between sections when the scrolling stops.
    const hop = new Float32Array(N);
    const hopBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, hopBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, hop, gl.DYNAMIC_DRAW);
    const hopLoc = gl.getAttribLocation(program, 'aHop');
    gl.enableVertexAttribArray(hopLoc);
    gl.vertexAttribPointer(hopLoc, 1, gl.FLOAT, false, 0, 0);

    const u = {};
    ['uShapes', 'uRes', 'uDpr', 'uTime', 'uSize', 'uFrom', 'uTo', 'uEnter', 'uQuiet', 'uFromFrame', 'uToFrame', 'uRot', 'uFromBase', 'uFromAccent', 'uToBase', 'uToAccent', 'uAlpha', 'uTilt', 'uPointer', 'uGlow',
      'uFromMotion', 'uFromPart', 'uFromK', 'uFromView', 'uFromSize', 'uFromProg', 'uToMotion', 'uToPart', 'uToK', 'uToView', 'uToSize', 'uToProg']
      .forEach((name) => { u[name] = gl.getUniformLocation(program, name); });
    gl.uniform1i(u.uShapes, 0);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.clearColor(0, 0, 0, 0);

    const fog = makeFog();

    /* ---------- Anchors ---------- */

    const anchors = anchorEls.map((el) => {
      const index = Math.max(0, shapeNames.indexOf(el.dataset.shape));
      const shape = shapes[index];
      const palette = palettes[el.dataset.palette] ? el.dataset.palette : (palettes[shape.palette] ? shape.palette : paletteNames[0]);
      return {
        el,
        index,
        shape,
        palette,
        enter: Math.max(0, ENTERS.indexOf(el.dataset.enter || 'drift')),
        fit: el.dataset.fit || 'contain',
        scale: Number(el.dataset.scale) || 1,
        section: el.closest('section, footer') || el,
        hold: el.dataset.hold || 'screen',
      };
    });

    function frameOf(anchor, rect) {
      const w = rect.width;
      const h = rect.height;
      let size;
      let rotation = 0;
      if (anchor.fit === 'width') size = w / 2;
      else if (anchor.fit === 'height') size = h / 2;
      else if (anchor.fit === 'cover') size = Math.max(w / 4.8, h / 3);
      else if (anchor.fit === 'long') {
        if (h > w) { rotation = -Math.PI / 2; size = h / 2; } else size = w / 2;
      } else size = Math.min(w, h) / 2;
      return { x: rect.left + w / 2, y: rect.top + h / 2, size: size * anchor.scale, rotation };
    }

    /* ---------- Loop ---------- */

    let width = 0;
    let height = 0;
    let dpr = 1;
    function resize() {
      dpr = Math.min(window.devicePixelRatio || 1, small ? 1.5 : 1.75);
      width = window.innerWidth;
      height = window.innerHeight;
      dots.width = Math.round(width * dpr);
      dots.height = Math.round(height * dpr);
      gl.viewport(0, 0, dots.width, dots.height);
      if (fog) {
        fogCanvas.width = Math.ceil(width / 6);
        fogCanvas.height = Math.ceil(height / 6);
        fog.gl.viewport(0, 0, fogCanvas.width, fogCanvas.height);
      }
    }
    resize();
    window.addEventListener('resize', resize);

    const pointer = { x: -1e4, y: -1e4, tx: 0, ty: 0, sx: 0, sy: 0 };
    if (!still && window.matchMedia('(pointer: fine)').matches) {
      window.addEventListener('pointermove', (e) => {
        pointer.x = e.clientX;
        pointer.y = e.clientY;
        pointer.tx = (e.clientX / width - 0.5) * 0.5;
        pointer.ty = (e.clientY / height - 0.5) * 0.3;
      }, { passive: true });
      document.addEventListener('pointerleave', () => { pointer.x = pointer.y = -1e4; pointer.tx = pointer.ty = 0; });
    }

    const isDark = () => (root.dataset.theme || (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')) === 'dark';
    let dark = isDark();
    const syncTheme = () => { dark = isDark(); };
    new MutationObserver(syncTheme).observe(root, { attributes: true, attributeFilter: ['data-theme'] });
    const darkQuery = window.matchMedia('(prefers-color-scheme: dark)');
    if (darkQuery.addEventListener) darkQuery.addEventListener('change', syncTheme);

    const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
    const lerp = (x, y, t) => x + (y - x) * t;
    const lerp3 = (x, y, t) => [lerp(x[0], y[0], t), lerp(x[1], y[1], t), lerp(x[2], y[2], t)];

    let running = true;
    let shown = false;
    dots.addEventListener('webglcontextlost', (e) => { e.preventDefault(); running = false; root.classList.remove('has-scene'); });

    // Text blocks the particles dim behind.
    const quietEls = Array.from(document.querySelectorAll('[data-quiet], h1, h2, .lead'));
    const quiet = new Float32Array(16);
    const debug = /debug-scene/.test(location.search);
    let lastTime = 0;
    let pairFrom = -1;
    let pairTo = -1;
    let shownMix = 0;

    const setShape = (prefix, shape, prog) => {
      gl.uniform3f(u[`u${prefix}Motion`], shape.motions[0].id, shape.motions[1].id, shape.motions[2].id);
      gl.uniform3f(u[`u${prefix}Part`], shape.motions[0].part, shape.motions[1].part, shape.motions[2].part);
      gl.uniform4fv(u[`u${prefix}K`], [...shape.motions[0].k, ...shape.motions[1].k, ...shape.motions[2].k]);
      gl.uniform2f(u[`u${prefix}View`], shape.view[0], shape.view[1]);
      gl.uniform1f(u[`u${prefix}Size`], shape.size);
      gl.uniform1f(u[`u${prefix}Prog`], prog);
    };

    function draw(now) {
      if (!running) return;
      const t = still ? 4 : now / 1000;
      const dt = lastTime ? Math.min(0.1, (now - lastTime) / 1000) : 1 / 60;
      lastTime = now;
      // Anchors hidden at this width (display: none) have no box and are skipped.
      const rects = anchors.map((a) => a.el.getBoundingClientRect());
      const live = [];
      rects.forEach((r, k) => { if (r.width > 0 || r.height > 0) live.push(k); });
      if (live.length === 0) {
        gl.clear(gl.COLOR_BUFFER_BIT);
        requestAnimationFrame(draw);
        return;
      }

      // The pair in play: the anchor whose edge is nearest the middle of the screen, and the next.
      // While an edge between sections is on screen, each section keeps its own shape, and
      // particles cross to the next one in proportion to how much of it shows. Inside one
      // section with two anchors, the edge is halfway between them.
      const focus = height * 0.5;
      const band = height * 0.5;
      const centres = live.map((k) => rects[k].top + rects[k].height / 2);
      const sections = live.map((k) => anchors[k].section.getBoundingClientRect());
      const edges = [];
      for (let k = 0; k < live.length - 1; k++) {
        edges.push(anchors[live[k]].section === anchors[live[k + 1]].section ? (centres[k] + centres[k + 1]) / 2 : sections[k + 1].top);
      }
      let i = 0;
      while (i < edges.length && focus > edges[i] + band) i++;
      const j = Math.min(i + 1, live.length - 1);
      const target = i < edges.length ? clamp((focus - (edges[i] - band)) / (band * 2), 0, 1) : 0;
      const from = live[i];
      const to = live[j];
      let dirty = false;
      if (from !== pairFrom || to !== pairTo) {
        // A new pair shares one anchor with the old one, so particles resting there stay put.
        if (from === pairTo) hop.fill(0);
        else if (to === pairFrom) hop.fill(1);
        else for (let k = 0; k < N; k++) hop[k] = rand[k * 4] < target ? 1 : 0;
        pairFrom = from;
        pairTo = to;
        shownMix = target;
        dirty = true;
      }
      const step = still ? 1 : dt * 1.7;
      for (let k = 0; k < N; k++) {
        const side = rand[k * 4] < target ? 1 : 0;
        const h = hop[k];
        if (h !== side) {
          hop[k] = side > h ? Math.min(1, h + step) : Math.max(0, h - step);
          dirty = true;
        }
      }
      if (dirty) {
        gl.bindBuffer(gl.ARRAY_BUFFER, hopBuffer);
        gl.bufferSubData(gl.ARRAY_BUFFER, 0, hop);
      }
      shownMix += (target - shownMix) * (still ? 1 : Math.min(1, dt * 5));
      const mix = shownMix;
      const a = anchors[from];
      const b = anchors[to];

      // A shape stays inside the visible part of its own section while it is in play, so it
      // waits on screen instead of sliding off with an anchor near the section's edge.
      const settle = (k, anchor, frame) => {
        if (!anchor.shape.settle) return frame;
        const low = frame.rotation ? -1.15 : anchor.shape.reach[0];
        const high = frame.rotation ? 1.15 : anchor.shape.reach[1];
        const s = sections[k];
        const pad = 12;
        const top = s.top + pad + high * frame.size;
        const bottom = s.bottom - pad + low * frame.size;
        if (anchor.hold === 'anchor') {
          if (top <= bottom) frame.y = clamp(frame.y, top, bottom);
          return frame;
        }
        let lo = Math.max(top, pad + high * frame.size);
        let hi = Math.min(bottom, height - pad + low * frame.size);
        if (lo > hi) {
          if ((high - low) * frame.size > height - 2 * pad) {
            // Taller than the screen: keep at least a band of it showing, as far as the section allows.
            lo = Math.max(top, 90 + low * frame.size);
            hi = Math.min(bottom, height - 90 + high * frame.size);
          } else {
            // It fits the screen but not the part of its section still on screen: centre it on that part.
            const y = (Math.max(s.top, 0) + Math.min(s.bottom, height)) / 2 + ((high + low) / 2) * frame.size;
            lo = hi = top <= bottom ? clamp(y, top, bottom) : y;
          }
        }
        if (lo <= hi) frame.y = clamp(frame.y, lo, hi);
        else if (top <= bottom) frame.y = clamp(frame.y, top, bottom);
        return frame;
      };
      const fa = settle(i, a, frameOf(a, rects[from]));
      const fb = settle(j, b, frameOf(b, rects[to]));
      // How far each section has travelled through the screen, for scroll-driven motions.
      const progOf = (s) => clamp((height - s.top) / (height + s.height), 0, 1);
      if (debug) window.__scene = { from: a.shape.name, to: b.shape.name, enter: ENTERS[b.enter], target: +target.toFixed(3), a: [Math.round(fa.x), Math.round(fa.y), Math.round(fa.size)], b: [Math.round(fb.x), Math.round(fb.y), Math.round(fb.size)], sa: [Math.round(sections[i].top), Math.round(sections[i].bottom)], sb: [Math.round(sections[j].top), Math.round(sections[j].bottom)] };

      quiet.fill(-1e5);
      let q = 0;
      for (const el of quietEls) {
        if (q >= 4) break;
        const r = el.getBoundingClientRect();
        if (r.bottom < 0 || r.top > height || r.width === 0) continue;
        quiet[q * 4] = r.left;
        quiet[q * 4 + 1] = r.top;
        quiet[q * 4 + 2] = r.right;
        quiet[q * 4 + 3] = r.bottom;
        q++;
      }
      const pa = palettes[a.palette][dark ? 'dark' : 'light'];
      const pb = palettes[b.palette][dark ? 'dark' : 'light'];
      pointer.sx += (pointer.tx - pointer.sx) * 0.05;
      pointer.sy += (pointer.ty - pointer.sy) * 0.05;

      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.uniform2f(u.uRes, width, height);
      gl.uniform1f(u.uDpr, dpr);
      gl.uniform1f(u.uTime, t);
      gl.uniform1f(u.uSize, small ? 2.2 : 2.0);
      gl.uniform1f(u.uFrom, a.index);
      gl.uniform1f(u.uTo, b.index);
      gl.uniform1f(u.uEnter, b.enter);
      gl.uniform4fv(u.uQuiet, quiet);
      gl.uniform3f(u.uFromFrame, fa.x, fa.y, fa.size);
      gl.uniform3f(u.uToFrame, fb.x, fb.y, fb.size);
      gl.uniform2f(u.uRot, fa.rotation, fb.rotation);
      gl.uniform3fv(u.uFromBase, pa.base);
      gl.uniform3fv(u.uFromAccent, pa.accent);
      gl.uniform3fv(u.uToBase, pb.base);
      gl.uniform3fv(u.uToAccent, pb.accent);
      gl.uniform2f(u.uAlpha, pa.alpha, pb.alpha);
      gl.uniform2f(u.uTilt, pointer.sx, pointer.sy);
      gl.uniform2f(u.uPointer, pointer.x, pointer.y);
      setShape('From', a.shape, progOf(sections[i]));
      setShape('To', b.shape, progOf(sections[j]));
      const glow = lerp(pa.glow, pb.glow, mix);
      gl.uniform1f(u.uGlow, glow);
      // On dark skies particles add light where they overlap; on light skies they lay colour over.
      if (glow > 0.5) gl.blendFuncSeparate(gl.ONE, gl.ONE, gl.ONE, gl.ONE);
      else gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      gl.drawArrays(gl.POINTS, 0, N);

      if (fog) {
        const f = fog.gl;
        f.clearColor(0, 0, 0, 0);
        f.clear(f.COLOR_BUFFER_BIT);
        f.uniform2f(fog.u.uRes, fogCanvas.width, fogCanvas.height);
        f.uniform1f(fog.u.uTime, t);
        f.uniform3fv(fog.u.uColor, lerp3(pa.fog, pb.fog, mix));
        f.uniform1f(fog.u.uAmount, lerp(pa.fogAmount, pb.fogAmount, mix));
        f.uniform1f(fog.u.uScroll, window.scrollY / Math.max(1, height));
        f.drawArrays(f.TRIANGLES, 0, 3);
      }

      if (!shown) {
        shown = true;
        root.classList.add('has-scene');
      }
      if (!still) requestAnimationFrame(draw);
    }
    if (still) {
      let queued = false;
      const redraw = () => { if (!queued) { queued = true; requestAnimationFrame((now) => { queued = false; draw(now); }); } };
      window.addEventListener('scroll', redraw, { passive: true });
      window.addEventListener('resize', redraw);
      new MutationObserver(redraw).observe(root, { attributes: true, attributeFilter: ['data-theme'] });
    }
    requestAnimationFrame(draw);
  }

  function compile(context, vert, frag) {
    const program = context.createProgram();
    [[context.VERTEX_SHADER, vert], [context.FRAGMENT_SHADER, frag]].forEach(([type, source]) => {
      const shader = context.createShader(type);
      context.shaderSource(shader, source);
      context.compileShader(shader);
      if (!context.getShaderParameter(shader, context.COMPILE_STATUS)) throw new Error(context.getShaderInfoLog(shader) || 'shader');
      context.attachShader(program, shader);
    });
    context.linkProgram(program);
    if (!context.getProgramParameter(program, context.LINK_STATUS)) throw new Error(context.getProgramInfoLog(program) || 'link');
    return program;
  }

  /* ---------- Fog: a few thousand pixels of noise, stretched by CSS ---------- */

  function makeFog() {
    if (!fogCanvas) return null;
    const fgl = fogCanvas.getContext('webgl', { alpha: true, antialias: false, premultipliedAlpha: true });
    if (!fgl) return null;
    try {
      const fogProgram = compile(fgl, `
attribute vec2 aPos;
void main() { gl_Position = vec4(aPos, 0.0, 1.0); }`, `
precision mediump float;
uniform vec2 uRes; uniform float uTime; uniform vec3 uColor; uniform float uAmount; uniform float uScroll;
float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float noise(vec2 p) {
  vec2 i = floor(p); vec2 f = fract(p); vec2 w = f * f * (3.0 - 2.0 * f);
  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), w.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), w.x), w.y);
}
float fbm(vec2 p) {
  float v = 0.0; float a = 0.5;
  for (int k = 0; k < 5; k++) { v += a * noise(p); p = p * 2.03 + 17.0; a *= 0.5; }
  return v;
}
void main() {
  vec2 uv = gl_FragCoord.xy / uRes;
  vec2 p = vec2(uv.x * uRes.x / uRes.y, uv.y) * 2.4;
  p.y -= uScroll * 0.6;
  float t = uTime * 0.03;
  float n = fbm(p + vec2(t, 0.0) + fbm(p * 0.9 - vec2(0.0, t)) * 0.8);
  float a = smoothstep(0.42, 0.9, n) * uAmount;
  gl_FragColor = vec4(uColor * a, a);
}`);
      fgl.useProgram(fogProgram);
      const quad = fgl.createBuffer();
      fgl.bindBuffer(fgl.ARRAY_BUFFER, quad);
      fgl.bufferData(fgl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), fgl.STATIC_DRAW);
      const loc = fgl.getAttribLocation(fogProgram, 'aPos');
      fgl.enableVertexAttribArray(loc);
      fgl.vertexAttribPointer(loc, 2, fgl.FLOAT, false, 0, 0);
      const fog = { gl: fgl, u: {} };
      ['uRes', 'uTime', 'uColor', 'uAmount', 'uScroll'].forEach((name) => { fog.u[name] = fgl.getUniformLocation(fogProgram, name); });
      return fog;
    } catch (error) {
      return null;
    }
  }

  // Text shapes need the page's fonts, so wait for them (but not for long).
  const fontsReady = document.fonts && document.fonts.ready ? Promise.race([document.fonts.ready, new Promise((r) => setTimeout(r, 1500))]) : Promise.resolve();
  fontsReady.then(() => {
    const def = scene || window.MURMURATION_SCENE(kit);
    if (def) start(def);
  });
})();
