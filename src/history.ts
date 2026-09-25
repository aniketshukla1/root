// Whether this backend keeps history, asked once.
//
// "Is a database configured" is a property of the server, not of any
// component's parent, and it does not change while the app is open. Threading
// it down through every screen that needs it would mean adding a prop to
// components that have nothing else to do with history, so it is cached at
// module scope and read by a hook instead.
//
// The promise is cached rather than the value, so ten components mounting at
// once make one request between them rather than ten.

import { useEffect, useState } from "react";

import { api } from "./api";
import type { HistoryStats } from "./types";

/** No database: the desktop default, and what a failed ask degrades to. */
const NONE: HistoryStats = { configured: false, timescale: false, rows: {} };

let pending: Promise<HistoryStats> | undefined;

function load(): Promise<HistoryStats> {
  // A backend too old to know this command, or one that cannot reach its
  // database, means "no history" rather than a broken screen. Every caller
  // uses this to decide what to offer, never to decide whether to render.
  pending ??= api.getHistoryStats().catch(() => NONE);
  return pending;
}

/**
 * What the history store is holding. `NONE` until the answer arrives, so a
 * first render offers ring-sized windows and widens once the server replies —
 * rather than offering a thirty-day button that briefly returns nothing.
 */
export function useHistoryStats(): HistoryStats {
  const [stats, setStats] = useState<HistoryStats>(NONE);

  useEffect(() => {
    let live = true;
    void load().then((answer) => live && setStats(answer));
    return () => {
      live = false;
    };
  }, []);

  return stats;
}

/** Forget the cached answer, so the next mount asks again. */
export function forgetHistoryStats() {
  pending = undefined;
}
