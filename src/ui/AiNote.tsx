import { useEffect } from "react";

import type { ComponentReading } from "../types";
import { CloseIcon, SparkleIcon } from "./icons";
import { Button } from "./primitives";

/** Where an inline answer is: not asked, being worked out, answered, or refused. */
export type AiState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "done"; reading: ComponentReading }
  | { status: "error"; message: string };

/**
 * The model's answer, written into the page next to the thing it was asked
 * about — ferro's inline note rather than a trip to a chat screen.
 *
 * It always carries its evidence. A sentence from a model about why
 * something broke is only worth acting on if the facts behind it can be
 * checked, so they are one click away under the answer, exactly as the model
 * was given them.
 *
 * A missing provider is not an error to apologise for; it is a setup step,
 * and the note says where it is.
 */
export function AiNote({
  title,
  state,
  onClose,
  onRetry,
  onSetup,
}: {
  title: string;
  state: AiState;
  onClose: () => void;
  onRetry?: () => void;
  onSetup?: () => void;
}) {
  useEffect(() => {
    if (state.status === "idle") return;
    const close = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [state.status, onClose]);

  if (state.status === "idle") return null;

  const needsProvider =
    state.status === "error" && /api key|provider|not configured|settings/i.test(state.message);

  return (
    <section
      className="a-wash overflow-hidden rounded-md border border-line-strong bg-raised"
      aria-label={title}
      aria-busy={state.status === "loading"}
    >
      <header className="flex h-8 items-center gap-2 border-b border-line px-3">
        <SparkleIcon size={13} className="shrink-0 text-ink" />
        <h3 className="min-w-0 truncate text-xs font-medium text-ink">{title}</h3>
        <span className="ml-auto flex shrink-0 items-center gap-2">
          {state.status === "done" && (
            <span className="text-2xs text-ink-4" title={`Answered by ${state.reading.model}`}>
              {state.reading.model}
            </span>
          )}
          <button
            onClick={onClose}
            title="Close (Esc)"
            aria-label="Close"
            className="grid h-6 w-6 place-items-center rounded-sm text-ink-4 hover:bg-wash hover:text-ink"
          >
            <CloseIcon size={12} />
          </button>
        </span>
      </header>

      <div className="px-3 py-2.5" aria-live="polite">
        {state.status === "loading" && (
          <div className="flex flex-col gap-2">
            <span className="text-xs text-ink-3">Reading the evidence…</span>
            <span className="ai-shimmer h-2 w-[86%] rounded-xs" />
            <span className="ai-shimmer h-2 w-[64%] rounded-xs" />
          </div>
        )}

        {state.status === "done" && (
          <>
            <p className="text-[13px] leading-relaxed text-ink">{state.reading.text}</p>
            <details className="group mt-2">
              <summary className="cursor-pointer list-none text-2xs text-ink-4 select-none hover:text-ink-2">
                <span className="group-open:hidden">Based on {state.reading.grounds.length} facts — show</span>
                <span className="hidden group-open:inline">Hide the evidence</span>
              </summary>
              <ul className="mt-1.5 flex flex-col gap-0.5 border-l border-line pl-2.5">
                {state.reading.grounds.map((fact, i) => (
                  <li key={i} className="font-mono text-[11px] leading-[16px] text-ink-3">
                    {fact}
                  </li>
                ))}
              </ul>
            </details>
          </>
        )}

        {state.status === "error" && (
          <div className="flex flex-wrap items-center gap-2">
            <p className="min-w-0 flex-1 text-xs text-ink-2">
              {needsProvider
                ? "AI needs a provider. Add a key in Settings → Assistant, or point it at a local model."
                : state.message}
            </p>
            {needsProvider && onSetup ? (
              <Button size="sm" onClick={onSetup}>
                Open Settings
              </Button>
            ) : (
              onRetry && (
                <Button size="sm" variant="ghost" onClick={onRetry}>
                  Try again
                </Button>
              )
            )}
          </div>
        )}
      </div>
    </section>
  );
}
