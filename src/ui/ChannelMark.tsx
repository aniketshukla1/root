import type { CSSProperties, ReactNode } from "react";

import type { ChannelKind } from "../types";
import { cx } from "./primitives";

/**
 * Where an alert can be sent, drawn as the brand it goes to.
 *
 * The same idea as a connection's mark: the colour is the brand's own, so a
 * row of these reads as "Slack, PagerDuty" before a word of it is read. A
 * webhook has no brand, so it is the one neutral tile.
 */
export const CHANNEL_BRAND: Record<ChannelKind, { label: string; color: string; glyph: ReactNode }> = {
  slack: {
    label: "Slack",
    color: "#611f69",
    glyph: (
      <path
        d="M9.5 4 7.5 20M16.5 4l-2 16M4.5 9h15.5M4 15h15.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.4"
        strokeLinecap="round"
      />
    ),
  },
  teams: {
    label: "Microsoft Teams",
    color: "#5b5fc7",
    glyph: <path d="M5 5.5h14v3.2h-5.3V19h-3.4V8.7H5z" fill="currentColor" />,
  },
  discord: {
    label: "Discord",
    color: "#5865f2",
    glyph: (
      <>
        <path
          d="M6.2 7.2c3.8-1.7 7.8-1.7 11.6 0l1.7 8.6c-1.5 1.3-3.2 2-4.8 2.2l-.9-1.6c-1.2.4-2.4.4-3.6 0l-.9 1.6c-1.6-.2-3.3-.9-4.8-2.2z"
          fill="currentColor"
        />
        <circle cx="9.6" cy="12.4" r="1.35" fill="var(--mark-bg)" />
        <circle cx="14.4" cy="12.4" r="1.35" fill="var(--mark-bg)" />
      </>
    ),
  },
  pagerduty: {
    label: "PagerDuty",
    color: "#06ac38",
    glyph: (
      <path
        d="M8 19V5h5.2c3 0 4.8 1.7 4.8 4.4s-1.8 4.4-4.8 4.4H11V19z"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.6"
        strokeLinejoin="round"
      />
    ),
  },
  webhook: {
    label: "Webhook",
    color: "var(--color-overlay)",
    glyph: (
      <path
        d="M9 5c-2 0-2.8 1-2.8 2.8v2c0 1.2-.6 2.2-2.2 2.2 1.6 0 2.2 1 2.2 2.2v2C6.2 18 7 19 9 19M15 5c2 0 2.8 1 2.8 2.8v2c0 1.2.6 2.2 2.2 2.2-1.6 0-2.2 1-2.2 2.2v2C17.8 18 17 19 15 19"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.9"
        strokeLinecap="round"
      />
    ),
  },
};

export function ChannelMark({
  kind,
  size = 28,
  className,
  title,
}: {
  kind: ChannelKind;
  size?: number;
  className?: string;
  /** Defaults to the brand's name. */
  title?: string;
}) {
  const brand = CHANNEL_BRAND[kind];
  const neutral = kind === "webhook";
  return (
    <span
      title={title ?? brand.label}
      className={cx(
        "inline-grid shrink-0 place-items-center",
        size < 20 ? "rounded-[4px]" : "rounded-[6px]",
        neutral ? "border border-line-strong text-ink-2" : "text-white",
        className,
      )}
      style={
        {
          width: size,
          height: size,
          background: brand.color,
          // The Discord mark punches its eyes out in the tile's own colour.
          "--mark-bg": brand.color,
        } as CSSProperties
      }
    >
      <svg width={size * 0.62} height={size * 0.62} viewBox="0 0 24 24" aria-hidden>
        {brand.glyph}
      </svg>
    </span>
  );
}
