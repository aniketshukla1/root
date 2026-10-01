// The title sequence's score, synthesised: nothing to load, nothing licensed.
//
// Quiet by design — a launch sound is heard every day, and the first thing it
// must not be is loud. A low swell under the whole sequence, soft chatter as
// footage arrives, a blip on each lock, air for the scan, and one deep hit
// when the name lands.

import { ACT, lockTimes, rng } from "./script";

const KEY = "root-splash-sound";

/** On unless someone has turned it off in Settings. */
export function readSplashSound(): boolean {
  try {
    return localStorage.getItem(KEY) !== "off";
  } catch {
    return true;
  }
}

export function writeSplashSound(on: boolean) {
  try {
    localStorage.setItem(KEY, on ? "on" : "off");
  } catch {
    // No storage: the setting lasts as long as this window.
  }
}

/**
 * Play the score from now. Returns a function that fades it out.
 *
 * A browser may refuse audio until someone has interacted with the page. When
 * it does, this stays silent rather than starting late: a score that begins
 * on the first click, out of step with a splash that has already gone, is
 * worse than none.
 */
export function playScore(): () => void {
  const Ctor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Ctor) return () => {};

  let ctx: AudioContext;
  try {
    ctx = new Ctor();
  } catch {
    return () => {};
  }
  void ctx.resume().catch(() => {});

  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    void ctx.close().catch(() => {});
  };
  const refused = window.setTimeout(() => {
    if (ctx.state !== "running") close();
  }, 160);
  // The score is over by here. An idle context still holds an audio device,
  // so it is let go rather than left for the splash to close.
  const over = window.setTimeout(close, 7200);

  const t0 = ctx.currentTime + 0.04;
  const at = (ms: number) => t0 + ms / 1000;

  const master = ctx.createGain();
  master.gain.value = 0.16;
  const glue = ctx.createDynamicsCompressor();
  master.connect(glue).connect(ctx.destination);

  const noise = ctx.createBuffer(1, ctx.sampleRate, ctx.sampleRate);
  const data = noise.getChannelData(0);
  for (let i = 0; i < data.length; i++) data[i] = Math.random() * 2 - 1;

  const envelope = (gain: GainNode, start: number, attack: number, peak: number, release: number) => {
    gain.gain.setValueAtTime(0.0001, start);
    gain.gain.exponentialRampToValueAtTime(peak, start + attack);
    gain.gain.exponentialRampToValueAtTime(0.0001, start + attack + release);
  };

  // The swell: three low voices through a filter that opens as the footage
  // builds and closes again for the name.
  const swell = ctx.createGain();
  swell.gain.setValueAtTime(0.0001, t0);
  swell.gain.linearRampToValueAtTime(0.5, at(2400));
  swell.gain.setValueAtTime(0.5, at(ACT.title - 150));
  swell.gain.linearRampToValueAtTime(0.0001, at(6300));
  const shade = ctx.createBiquadFilter();
  shade.type = "lowpass";
  shade.frequency.setValueAtTime(200, t0);
  shade.frequency.linearRampToValueAtTime(950, at(ACT.sweep));
  shade.frequency.linearRampToValueAtTime(280, at(6000));
  shade.connect(swell).connect(master);
  for (const [freq, type, level, detune] of [
    [55, "sine", 0.7, 0],
    [82.41, "triangle", 0.35, 6],
    [110, "sawtooth", 0.08, -4],
  ] as const) {
    const osc = ctx.createOscillator();
    const g = ctx.createGain();
    osc.type = type;
    osc.frequency.value = freq;
    osc.detune.value = detune;
    g.gain.value = level;
    osc.connect(g).connect(shade);
    osc.start(t0);
    osc.stop(at(6500));
  }

  // Chatter: short bursts of filtered noise while footage is arriving.
  const r = rng(11);
  for (let i = 0; i < 34; i++) {
    const start = at(ACT.feeds + r() * (ACT.sweep - ACT.feeds));
    const src = ctx.createBufferSource();
    src.buffer = noise;
    const band = ctx.createBiquadFilter();
    band.type = "bandpass";
    band.frequency.value = 2600 + r() * 4400;
    band.Q.value = 7;
    const g = ctx.createGain();
    envelope(g, start, 0.002, 0.05 + r() * 0.05, 0.03);
    src.connect(band).connect(g).connect(master);
    src.start(start, r() * 0.8, 0.06);
  }

  // A blip as each lock lands.
  for (const ms of lockTimes()) {
    const start = at(ms + 120);
    const osc = ctx.createOscillator();
    osc.type = "sine";
    osc.frequency.value = 1250 + r() * 400;
    const g = ctx.createGain();
    envelope(g, start, 0.004, 0.08, 0.06);
    osc.connect(g).connect(master);
    osc.start(start);
    osc.stop(start + 0.1);
  }

  // Air for the scan: noise swept upward.
  {
    const start = at(ACT.sweep);
    const src = ctx.createBufferSource();
    src.buffer = noise;
    src.loop = true;
    const band = ctx.createBiquadFilter();
    band.type = "bandpass";
    band.Q.value = 1.2;
    band.frequency.setValueAtTime(400, start);
    band.frequency.exponentialRampToValueAtTime(4200, start + 0.6);
    const g = ctx.createGain();
    envelope(g, start, 0.25, 0.14, 0.45);
    src.connect(band).connect(g).connect(master);
    src.start(start);
    src.stop(start + 0.8);
  }

  // The convergence: a tone drawn upward into the centre.
  {
    const start = at(ACT.converge);
    const osc = ctx.createOscillator();
    osc.type = "sine";
    osc.frequency.setValueAtTime(220, start);
    osc.frequency.exponentialRampToValueAtTime(660, start + 0.34);
    const g = ctx.createGain();
    envelope(g, start, 0.12, 0.06, 0.3);
    osc.connect(g).connect(master);
    osc.start(start);
    osc.stop(start + 0.5);
  }

  // The name: a falling sub, a click of noise, and a shimmer that hangs.
  {
    const start = at(ACT.title);
    const sub = ctx.createOscillator();
    sub.type = "sine";
    sub.frequency.setValueAtTime(72, start);
    sub.frequency.exponentialRampToValueAtTime(36, start + 0.5);
    const sg = ctx.createGain();
    envelope(sg, start, 0.008, 0.9, 1.3);
    sub.connect(sg).connect(master);
    sub.start(start);
    sub.stop(start + 1.5);

    const click = ctx.createBufferSource();
    click.buffer = noise;
    const cg = ctx.createGain();
    envelope(cg, start, 0.001, 0.25, 0.03);
    click.connect(cg).connect(master);
    click.start(start, 0, 0.05);

    for (const freq of [1318.5, 1975.5]) {
      const osc = ctx.createOscillator();
      osc.type = "triangle";
      osc.frequency.value = freq;
      const g = ctx.createGain();
      envelope(g, start + 0.02, 0.01, 0.045, 1.5);
      osc.connect(g).connect(master);
      osc.start(start);
      osc.stop(start + 1.7);
    }
  }

  return () => {
    window.clearTimeout(refused);
    window.clearTimeout(over);
    if (closed) return;
    try {
      master.gain.setTargetAtTime(0.0001, ctx.currentTime, 0.08);
    } catch {
      // Already closing.
    }
    window.setTimeout(close, 400);
  };
}
