import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { formatRelative } from "../format";
import { useTick } from "../ui/charts";
import { Button, cx, Kbd } from "../ui/primitives";
import { AssistantIcon, RefreshIcon } from "../ui/icons";

/**
 * Every screen opens with the same 44px band: what you are looking at on the
 * left, controls on the right. Keeping it identical across screens is what
 * lets the eye go straight past it to the data.
 */
export function ScreenHeader({
  title,
  meta,
  children,
  onRefresh,
  onAsk,
  syncedAt,
  live,
}: {
  title: ReactNode;
  meta?: ReactNode;
  children?: ReactNode;
  onRefresh?: () => void;
  /** Present on every screen: the assistant is reachable without leaving one. */
  onAsk?: () => void;
  syncedAt?: string;
  live?: boolean;
}) {
  useTick(15000);
  const metaRef = useRef<HTMLDivElement>(null);
  const [metaTitle, setMetaTitle] = useState<string>();

  // The meta line truncates, and until now it truncated into nothing: no
  // caller could pass a title because `meta` is arbitrary JSX, so the text
  // simply ended. The element reads its own content instead, and only when it
  // is genuinely clipped — a tooltip on text that already fits is noise that
  // follows the pointer around.
  useEffect(() => {
    const measure = () => {
      const el = metaRef.current;
      if (!el) return;
      const clipped = el.scrollWidth > el.clientWidth + 1;
      setMetaTitle(clipped ? (el.textContent ?? "").trim() || undefined : undefined);
    };
    measure();

    // Width is the whole question here, so a resize has to re-ask it.
    const observer = new ResizeObserver(measure);
    if (metaRef.current) observer.observe(metaRef.current);
    return () => observer.disconnect();
  }, [meta]);

  return (
    // The header wraps as a whole: when the filters cannot sit beside the
    // title, they drop to a row of their own rather than squeezing in under
    // it. Squeezed, a right-aligned group that cannot fit overflows to its
    // left — over the title, and past the edge of the header.
    <header className="sticky top-0 z-10 flex min-h-[52px] shrink-0 flex-wrap items-start gap-x-3 gap-y-2 border-b border-line bg-page/95 px-4 py-[11px] backdrop-blur">
      <h1 className="shrink-0 text-3xl leading-[30px] font-bold tracking-[-.03em] text-ink">
        {title}
      </h1>
      {meta && (
        <div
          ref={metaRef}
          title={metaTitle}
          className="flex min-h-[30px] min-w-0 items-center gap-1.5 truncate text-xs text-ink-4"
        >
          {meta}
        </div>
      )}

      {/* The screen's own filters and the controls every screen shares wrap
          as one right-aligned group. They used to sit in one row that refused
          to shrink, so on Alerts — a search, three segmented controls — the
          row was 88px wider than a 1440px window, and at 1100px group-by and
          Ask were cut off the edge entirely. Wrapping grows the band downward
          and only when the width is genuinely short, so at a given window
          size the header still holds still.

          The shared controls are one item inside this group, not a sibling of
          it: as a sibling they wrapped onto a header row of their own and sat
          at its left edge, stranded under the title. */}
      {/* No `min-w-0`: the group must not shrink below its widest control, or
          the header never wraps and the controls overflow instead. */}
      <div className="ml-auto flex min-h-[30px] max-w-full flex-wrap items-center justify-end gap-2">
        {children}
        <div className={cx("flex min-h-[30px] shrink-0 items-center gap-2", children != null && "ml-1")}>
          {onAsk && (
            <Button onClick={onAsk} title="Ask the assistant  ⌘J" className="gap-1.5">
              <span className="flex text-accent">
                <AssistantIcon size={13} />
              </span>
              Ask
              <Kbd>⌘J</Kbd>
            </Button>
          )}
          {syncedAt && (
            <span className="flex items-center gap-1.5 text-xs text-ink-4">
              {live && (
                <span className="relative flex h-1.5 w-1.5" title="Receiving live updates">
                  <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-ok opacity-60" />
                  <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-ok" />
                </span>
              )}
              {/* Width reserved, not measured. This label sits at the right-hand
                  end of a group anchored to the right edge, so any change in its
                  width moves everything to its left — the filter, both segmented
                  controls and Ask. It alternates between "just now" and "6s ago"
                  on nearly every fetch, which slid the whole cluster 13px back
                  and forth every few seconds. 8ch is a little wider than "just
                  now", the widest thing formatRelative returns short of a
                  hundred days. */}
              <span className="num inline-block min-w-[8ch]">{formatRelative(syncedAt)}</span>
            </span>
          )}
          {onRefresh && (
            <Button variant="ghost" onClick={onRefresh} title="Refresh now" className="px-1.5">
              <RefreshIcon size={14} />
            </Button>
          )}
        </div>
      </div>
    </header>
  );
}

/**
 * Standard body container for a screen.
 *
 * `scroll={false}` hands scrolling to a child instead — which is what a
 * console-style screen needs, so its stream scrolls under a pinned filter bar
 * rather than dragging the whole page.
 */
export function Screen({
  children,
  className,
  padded = true,
  scroll = true,
}: {
  children: ReactNode;
  className?: string;
  padded?: boolean;
  scroll?: boolean;
}) {
  return (
    <div
      className={cx(
        // A container, not just a box. The assistant dock takes real width off
        // this column, and viewport breakpoints cannot see that — they kept
        // reporting a wide window while the content had 300px to live in, and
        // headings wrapped one word per line. Everything inside sizes itself
        // with @ variants against THIS element instead.
        "@container min-h-0 flex-1",
        // Direct children of a scrolling flex column must not shrink.
        //
        // A scroll container has a definite height, so flex items default to
        // being squeezed to fit it — and a panel with `overflow-hidden` then
        // clips its own content instead of letting the page scroll. Metrics
        // rendered eleven connections as eleven empty bands this way: each
        // panel wanted 169px, each was given 24.
        //
        // This used to be an opt-in `.stack` class, and four of the six
        // screens that needed it did not have it. A rule every caller must
        // remember is a rule that gets forgotten, so it lives here now. On a
        // Screen that is not a flex column it is simply inert.
        "[&>*]:shrink-0",
        scroll ? "overflow-y-auto" : "overflow-hidden",
        padded && "screen-pad",
        className,
      )}
    >
      {children}
    </div>
  );
}
