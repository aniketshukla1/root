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

  return (
    <header className="sticky top-0 z-10 flex h-[52px] shrink-0 items-center gap-3 border-b border-line bg-page/95 px-4 backdrop-blur">
      <h1 className="shrink-0 text-[22px] font-bold tracking-[-.03em] text-ink">{title}</h1>
      {meta && (
        <div className="flex min-w-0 items-center gap-1.5 truncate text-xs text-ink-4">{meta}</div>
      )}

      <div className="ml-auto flex shrink-0 items-center gap-2">
        {children}
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
                and forth every few seconds. The face is monospace, so 8ch is
                exactly "just now", the widest thing formatRelative returns
                short of a hundred days. */}
            <span className="num inline-block min-w-[8ch]">{formatRelative(syncedAt)}</span>
          </span>
        )}
        {onRefresh && (
          <Button variant="ghost" onClick={onRefresh} title="Refresh now" className="px-1.5">
            <RefreshIcon size={14} />
          </Button>
        )}
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
        padded && "p-4",
        className,
      )}
    >
      {children}
    </div>
  );
}
