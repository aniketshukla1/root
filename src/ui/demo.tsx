import { useCallback, useEffect, useRef, useState } from "react";

import { api, errorMessage } from "../api";
import type { AppConfig, DemoStatus } from "../types";
import { useTick } from "./charts";
import { Button, cx } from "./primitives";
import { ChevronDown } from "./icons";

/**
 * The demo's own colour: not a status hue, so a banner that is always on
 * screen never reads as an alarm, and not the accent, so it is never
 * mistaken for a control.
 */
export const DEMO_HUE = "var(--color-series-6)";

export const demoRunning = (config: AppConfig | null | undefined) => Boolean(config?.demo?.startedAt);

/** Whether a connection belongs to the live demo rather than to anything real. */
export const isSimulated = (config: AppConfig | null | undefined, profileId: string) =>
  Boolean(config?.profiles.find((p) => p.id === profileId)?.simulated);

/**
 * What the demo is doing, polled while it runs.
 *
 * Five seconds, because a phase is a minute or two long and the countdown is
 * read off the clock between polls rather than off the poll itself.
 */
export function useDemoStatus(config: AppConfig | null) {
  const running = demoRunning(config);
  const [status, setStatus] = useState<DemoStatus | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.getDemoStatus());
    } catch (error) {
      console.error("demo status unavailable:", errorMessage(error));
    }
  }, []);

  useEffect(() => {
    if (!running) {
      setStatus(null);
      return;
    }
    void refresh();
    const timer = setInterval(refresh, 5000);
    return () => clearInterval(timer);
  }, [running, refresh]);

  return { status, refresh };
}

function countdown(iso: string): string {
  const secs = Math.max(0, Math.round((Date.parse(iso) - Date.now()) / 1000));
  const m = Math.floor(secs / 60);
  const s = String(secs % 60).padStart(2, "0");
  return `${m}:${s}`;
}

/**
 * The band across the top of every screen while the demo runs.
 *
 * Two jobs. Honesty first: nothing on screen is real, and the band says so on
 * every screen rather than once at the start, because a screenshot of an
 * incident travels without its context. Then the presenter's controls — what
 * is playing, what comes next, and "break something" when the audience is
 * ready rather than when the clock says.
 */
export function DemoBar({
  config,
  onConfigChange,
}: {
  config: AppConfig | null;
  onConfigChange: (config: AppConfig) => void;
}) {
  useTick(1000);
  const { status, refresh } = useDemoStatus(config);
  const [menu, setMenu] = useState(false);
  const [busy, setBusy] = useState<"trigger" | "stop" | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  // A menu that stays open after the pointer has gone elsewhere is a menu
  // that eats the next click.
  useEffect(() => {
    if (!menu) return;
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (event instanceof KeyboardEvent && event.key !== "Escape") return;
      if (event instanceof MouseEvent && menuRef.current?.contains(event.target as Node)) return;
      setMenu(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", close);
    };
  }, [menu]);

  useEffect(() => {
    if (!note) return;
    const timer = setTimeout(() => setNote(null), 6000);
    return () => clearTimeout(timer);
  }, [note]);

  if (!demoRunning(config)) return null;

  const playing = status?.playing;
  const simulated = config?.profiles.filter((p) => p.simulated).length ?? 0;

  const trigger = async (story?: string) => {
    setMenu(false);
    setBusy("trigger");
    try {
      const next = await api.triggerDemoStory(story);
      // Started now if what is playing is a hand-started story that began in
      // the last few seconds; otherwise it waits for the one playing.
      const started =
        next.playing?.manual && Date.now() - Date.parse(next.playing.startedAt) < 10_000;
      setNote(
        started && next.playing
          ? `${next.playing.title} — starting now`
          : `${next.next?.title ?? "Next story"} — starts when this one ends`,
      );
      await refresh();
    } catch (error) {
      setNote(errorMessage(error));
    } finally {
      setBusy(null);
    }
  };

  const stop = async () => {
    const paused = config?.demo?.paused?.length ?? 0;
    const restore = paused
      ? ` Your ${paused} paused connection${paused === 1 ? " is" : "s are"} switched back on.`
      : "";
    if (!confirm(`End the live demo? Its ${simulated} simulated connections and their history are removed.${restore}`))
      return;
    setBusy("stop");
    try {
      onConfigChange(await api.stopDemo());
    } catch (error) {
      setNote(errorMessage(error));
      setBusy(null);
    }
  };

  return (
    <div
      className="hue relative z-20 flex min-h-[34px] shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b px-4 py-1.5 text-xs"
      style={
        {
          "--hue": DEMO_HUE,
          borderColor: `color-mix(in oklab, ${DEMO_HUE} 30%, var(--color-line))`,
          background: `color-mix(in oklab, ${DEMO_HUE} 7%, var(--color-page))`,
        } as React.CSSProperties
      }
    >
      <span className="flex items-center gap-2 font-mono text-2xs font-semibold tracking-[.08em]" style={{ color: DEMO_HUE }}>
        <span className="relative flex h-1.5 w-1.5" aria-hidden>
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full opacity-60" style={{ background: DEMO_HUE }} />
          <span className="relative inline-flex h-1.5 w-1.5 rounded-full" style={{ background: DEMO_HUE }} />
        </span>
        LIVE DEMO
      </span>
      <span className="text-ink-3">
        Acme Shop · {simulated} simulated services. Nothing here is real, and nobody is paged.
      </span>

      <span className="ml-auto flex min-w-0 items-center gap-2">
        {playing ? (
          <span className="flex min-w-0 items-center gap-1.5 text-ink-2" title={`Ends at ${new Date(playing.endsAt).toLocaleTimeString()}`}>
            <span className="font-medium text-ink">{playing.title}</span>
            <span className="text-ink-4">·</span>
            <span className="truncate">{playing.phase}</span>
            <span className="num text-ink-4">{countdown(playing.endsAt)}</span>
          </span>
        ) : status?.next ? (
          <span className="flex min-w-0 items-center gap-1.5 text-ink-3">
            Calm
            <span className="text-ink-4">·</span>
            <span className="truncate">
              next: <span className="text-ink-2">{status.next.title}</span> in{" "}
              <span className="num">{countdown(status.next.at)}</span>
            </span>
          </span>
        ) : null}

        {note && <span className="truncate text-ink-3">{note}</span>}

        <div ref={menuRef} className="relative">
          <Button
            size="sm"
            onClick={() => setMenu((open) => !open)}
            loading={busy === "trigger"}
            aria-haspopup="menu"
            aria-expanded={menu}
            title="Start an incident now — or straight after the one playing"
          >
            Break something
            <ChevronDown size={11} />
          </Button>
          {menu && (
            <div
              role="menu"
              className="absolute right-0 top-[calc(100%+4px)] z-30 w-60 overflow-hidden rounded-md border border-line-strong bg-overlay py-1 shadow-xl"
            >
              <p className="label px-3 pt-1.5 pb-1">
                {playing ? "Queue after this one" : "Start now"}
              </p>
              {(status?.stories ?? []).map((story) => (
                <button
                  key={story.id}
                  role="menuitem"
                  type="button"
                  onClick={() => trigger(story.id)}
                  className="block w-full px-3 py-1.5 text-left text-xs text-ink-2 hover:bg-wash hover:text-ink"
                >
                  {story.title}
                </button>
              ))}
            </div>
          )}
        </div>
        <Button size="sm" variant="ghost" onClick={stop} loading={busy === "stop"}>
          End demo
        </Button>
      </span>
    </div>
  );
}

/**
 * The way into the demo, for a first run and for Settings.
 *
 * Offers to pause real connections only when there are some, and defaults to
 * pausing them: an incident on a simulated estate shown beside eight real
 * connections that are down because Docker is not running teaches the wrong
 * lesson about both.
 */
export function DemoLauncher({
  config,
  onConfigChange,
  compact = false,
}: {
  config: AppConfig | null;
  onConfigChange: (config: AppConfig) => void;
  compact?: boolean;
}) {
  const real = config?.profiles.filter((p) => !p.simulated && p.enabled).length ?? 0;
  const [pause, setPause] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      onConfigChange(await api.startDemo(real > 0 && pause));
    } catch (e) {
      setError(errorMessage(e));
      setBusy(false);
    }
  };

  return (
    <section
      className={cx("hue panel relative flex flex-col gap-3 overflow-hidden", compact ? "p-3.5" : "p-5")}
      style={{ "--hue": DEMO_HUE } as React.CSSProperties}
    >
      <div className="flex flex-wrap items-start gap-x-6 gap-y-3">
        <div className="min-w-[260px] flex-1">
          <p className="label mb-1.5" style={{ color: DEMO_HUE }}>
            Live demo
          </p>
          <h2 className={cx("font-medium tracking-[-.02em] text-ink", compact ? "text-lg" : "text-2xl")}>
            Watch root work on a shop that breaks on schedule.
          </h2>
          <p className="mt-1.5 max-w-[68ch] text-sm leading-relaxed text-ink-3">
            Thirteen simulated services — a load balancer, containers, Postgres, Redis, Kafka,
            Kubernetes, CI — with two hours of history already behind them. Every twelve minutes
            something goes wrong: a bad deploy, a consumer in a crash loop, a search node in GC.
            Nothing real is touched, and no alert leaves the app.
          </p>
        </div>
        <div className="flex shrink-0 flex-col items-start gap-2">
          <Button variant="primary" onClick={start} loading={busy}>
            Start live demo
          </Button>
          {real > 0 && (
            <label className="flex cursor-pointer items-center gap-1.5 text-xs text-ink-3">
              <input
                type="checkbox"
                checked={pause}
                onChange={(e) => setPause(e.target.checked)}
                className="accent-accent"
              />
              Pause my {real} connection{real === 1 ? "" : "s"} while it runs
            </label>
          )}
        </div>
      </div>
      {error && <p className="text-xs text-crit">{error}</p>}
    </section>
  );
}
