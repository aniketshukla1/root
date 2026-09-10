import { CheckIcon } from "../ui/icons";

/**
 * The boot screen.
 *
 * Two rules, and they are the same rule twice. The mark draws itself rather
 * than spinning: one root, then the branches reaching out to what they find —
 * the product's own gesture, and it takes exactly as long as it takes.
 *
 * And the steps are named, not a bar. A progress bar on a boot screen is
 * almost always a lie: it animates on a timer while the thing it claims to
 * measure has not started. Every line here is a fact the app already knows —
 * the workspace is loaded or it is not, and the collector count is how many
 * connections have actually reported against how many are configured. If a
 * collector is slow, the number stops moving, which is the truth.
 */
export function Splash({
  workspaceLoaded,
  catalogLoaded,
  reached,
  total,
  workspace,
}: {
  workspaceLoaded: boolean;
  catalogLoaded: boolean;
  /** Connections that have returned a snapshot. */
  reached: number;
  /** Connections configured. Zero means a first run, not a stall. */
  total: number;
  workspace?: string;
}) {
  const collectorsDone = total > 0 && reached >= total;

  return (
    <div className="relative flex h-screen w-screen flex-col overflow-hidden bg-page">
      {/* The ribbon is the first thing to arrive and the last thing to leave. */}
      <div className="flex h-[3px] shrink-0 gap-px" aria-hidden>
        <span className="a-reach grow bg-crit" style={{ animationDelay: "120ms" }} />
        <span className="a-reach grow-[3] bg-warn" style={{ animationDelay: "180ms" }} />
        <span className="a-reach grow bg-unknown" style={{ animationDelay: "240ms" }} />
        <span className="a-reach grow-[7] bg-ok" style={{ animationDelay: "300ms" }} />
      </div>

      <div
        className="pointer-events-none absolute inset-x-0 top-0 h-[560px]"
        style={{
          background:
            "radial-gradient(110% 90% at 50% -12%, color-mix(in srgb, var(--color-accent) 9%, transparent), transparent 68%)",
        }}
        aria-hidden
      />

      <div className="relative z-10 flex min-h-0 flex-1 flex-col items-center justify-center px-6">
        <div className="grid h-28 w-28 place-items-center rounded-md bg-accent text-on-accent">
          <DrawingMark />
        </div>

        <div className="a-wash mt-7" style={{ animationDelay: "900ms" }}>
          <span className="text-[80px] leading-[0.84] font-bold tracking-[-.05em] text-ink">
            root
          </span>
        </div>

        <p
          className="a-wash mt-5 font-mono text-xs tracking-[.2em] text-ink-3 uppercase"
          style={{ animationDelay: "1100ms" }}
        >
          Infrastructure intelligence
        </p>

        <div
          className="a-wash mt-12 flex w-[320px] flex-col gap-2.5"
          style={{ animationDelay: "1400ms" }}
        >
          {/* Each label is in the tense of its own state. "Workspace loaded"
              beside a pending dot is a small lie, and this screen has exactly
              three lines on which to establish that it does not tell them. */}
          <Step
            done={workspaceLoaded}
            label={
              workspaceLoaded
                ? "Workspace and thresholds loaded"
                : "Loading workspace and thresholds"
            }
          />
          <Step
            done={catalogLoaded}
            label={catalogLoaded ? "Service catalog loaded" : "Loading service catalog"}
          />

          {total === 0 ? (
            <Step
              done={catalogLoaded && workspaceLoaded}
              label={
                catalogLoaded && workspaceLoaded
                  ? "No connections configured yet"
                  : "Looking for connections"
              }
            />
          ) : (
            <>
              <Step
                done={collectorsDone}
                label={`Reaching ${total} collector${total === 1 ? "" : "s"}`}
                trailing={`${Math.min(reached, total)} / ${total}`}
              />
              {/* A real measure: filled by connections that answered. It stops
                  when they stop, which is the point. */}
              <div className="mt-1 flex h-1 gap-0.5" aria-hidden>
                <span
                  className="rounded-[1px] bg-ok transition-[flex-grow] duration-500"
                  style={{ flexGrow: Math.max(0.001, Math.min(reached, total)) }}
                />
                <span
                  className="rounded-[1px] bg-sunk transition-[flex-grow] duration-500"
                  style={{ flexGrow: Math.max(0.001, total - Math.min(reached, total)) }}
                />
              </div>
            </>
          )}
        </div>
      </div>

      <div className="relative z-10 flex shrink-0 items-center px-6 pb-5">
        <span className="num text-2xs text-ink-4">root {__APP_VERSION__}</span>
        {workspace && <span className="num ml-auto text-2xs text-ink-4">{workspace}</span>}
      </div>
    </div>
  );
}

function Step({
  done,
  label,
  trailing,
}: {
  done: boolean;
  label: string;
  trailing?: string;
}) {
  return (
    <div className="flex items-center gap-2.5">
      <span className="grid h-3.5 w-3.5 shrink-0 place-items-center">
        {done ? (
          <CheckIcon size={13} style={{ color: "var(--color-ok)" }} />
        ) : (
          // Not a spinner. A pending step is a dot that has not turned yet, and
          // it stays still if the thing it waits on stays still.
          <span className="h-[7px] w-[7px] rounded-full bg-accent" />
        )}
      </span>
      <span className={`num flex-1 text-xs ${done ? "text-ink-3" : "text-ink-2"}`}>{label}</span>
      {trailing && <span className="num text-xs text-ink-4">{trailing}</span>}
    </div>
  );
}

/**
 * The mark, drawn in the order it means: the root first, then each branch
 * reaching out, then the thing it found at the end of it.
 *
 * Plain CSS rather than Motion. It runs once, on the first paint of the app,
 * with nothing to orchestrate against and nothing that could re-render it
 * mid-gesture — which is exactly the case app.css keeps these classes for.
 */
function DrawingMark() {
  return (
    <svg
      viewBox="0 0 24 24"
      width={68}
      height={68}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-label="root"
    >
      <circle className="a-pop" cx="5.6" cy="3.7" r="2" fill="currentColor" stroke="none" />
      <path className="a-draw" style={{ "--len": 14, animationDelay: "180ms" } as never} d="M5.6 5.7V19.2" />
      <path className="a-draw" style={{ "--len": 10, animationDelay: "520ms" } as never} d="M5.6 9.7h9.3" />
      <path className="a-draw" style={{ "--len": 10, animationDelay: "640ms" } as never} d="M5.6 14.45h9.3" />
      <path className="a-draw" style={{ "--len": 10, animationDelay: "760ms" } as never} d="M5.6 19.2h9.3" />
      <circle className="a-pop" style={{ animationDelay: "840ms" }} cx="17.4" cy="9.7" r="1.7" strokeWidth={1.35} />
      <circle className="a-pop" style={{ animationDelay: "940ms" }} cx="17.4" cy="14.45" r="1.7" strokeWidth={1.35} />
      <circle className="a-pop" style={{ animationDelay: "1040ms" }} cx="17.4" cy="19.2" r="1.7" strokeWidth={1.35} />
    </svg>
  );
}
