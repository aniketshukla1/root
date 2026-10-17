// The layers that give a metric's chart its meaning, built once for every
// screen that draws one: the normal range under the line, the readings
// outside it and where the trend is heading over it, the window stretched to
// show where that leads, and a mark where it reaches the limit.

import { DATA_INK } from "../catalog";
import { formatDateTime } from "../format";
import { forecastCone, normalRange } from "../meaning";
import type { MetricSeries } from "../types";
import type { Annotation, Domain, Series } from "./charts";

export interface Layers {
  /** Drawn before the series itself. */
  under: Series[];
  /** Drawn after it. */
  over: Series[];
  bands: [string, string][];
  /** The window, stretched forward when a forecast needs the room. */
  domain: Domain;
  /** Where the trend reaches the limit, when that falls inside the window. */
  limit?: Annotation;
}

/** `live`: the window ends now. A forecast drawn from a past stretch says nothing. */
export function layersFor(s: MetricSeries, view: Domain, live: boolean): Layers {
  const under: Series[] = [];
  const over: Series[] = [];
  const bands: [string, string][] = [];

  const normal = normalRange(s.points);
  if (normal.upper.length > 1) {
    under.push(
      { key: `${s.key}~hi`, label: "Normal, high", color: DATA_INK, points: normal.upper, ghost: true },
      { key: `${s.key}~lo`, label: "Normal, low", color: DATA_INK, points: normal.lower, ghost: true },
    );
    bands.push([`${s.key}~hi`, `${s.key}~lo`]);
  }
  if (normal.anomalies.length > 0) {
    over.push({ key: `${s.key}~odd`, label: "Unusual", color: "var(--color-crit)", points: normal.anomalies, dots: true });
  }

  let domain = view;
  let limit: Annotation | undefined;
  const cone = live ? forecastCone(s.points, s.trend, s.warnAbove, (view[1] - view[0]) / 2) : null;
  if (cone) {
    over.push(
      { key: `${s.key}~next`, label: "Heading", color: DATA_INK, points: cone.center, dashed: true },
      { key: `${s.key}~next-hi`, label: "Heading, high", color: "var(--color-warn)", points: cone.upper, ghost: true },
      { key: `${s.key}~next-lo`, label: "Heading, low", color: "var(--color-warn)", points: cone.lower, ghost: true },
    );
    bands.push([`${s.key}~next-hi`, `${s.key}~next-lo`]);
    const end = Date.parse(cone.center[cone.center.length - 1].t);
    domain = [view[0], Math.max(view[1], end)];
    if (cone.hitsAt <= end) {
      limit = {
        id: `limit:${s.key}`,
        t: cone.hitsAt,
        color: "var(--color-warn)",
        label: "Reaches its limit",
        detail: `At this rate, ${formatDateTime(new Date(cone.hitsAt).toISOString())}`,
      };
    }
  }
  return { under, over, bands, domain, limit };
}
