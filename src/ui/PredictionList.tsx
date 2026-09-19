// Predictions, rendered as what they are: things that have not happened.
//
// Deliberately not an `AlertList`. An alert is a statement about the present
// and a prediction is a statement about the future, and the moment the two
// render the same way the operator is back to doing the sorting themselves —
// at the hour they are least able to. So this is quieter, carries a horizon
// rather than a severity, and every screen that shows it frames it in words
// that say nothing here is firing.
//
// It lives beside `AlertList` rather than inside a screen because two screens
// show it, and a second copy would drift the first time either changed.

import type { Catalog } from "../catalog";
import type { View } from "../nav";
import type { Alert } from "../types";
import { Button, ServiceMark } from "./primitives";

/**
 * How long there is to act, in the words the prediction engine reasons in.
 *
 * The engine emits `now` / `soon` / `advisory` and those arrive here as alert
 * severities. Rendering them back as horizons rather than as severities is the
 * whole point: a forecast has a deadline, not an urgency.
 */
const HORIZON: Record<
  Alert["severity"],
  { label: string; color: string; hint: string }
> = {
  critical: {
    label: "hours",
    color: "var(--color-crit)",
    hint: "Expected within hours. Acting after this lands is acting during an outage.",
  },
  warning: {
    label: "soon",
    color: "var(--color-warn)",
    hint: "Expected, without a confident hour. Worth planning around rather than reacting to.",
  },
  info: {
    label: "watch",
    color: "var(--color-ink-4)",
    hint: "A direction, not a date. Here so it is not a surprise later.",
  },
};

export function PredictionList({
  predictions,
  catalog,
  limit,
  onNavigate,
  onExplain,
}: {
  predictions: Alert[];
  catalog: Catalog;
  /** Omit to show every one. */
  limit?: number;
  onNavigate: (view: View) => void;
  onExplain: (alert: Alert) => void;
}) {
  const shown = limit === undefined ? predictions : predictions.slice(0, limit);
  return (
    <div className="flex flex-col">
      {shown.map((prediction) => {
        const descriptor = catalog.get(prediction.kind);
        const horizon = HORIZON[prediction.severity];
        return (
          <div
            key={prediction.id}
            className="flex items-start gap-2.5 border-b border-line px-3 py-2.5 last:border-b-0"
          >
            {/* The horizon, not the severity. "Critical" on a thing that has
                not happened is the conflation this panel exists to end. */}
            <span
              className="num mt-0.5 w-12 shrink-0 text-2xs tracking-[.08em] uppercase"
              style={{ color: horizon.color }}
              title={horizon.hint}
            >
              {horizon.label}
            </span>

            <div className="min-w-0 flex-1">
              <button
                type="button"
                onClick={() =>
                  onNavigate({ name: "service", profileId: prediction.profileId })
                }
                className="flex min-w-0 items-center gap-1.5 text-left"
                title={`Open ${prediction.profileName}`}
              >
                <ServiceMark descriptor={descriptor} size={14} />
                <span className="num truncate text-2xs text-ink-3">
                  {prediction.profileName}
                </span>
              </button>
              <p className="mt-1 text-xs leading-snug text-ink">{prediction.title}</p>
              <p className="num mt-1 text-2xs leading-relaxed text-ink-4">
                {prediction.detail}
              </p>
            </div>

            <Button variant="ghost" onClick={() => onExplain(prediction)}>
              Explain
            </Button>
          </div>
        );
      })}
    </div>
  );
}
