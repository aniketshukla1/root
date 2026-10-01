// A stage for the splash: the real component, fed an estate that answers the
// way a real one does — over a couple of seconds, not all at once, with the
// odd slow collector — then leaving into a stand-in for the app so the handoff
// can be seen too.
//
// The timing mirrors App's (`SPLASH_MIN_MS`, `SPLASH_MAX_MS`, `SPLASH_BEAT_MS`).
// Keep the two in step.

import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactDOM from "react-dom/client";
import { AnimatePresence, MotionConfig } from "motion/react";

import "@fontsource-variable/ibm-plex-sans";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "@fontsource/ibm-plex-mono/600.css";
import "../app.css";

import { Splash } from "../shell/Splash";
import { ALL_FEATURES, SEQUENCE_MS, type SplashFeatures } from "../shell/splash/script";
import type { Health, HealthSnapshot } from "../types";
import { RootMark } from "../ui/primitives";

const MIN_MS = SEQUENCE_MS;
const MAX_MS = 8000;
const BEAT_MS = 350;

const NAMES = [
  "orders-api", "orders-db", "checkout-web", "payments", "session-cache", "search",
  "ledger", "kafka-main", "edge-lb", "auth", "billing-worker", "notifications",
  "inventory", "pricing", "media-cdn", "reports", "scheduler", "gateway",
];

interface Planned {
  id: string;
  name: string;
  health: Health;
  at: number;
  latencyMs: number;
}

/** A deterministic estate, so a replay is the same estate answering again. */
function plan(size: number, slow: boolean, seed: number): Planned[] {
  let s = seed >>> 0;
  const rand = () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32);
  return Array.from({ length: size }, (_, i) => {
    // Mostly healthy, with the few problems a real estate has — placed, so
    // every size shows them rather than a seed that happens to roll none.
    const r = rand();
    const health: Health =
      i === 3 ? "degraded" : i === 8 ? "down" : i === 15 ? "unknown"
      : r < 0.04 ? "down" : r < 0.1 ? "degraded" : "healthy";
    const round = Math.floor(i / NAMES.length);
    return {
      id: `c${i}`,
      name: NAMES[i % NAMES.length] + (round ? `-${round + 1}` : ""),
      health,
      at: slow && i === size - 1 ? 3900 : 240 + rand() * 1500,
      latencyMs: 12 + rand() * 380,
    };
  });
}

function snapshotOf(p: Planned): HealthSnapshot {
  return {
    profileId: p.id,
    profileName: p.name,
    kind: "docker",
    health: p.health,
    summary: "",
    metrics: [],
    sections: [],
    checkedAt: new Date().toISOString(),
    latencyMs: p.latencyMs,
  };
}

function Stage() {
  const params = new URLSearchParams(window.location.search);
  const [size, setSize] = useState(Number(params.get("n") ?? 12));
  const [slow, setSlow] = useState(params.get("slow") === "1");
  const [hold, setHold] = useState(params.get("hold") === "1");
  const [light, setLight] = useState(params.get("theme") === "light");
  const [run, setRun] = useState(0);
  // Every upgrade on unless the URL says otherwise: ?off=camera,sound
  const [features, setFeatures] = useState<SplashFeatures>(() => {
    const off = new Set((params.get("off") ?? "").split(",").filter(Boolean));
    return Object.fromEntries(
      Object.keys(ALL_FEATURES).map((k) => [k, !off.has(k)]),
    ) as unknown as SplashFeatures;
  });

  const [booting, setBooting] = useState(true);
  const [handingOff, setHandingOff] = useState(true);
  const [workspaceLoaded, setWorkspaceLoaded] = useState(false);
  const [catalogLoaded, setCatalogLoaded] = useState(false);
  const [snapshots, setSnapshots] = useState<HealthSnapshot[]>([]);
  const started = useRef(performance.now());

  const planned = useMemo(() => plan(size, slow, 7 + run), [size, slow, run]);
  const connections = useMemo(() => planned.map(({ id, name }) => ({ id, name })), [planned]);

  useEffect(() => {
    document.documentElement.dataset.theme = light ? "light" : "dark";
    document.documentElement.style.background = light ? "#f7f3ec" : "#0a0e14";
  }, [light]);

  // Reset in the same event that starts a run. Resetting in the run's own
  // effect meant the new splash mounted one render early with the old run's
  // answers still in hand and stamped all of them at once.
  const restart = useCallback(() => {
    setBooting(true);
    setHandingOff(true);
    setWorkspaceLoaded(false);
    setCatalogLoaded(false);
    setSnapshots([]);
    setRun((r) => r + 1);
  }, []);

  // Each run: the workspace, the catalog, then the estate answering on its own schedule.
  useEffect(() => {
    started.current = performance.now();
    const timers = [
      window.setTimeout(() => setWorkspaceLoaded(true), 140),
      window.setTimeout(() => setCatalogLoaded(true), 300),
      ...planned.map((p) =>
        window.setTimeout(() => setSnapshots((prev) => [...prev, snapshotOf(p)]), p.at),
      ),
    ];
    return () => timers.forEach((t) => window.clearTimeout(t));
  }, [planned]);

  const ready = workspaceLoaded && catalogLoaded && snapshots.length >= size;

  // The same clock as App.
  useEffect(() => {
    if (!booting || hold) return;
    const elapsed = performance.now() - started.current;
    const ceiling = window.setTimeout(() => setBooting(false), Math.max(0, MAX_MS - elapsed));
    const floor = ready
      ? window.setTimeout(() => setBooting(false), Math.max(MIN_MS - elapsed, BEAT_MS))
      : undefined;
    return () => {
      window.clearTimeout(ceiling);
      if (floor !== undefined) window.clearTimeout(floor);
    };
  }, [booting, ready, hold]);

  const skip = useCallback(() => {
    if (ready && !hold) setBooting(false);
  }, [ready, hold]);

  return (
    <>
      <FakeApp markHidden={handingOff} />

      <MotionConfig reducedMotion="user">
        <AnimatePresence onExitComplete={() => setHandingOff(false)}>
          {booting && (
            <Splash
              key={`splash-${run}`}
              workspaceLoaded={workspaceLoaded}
              catalogLoaded={catalogLoaded}
              connections={connections}
              snapshots={snapshots}
              workspace="acme · production"
              ready={ready}
              onSkip={skip}
              features={features}
            />
          )}
        </AnimatePresence>
      </MotionConfig>

      <Controls
        size={size}
        slow={slow}
        hold={hold}
        light={light}
        onSize={(n) => { setSize(n); restart(); }}
        onSlow={() => { setSlow((v) => !v); restart(); }}
        onHold={() => setHold((v) => !v)}
        onLight={() => setLight((v) => !v)}
        onReplay={restart}
        features={features}
        onFeature={(k) => {
          setFeatures((f) => ({ ...f, [k]: !f[k] }));
          restart();
        }}
      />
    </>
  );
}

/** A stand-in for the shell: enough sidebar for the mark to land in its real place. */
function FakeApp({ markHidden }: { markHidden: boolean }) {
  return (
    <div className="flex h-screen w-screen bg-page">
      <aside className="flex w-[216px] shrink-0 flex-col border-r border-line bg-panel p-2">
        <div className="mb-2 flex h-8 items-center gap-2 px-1">
          <span
            data-splash-target
            className="grid h-[26px] w-[26px] place-items-center rounded-sm bg-accent text-on-accent transition-opacity duration-150"
            style={{ opacity: markHidden ? 0 : 1 }}
          >
            <RootMark size={16} />
          </span>
          <span className="font-mono text-xs tracking-[.18em] text-ink uppercase">root</span>
        </div>
        {["Overview", "Services", "Alerts", "Metrics", "Logs", "Lanes"].map((label) => (
          <div key={label} className="rounded-sm px-2 py-1.5 text-xs text-ink-3">
            {label}
          </div>
        ))}
      </aside>
      <main className="flex-1 p-4">
        <div className="h-full rounded-md border border-line bg-panel" />
      </main>
    </div>
  );
}

function Controls(props: {
  size: number;
  slow: boolean;
  hold: boolean;
  light: boolean;
  onSize: (n: number) => void;
  onSlow: () => void;
  onHold: () => void;
  onLight: () => void;
  onReplay: () => void;
  features: SplashFeatures;
  onFeature: (k: keyof SplashFeatures) => void;
}) {
  const chip = (on: boolean) =>
    `rounded-sm border px-2 py-1 font-mono text-2xs tracking-[.12em] uppercase ${
      on ? "border-accent text-accent" : "border-line text-ink-3 hover:text-ink"
    }`;
  return (
    <div
      className="fixed right-3 bottom-3 z-[400] flex items-center gap-1.5 rounded-md border border-line bg-panel/95 p-1.5"
      onPointerDown={(e) => e.stopPropagation()}
      onKeyDown={(e) => e.stopPropagation()}
    >
      <button className={chip(false)} onClick={props.onReplay}>Replay</button>
      <span className="mx-1 h-4 w-px bg-line" />
      {[0, 7, 12, 40, 120].map((n) => (
        <button key={n} className={chip(props.size === n)} onClick={() => props.onSize(n)}>
          {n}
        </button>
      ))}
      <span className="mx-1 h-4 w-px bg-line" />
      {(Object.keys(props.features) as (keyof SplashFeatures)[]).map((k) => (
        <button key={k} className={chip(props.features[k])} onClick={() => props.onFeature(k)}>
          {k}
        </button>
      ))}
      <span className="mx-1 h-4 w-px bg-line" />
      <button className={chip(props.slow)} onClick={props.onSlow}>Slow one</button>
      <button className={chip(props.hold)} onClick={props.onHold}>Hold</button>
      <button className={chip(props.light)} onClick={props.onLight}>Light</button>
    </div>
  );
}

// The stage lives in its entry module, so editing it re-runs this file. Reuse
// the root rather than mounting a second one on the same container.
const container = document.getElementById("root") as HTMLElement & { __root?: ReactDOM.Root };
container.__root ??= ReactDOM.createRoot(container);
container.__root.render(
  <React.StrictMode>
    <Stage />
  </React.StrictMode>,
);
