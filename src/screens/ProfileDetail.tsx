// One flame graph, and the reason it is worth looking at.
//
// A flame graph is a picture, and a picture is not a finding. The same
// function usually appears in a dozen places in the graph — once per call
// path — and each of those rectangles is too thin to notice while together
// they are the reason the process is slow. So this screen leads with the
// self-time ranking, the way TraceDetail leads with where the time went, and
// the graph below it is how you check the ranking rather than how you find it.
//
// Frames arrive positioned and parented from the backend (see `profiles.rs`).
// The wire format delta-encodes every offset against the end of the previous
// frame on the same row, and a renderer that read those as absolute positions
// would draw a graph that was subtly, plausibly wrong. Nothing here does
// arithmetic on an offset it did not receive.

import { useCallback, useEffect, useMemo, useState } from "react";

import { api, errorMessage } from "../api";
import { formatBytes, formatDuration } from "../format";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import type { Profile } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Button, cx, Empty, Panel, PanelHeader, Segmented, Sep } from "../ui/primitives";
import { ChevronLeft } from "../ui/icons";

/** Row height in pixels. Tight enough that a deep stack fits on a screen. */
const ROW_H = 18;

/** Windows offered, in minutes. A profile is an aggregate over time, and a
 *  window measured in seconds is a sample of a sample. */
const WINDOWS = [
  { value: "15", label: "15m" },
  { value: "60", label: "1h" },
  { value: "360", label: "6h" },
  { value: "1440", label: "24h" },
];

/** Narrowest frame still worth drawing. Below this it is a line, not a box,
 *  and a hundred of them are a smear rather than information. */
const MIN_VISIBLE_PERCENT = 0.08;

export function ProfileDetail({
  profileId,
  query,
  model,
  onNavigate,
}: {
  profileId: string;
  /** The store's own selector — a profile type and a label set. */
  query: string;
  model: EstateModel;
  onNavigate: (view: View) => void;
}) {
  const [profile, setProfile] = useState<Profile | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [minutes, setMinutes] = useState("60");

  const service = model.services.find((s) => s.snapshot.profileId === profileId);
  const profileName = service?.snapshot.profileName ?? profileId;

  const load = useCallback(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .getProfile(profileId, query, Number(minutes))
      .then((result) => {
        if (cancelled) return;
        setProfile(result);
        setLoading(false);
      })
      .catch((cause) => {
        if (cancelled) return;
        setProfile(null);
        setError(errorMessage(cause));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [profileId, query, minutes]);

  useEffect(() => load(), [load]);

  const onBack = () => onNavigate({ name: "service", profileId, tab: "components" });

  if (loading) {
    return (
      <Screen>
        <Empty title="Reading the profile" hint="Merging samples in the store." />
      </Screen>
    );
  }

  if (error || !profile) {
    return (
      <Screen>
        <Empty
          title="That profile could not be read"
          hint={
            <span className="flex flex-col items-center gap-3">
              <span>{error ?? "Nothing came back."}</span>
              <span className="flex gap-2">
                <Button onClick={onBack}>Back</Button>
                <Button variant="primary" onClick={load}>
                  Try again
                </Button>
              </span>
            </span>
          }
        />
      </Screen>
    );
  }

  return (
    <>
      <ScreenHeader
        title={profile.name || "Profile"}
        meta={
          <>
            <Button variant="ghost" onClick={onBack} title="Back">
              <ChevronLeft />
            </Button>
            <span className="num text-2xs font-semibold tracking-[.06em] text-ink-3 uppercase">
              {profileName}
            </span>
            {profile.spy && (
              <>
                <Sep />
                <span className="num">{profile.spy}</span>
              </>
            )}
            <Sep />
            <Segmented value={minutes} onChange={setMinutes} options={WINDOWS} />
          </>
        }
      />

      <Screen>
        {profile.frames.length === 0 ? (
          <Empty
            title="No samples in this window"
            hint="The store answered, and had nothing for this selector over this window. A longer window, or a selector that matches something currently running, is the usual fix."
          />
        ) : (
          <>
            <Ranking profile={profile} />
            <FlameGraph profile={profile} />
          </>
        )}
      </Screen>
    </>
  );
}

/**
 * The answer, before the picture.
 *
 * Ranked by self samples summed across every call site, because the question
 * is "what is expensive", and the graph answers "where is it called from".
 */
function Ranking({ profile }: { profile: Profile }) {
  return (
    <Panel>
      <PanelHeader
        title="Where it goes"
        subtitle={`${describe(profile.total, profile.unit)} in the window`}
      />
      <div className="flex flex-col">
        {profile.hot.map((symbol) => (
          <div
            key={symbol.name}
            className="flex items-baseline gap-3 border-b border-line px-3 py-1.5 last:border-b-0"
          >
            <span className="num w-14 shrink-0 text-right text-xs tabular-nums text-ink">
              {(symbol.share * 100).toFixed(1)}%
            </span>
            {/* The bar is scaled against the worst symbol rather than against
                the whole profile: a flat profile with no culprit should read
                as flat, not as twelve invisible slivers. */}
            <span className="h-1.5 w-20 shrink-0 self-center overflow-hidden rounded-full bg-line">
              <span
                className="block h-full rounded-full bg-accent"
                style={{
                  width: `${Math.max(2, (symbol.own / (profile.hot[0]?.own || 1)) * 100)}%`,
                }}
              />
            </span>
            <span className="num min-w-0 flex-1 truncate text-xs text-ink" title={symbol.name}>
              {symbol.name}
            </span>
            {symbol.sites > 1 && (
              <span
                className="num shrink-0 text-2xs text-ink-4"
                title="Called from this many places in the graph. A cost spread across call sites is invisible in the picture and obvious here."
              >
                {symbol.sites} sites
              </span>
            )}
            <span className="num shrink-0 text-2xs text-ink-4 tabular-nums">
              {describe(symbol.own, profile.unit)}
            </span>
          </div>
        ))}
      </div>
    </Panel>
  );
}

/**
 * The graph. Click a frame to make it the root; click the root to go back out.
 *
 * Zooming is containment against the focused frame rather than a re-fetch:
 * the samples do not change, only which slice of them fills the width.
 */
function FlameGraph({ profile }: { profile: Profile }) {
  const [focus, setFocus] = useState(0);
  const [hover, setHover] = useState<number | null>(null);

  // Clicking a frame in a graph that has since been reloaded must not leave a
  // focus pointing past the end of the array.
  const root = profile.frames[focus] ?? profile.frames[0];
  const span = Math.max(root?.total ?? 0, 1);

  const visible = useMemo(() => {
    if (!root) return [];
    const end = root.offset + root.total;
    return profile.frames
      .map((frame, index) => ({ frame, index }))
      .filter(
        ({ frame }) =>
          frame.depth >= root.depth &&
          frame.offset < end &&
          frame.offset + frame.total > root.offset,
      );
  }, [profile.frames, root]);

  const depth = visible.reduce((max, { frame }) => Math.max(max, frame.depth), root?.depth ?? 0);
  const rows = depth - (root?.depth ?? 0) + 1;

  if (!root) return null;

  const shown = hover !== null ? profile.frames[hover] : null;

  return (
    <Panel>
      <PanelHeader
        title="Flame graph"
        subtitle={
          focus === 0
            ? "Click a frame to zoom into it"
            : `Zoomed into ${root.name} — click it again to zoom out`
        }
        actions={
          focus !== 0 && (
            <Button variant="ghost" onClick={() => setFocus(0)}>
              Reset
            </Button>
          )
        }
      />

      {/* Two elements rather than one: an absolute child's percentages resolve
          against its positioned ancestor's *padding* box, so putting the
          padding on the positioned element would push every frame right by the
          gutter and clip the last one. The inner div is the drawing area, and
          100% of it is 100% of the graph. */}
      <div className="w-full px-3 py-2" onMouseLeave={() => setHover(null)}>
        <div className="relative w-full overflow-hidden" style={{ height: rows * ROW_H }}>
          {visible.map(({ frame, index }) => {
            // Clamp to the focused frame so a partially-contained ancestor is
            // drawn at the width it actually occupies here rather than
            // overflowing the panel.
            const start = Math.max(frame.offset, root.offset);
            const stop = Math.min(frame.offset + frame.total, root.offset + root.total);
            const width = ((stop - start) / span) * 100;
            if (width < MIN_VISIBLE_PERCENT) return null;

            const heat = profile.maxSelf > 0 ? frame.own / profile.maxSelf : 0;
            return (
              <button
                key={`${frame.depth}-${frame.offset}-${index}`}
                type="button"
                onClick={() => setFocus(index === focus ? (frame.parent ?? 0) : index)}
                onMouseEnter={() => setHover(index)}
                title={`${frame.name} — ${describe(frame.own, profile.unit)} of its own, ${describe(frame.total, profile.unit)} including what it called`}
                className={cx(
                  "absolute overflow-hidden rounded-[2px] border border-panel px-1 text-left",
                  "transition-[filter] hover:brightness-125",
                  hover === index && "ring-1 ring-accent",
                )}
                style={{
                  left: `${((start - root.offset) / span) * 100}%`,
                  top: (frame.depth - root.depth) * ROW_H,
                  width: `calc(${width}% - 1px)`,
                  height: ROW_H - 2,
                  // Self samples drive the colour, so the frames that are
                  // actually spending the time are the ones that stand out —
                  // a wide frame that only waited on its children stays cool.
                  // Floored at a few percent so a frame that spent nothing is
                  // still a frame rather than a hole in the graph.
                  backgroundColor: `color-mix(in oklab, var(--color-warn) ${Math.round(6 + heat * 70)}%, var(--color-raised))`,
                }}
              >
                <span
                  className={cx(
                    "num block truncate text-[10px] leading-none",
                    // Light ink on a dark frame, dark on a hot one: the fill at
                    // full heat is bright enough that white text on it is the
                    // one thing on this screen that cannot be read.
                    heat > 0.55 ? "text-on-accent" : "text-ink",
                  )}
                >
                  {frame.name}
                </span>
              </button>
            );
          })}
        </div>
      </div>

      {/* A fixed line rather than a floating tooltip: a tooltip that follows
          the pointer across a hundred rectangles is unreadable, and the thing
          being read is always the same three numbers. */}
      <div className="flex items-baseline gap-3 border-t border-line px-3 py-1.5">
        {shown ? (
          <>
            <span className="num min-w-0 flex-1 truncate text-xs text-ink">{shown.name}</span>
            <span className="num shrink-0 text-2xs text-ink-4">
              {describe(shown.own, profile.unit)} itself
            </span>
            <span className="num shrink-0 text-2xs text-ink-4">
              {describe(shown.total, profile.unit)} with children
            </span>
            <span className="num w-14 shrink-0 text-right text-xs tabular-nums text-ink">
              {((shown.total / Math.max(profile.total, 1)) * 100).toFixed(1)}%
            </span>
          </>
        ) : (
          <span className="num text-2xs text-ink-4">
            Colour is self time, not width — a wide frame that only waited on what it called
            stays cool.
          </span>
        )}
      </div>
    </Panel>
  );
}

/**
 * A sample count in whatever the store said it was counting.
 *
 * A store that named no unit gets a bare number rather than a guessed one:
 * calling samples "ms" because they usually are would be the kind of quiet
 * wrong that is worse than saying nothing.
 */
function describe(value: number, unit: string): string {
  switch (unit) {
    case "nanoseconds":
      return formatDuration(value / 1e6);
    case "bytes":
      return formatBytes(value);
    case "objects":
      return `${value.toLocaleString()} objects`;
    case "samples":
      return `${value.toLocaleString()} samples`;
    default:
      return value.toLocaleString();
  }
}
