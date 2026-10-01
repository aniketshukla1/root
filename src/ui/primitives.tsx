import type { ComponentProps, KeyboardEvent, ReactNode } from "react";

import { healthColor, healthLabel, healthShape, markColor } from "../catalog";
import { iconFor } from "../icons/registry";
import type { Health, ServiceDescriptor } from "../types";

export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

// -------------------------------------------------------------------- status

/**
 * The health dot. Colour *and* silhouette both carry the state, so the signal
 * survives colour-vision deficiency, a monochrome screenshot, and forced-colors
 * mode. Everywhere it appears without an adjacent word, `title` supplies one.
 */
export function Dot({ health, size = 8 }: { health: Health; size?: number }) {
  const shape = healthShape(health);
  const color = healthColor(health);
  const common = { width: size, height: size, "aria-hidden": true as const };

  if (shape === "triangle") {
    return (
      <svg {...common} viewBox="0 0 10 10" className="shrink-0">
        <path d="M5 0.5 L9.5 9 L0.5 9 Z" fill={color} />
      </svg>
    );
  }
  if (shape === "square") {
    return (
      <svg {...common} viewBox="0 0 10 10" className="shrink-0">
        <rect x="0.5" y="0.5" width="9" height="9" fill={color} />
      </svg>
    );
  }
  if (shape === "ring") {
    return (
      <svg {...common} viewBox="0 0 10 10" className="shrink-0">
        <circle cx="5" cy="5" r="3.6" fill="none" stroke={color} strokeWidth="1.8" />
      </svg>
    );
  }
  return (
    <svg {...common} viewBox="0 0 10 10" className="shrink-0">
      <circle cx="5" cy="5" r="4" fill={color} />
    </svg>
  );
}

/** Dot plus the word. Use this wherever there is room; status is never colour alone. */
export function HealthBadge({ health, className }: { health: Health; className?: string }) {
  return (
    <span className={cx("inline-flex items-center gap-1.5 whitespace-nowrap", className)}>
      <Dot health={health} />
      <span className="text-2xs font-medium" style={{ color: healthColor(health) }}>
        {healthLabel(health)}
      </span>
    </span>
  );
}

// ------------------------------------------------------------------ identity

/**
 * A service's identity badge: brand mark when one ships, monogram otherwise.
 * The fallback is a supported outcome, not a missing asset — a collector this
 * build has never seen still gets a correct, stable badge.
 */
export function ServiceMark({
  descriptor,
  size = 20,
}: {
  descriptor: ServiceDescriptor;
  size?: number;
}) {
  const color = markColor(descriptor.accent);
  const mark = iconFor(descriptor.kind);

  return (
    <span
      className="inline-grid shrink-0 place-items-center rounded-sm"
      style={{
        width: size,
        height: size,
        color,
        boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${color} 45%, transparent)`,
        background: `color-mix(in srgb, ${color} 15%, transparent)`,
      }}
      title={descriptor.label}
    >
      {mark ? (
        <svg
          viewBox="0 0 24 24"
          width={size * 0.6}
          height={size * 0.6}
          fill="currentColor"
          aria-hidden
        >
          <path d={mark.path} />
        </svg>
      ) : (
        <span
          className="font-mono font-semibold leading-none"
          style={{ fontSize: size * 0.42 }}
        >
          {descriptor.monogram}
        </span>
      )}
    </span>
  );
}

/**
 * The mark: a filesystem tree — one solid root, three branches, three open
 * leaves. Root node, root cause, tree root. The root is solid and the leaves
 * are open because what the estate contains is discovered, not declared.
 */
export function RootMark({ size = 20 }: { size?: number }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.9}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      className="shrink-0"
    >
      <circle cx="5.6" cy="3.7" r="2" fill="currentColor" stroke="none" />
      <path d="M5.6 5.7V19.2M5.6 9.7h9.3M5.6 14.45h9.3M5.6 19.2h9.3" />
      <circle cx="17.4" cy="9.7" r="1.7" strokeWidth={1.35} />
      <circle cx="17.4" cy="14.45" r="1.7" strokeWidth={1.35} />
      <circle cx="17.4" cy="19.2" r="1.7" strokeWidth={1.35} />
    </svg>
  );
}

// -------------------------------------------------------------------- layout

/**
 * The 33px band at the top of a panel.
 *
 * `Panel` uses it, and so do the panels that cannot: an incident hero, a lane
 * chart and an evidence list all own their body layout but want the same head.
 * Those three had copied the band's classes, which is how two of them ended up
 * a pixel taller than the rest.
 */
export function PanelHeader({
  title,
  subtitle,
  actions,
  className,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <header
      className={cx(
        "panel-pad-x flex h-[33px] shrink-0 items-center gap-2.5 border-b border-line",
        className,
      )}
    >
      {/* `min-w-0` on both, `shrink-0` on the actions. A flex child's default
          minimum is its content, so `truncate` alone does nothing here — the
          title refuses to shrink and pushes the buttons out of the panel.
          When something has to give it should be the words, which are still
          readable from the tooltip, and never the controls, which cannot be
          clicked once they are off the edge. */}
      <h2 className="label min-w-0 truncate whitespace-nowrap">{title}</h2>
      {subtitle && (
        <span className="min-w-0 truncate text-2xs text-ink-4">{subtitle}</span>
      )}
      {actions && <div className="ml-auto flex shrink-0 items-center gap-1.5">{actions}</div>}
    </header>
  );
}

export function Panel({
  title,
  subtitle,
  actions,
  children,
  className,
  bodyClassName,
  flush,
}: {
  title?: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
  bodyClassName?: string;
  /** Table panels manage their own padding so rows can bleed to the edge. */
  flush?: boolean;
}) {
  return (
    <section className={cx("panel flex min-w-0 flex-col overflow-hidden", className)}>
      {(title || actions) && (
        <PanelHeader title={title} subtitle={subtitle} actions={actions} />
      )}
      <div className={cx("min-h-0 flex-1", flush ? "" : "panel-pad", bodyClassName)}>{children}</div>
    </section>
  );
}

/**
 * Column tracks that do not leave one card alone on the last row.
 *
 * `auto-fill` packs as many 340px tracks as fit and lets the remainder fall
 * where it falls — which at 1440px put four time series in a 3 + 1, the lone
 * chart reading as a different kind of thing rather than the fourth of four.
 * Capping the track count at a divisor-friendly number turns that into 2 + 2,
 * and a narrow window still collapses because the minimum is unchanged.
 */
export function balancedTracks(count: number, min: number) {
  const cap = count <= 3 ? count : count === 4 ? 2 : count % 3 === 1 ? 4 : 3;
  return {
    gridTemplateColumns: `repeat(auto-fill, minmax(max(${min}px, calc((100% - ${(cap - 1) * 12}px) / ${cap})), 1fr))`,
  };
}


export function Empty({ title, hint }: { title: string; hint?: ReactNode }) {
  return (
    <div className="flex h-full min-h-24 flex-col items-center justify-center gap-1 px-4 py-8 text-center">
      <p className="text-base font-medium text-ink-2">{title}</p>
      {hint && <p className="max-w-md text-xs text-ink-4">{hint}</p>}
    </div>
  );
}

// ------------------------------------------------------------------ controls

type ButtonVariant = "default" | "primary" | "danger" | "ghost";

/**
 * Control heights, named.
 *
 * `sm` is the in-a-header size, `md` the default, `lg` a form's submit. Six
 * different heights used to be reached by passing `h-6` or `h-8` in
 * `className`, which meant the system had a size scale that only existed in
 * call sites.
 */
export type ControlSize = "sm" | "md" | "lg";

export const CONTROL_H: Record<ControlSize, string> = {
  sm: "h-6",
  md: "h-7",
  lg: "h-8",
};

export function Button({
  variant = "default",
  size = "md",
  loading,
  className,
  children,
  ...props
}: ComponentProps<"button"> & {
  variant?: ButtonVariant;
  size?: ControlSize;
  /** Reserves the label's width and blocks the click, so the row cannot jump. */
  loading?: boolean;
}) {
  const styles: Record<ButtonVariant, string> = {
    default:
      "border-line-strong bg-raised text-ink hover:bg-overlay hover:border-ink-4/50",
    // The focus ring is the button's own ink: the default ring is a light
    // grey drawn just inside the edge, which on a white button is invisible.
    primary:
      "border-accent bg-accent text-on-accent hover:bg-accent-dim hover:border-accent-dim focus-visible:[outline-color:var(--color-on-accent)]",
    danger:
      "border-crit/45 bg-crit/10 text-crit hover:border-crit",
    ghost:
      "border-transparent text-ink-3 hover:bg-wash hover:text-ink",
  };
  return (
    <button
      type="button"
      aria-busy={loading || undefined}
      {...props}
      disabled={props.disabled || loading}
      className={cx(
        // One line, always, as ferro's buttons are: a label that wraps spills
        // out of a fixed-height button and over whatever is under it.
        "relative inline-flex min-w-0 shrink-0 items-center justify-center gap-1.5 rounded-md border px-3 whitespace-nowrap",
        CONTROL_H[size],
        size === "sm" ? "rounded-sm text-xs font-medium" : "text-[13px] font-medium",
        "transition-colors disabled:pointer-events-none disabled:opacity-40",
        styles[variant],
        className,
      )}
    >
      {/* The label stays in the layout while it is hidden: a button that
          shrinks to the width of a spinner drags every control beside it. */}
      <span className={cx("inline-flex min-w-0 items-center gap-1.5 overflow-hidden", loading && "invisible")}>
        {children}
      </span>
      {loading && <Spinner />}
    </button>
  );
}

/** Two arcs of the accent, sized to the text beside it. */
function Spinner() {
  return (
    <svg
      className="absolute h-3.5 w-3.5 animate-spin"
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden
    >
      <circle cx="8" cy="8" r="6" stroke="currentColor" strokeWidth="2" opacity="0.25" />
      <path d="M14 8a6 6 0 0 0-6-6" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
    </svg>
  );
}

export function Input({
  className,
  size = "md",
  invalid,
  ...props
}: Omit<ComponentProps<"input">, "size"> & {
  size?: ControlSize;
  /** Draws the error border AND says so to a screen reader. Never one alone. */
  invalid?: boolean;
}) {
  return (
    <input
      {...props}
      aria-invalid={invalid || undefined}
      className={cx(
        "min-w-0 rounded-md border bg-sunk px-2.5 text-[13px] text-ink",
        CONTROL_H[size],
        "placeholder:text-ink-4 focus:outline-none transition-[border-color,box-shadow]",
        invalid
          ? "border-crit focus:border-crit"
          : "border-line-strong hover:border-ink-4/60 focus:border-ink/45 focus:shadow-[0_0_0_3px_color-mix(in_oklab,var(--color-ink)_9%,transparent)]",
        "disabled:cursor-not-allowed disabled:opacity-40",
        className,
      )}
    />
  );
}

/**
 * A labelled control, with its error bound to it.
 *
 * The `<label>` wraps the control, which is what labels it — no `htmlFor`/`id`
 * pair to keep in sync. The error is the part that was missing everywhere:
 * rendered as loose text beside a field it is invisible to a screen reader,
 * so here it carries `role="alert"` and the caller marks the control
 * `invalid`, which is what actually reaches assistive tech.
 */
export function Field({
  label,
  required,
  help,
  error,
  children,
}: {
  label: ReactNode;
  required?: boolean;
  help?: ReactNode;
  error?: ReactNode;
  children: ReactNode;
}) {
  return (
    <label className="flex min-w-0 flex-col gap-1">
      <span className="text-xs text-ink-3">
        {label}
        {required && (
          <span className="ml-0.5 text-crit" title="Required">
            *
          </span>
        )}
      </span>
      {children}
      {error ? (
        <span role="alert" className="text-2xs leading-snug text-crit">
          {error}
        </span>
      ) : (
        help && <span className="text-2xs leading-snug text-ink-4">{help}</span>
      )}
    </label>
  );
}

export function Select({
  className,
  size = "md",
  children,
  ...props
}: Omit<ComponentProps<"select">, "size"> & { size?: ControlSize }) {
  return (
    <select
      {...props}
      className={cx(
        "min-w-0 cursor-pointer rounded-md border border-line bg-sunk px-2 font-mono text-xs text-ink",
        CONTROL_H[size],
        "focus:border-accent focus:outline-none",
        "disabled:cursor-not-allowed disabled:opacity-40",
        className,
      )}
    >
      {children}
    </select>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  className,
  label,
}: {
  value: T;
  options: { value: T; label: ReactNode; title?: string }[];
  onChange: (value: T) => void;
  className?: string;
  /** What this group filters — announced before the options. */
  label?: string;
}) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={cx(
        // `w-fit` so it hugs its options in a column layout too, where the
        // default cross-axis stretch would otherwise pull it full-width.
        "inline-flex h-7 w-fit shrink-0 items-center gap-[2px] rounded-md border border-line bg-sunk p-[2px]",
        className,
      )}
    >
      {options.map((option) => (
        <button
          key={option.value}
          // A filter, not a tab: nothing here controls a tabpanel, and a
          // screen reader announcing "tab 2 of 4" for a severity filter
          // describes a widget that does not exist.
          role="radio"
          aria-checked={option.value === value}
          title={option.title}
          onClick={() => onChange(option.value)}
          className={cx(
            "h-[22px] rounded-sm px-2.5 text-xs font-medium transition-colors",
            option.value === value
              ? "bg-overlay text-ink shadow-[0_0_0_1px_var(--color-line-strong),0_1px_2px_rgba(0,0,0,0.16)]"
              : "text-ink-3 hover:text-ink",
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function Tabs<T extends string>({
  value,
  options,
  onChange,
  panelId,
}: {
  value: T;
  options: { value: T; label: ReactNode; count?: number }[];
  onChange: (value: T) => void;
  /** The element these tabs switch, so the relationship is stated, not implied. */
  panelId?: string;
}) {
  /**
   * A tablist is a single tab stop and the arrows move between tabs. Without
   * this, `role="tab"` is a promise to a screen-reader user that the widget
   * does not keep — they tab into it and the arrow keys do nothing.
   */
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const keys = ["ArrowRight", "ArrowLeft", "Home", "End"];
    if (!keys.includes(event.key)) return;
    event.preventDefault();
    const i = options.findIndex((o) => o.value === value);
    const next =
      event.key === "ArrowRight"
        ? (i + 1) % options.length
        : event.key === "ArrowLeft"
          ? (i - 1 + options.length) % options.length
          : event.key === "Home"
            ? 0
            : options.length - 1;
    onChange(options[next].value);
    const tabs = event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]');
    tabs[next]?.focus();
  };

  return (
    <div
      role="tablist"
      onKeyDown={onKeyDown}
      // Scrolls on its own when the tabs outgrow the panel, rather than
      // widening it: a strip that overflows its panel makes the whole panel
      // scrollable, and the first thing to take focus scrolls it sideways.
      // The rule under the strip is drawn inside it (an inset shadow) rather
      // than as a border, so the active tab's underline lies over it without
      // reaching outside the box a scrolling strip clips to.
      className="no-scrollbar flex min-w-0 items-center gap-4 overflow-x-auto overflow-y-hidden px-3 shadow-[inset_0_-1px_0_var(--color-line)]"
    >
      {options.map((option) => (
        <button
          key={option.value}
          role="tab"
          aria-selected={option.value === value}
          aria-controls={panelId}
          tabIndex={option.value === value ? 0 : -1}
          onClick={() => onChange(option.value)}
          className={cx(
            "flex h-8 shrink-0 items-center gap-1.5 border-b-2 text-[13px] font-medium whitespace-nowrap transition-colors",
            option.value === value
              ? "border-accent text-ink"
              : "border-transparent text-ink-4 hover:text-ink-2",
          )}
        >
          {option.label}
          {option.count !== undefined && option.count > 0 && (
            <span className="num rounded-sm bg-sunk px-1 text-2xs text-ink-3">
              {option.count}
            </span>
          )}
        </button>
      ))}
    </div>
  );
}

// --------------------------------------------------------------------- atoms

export function Tag({
  children,
  color,
  title,
  className,
}: {
  children: ReactNode;
  color?: string;
  title?: string;
  className?: string;
}) {
  return (
    <span
      title={title}
      className={cx(
        "inline-flex h-[18px] shrink-0 items-center rounded-sm border bg-panel px-1.5 text-2xs font-medium whitespace-nowrap",
        className,
      )}
      style={
        color
          ? {
              color,
              borderColor: `color-mix(in srgb, ${color} 32%, transparent)`,
              background: `color-mix(in srgb, ${color} 10%, transparent)`,
            }
          : { color: "var(--color-ink-3)", borderColor: "var(--color-line)" }
      }
    >
      {children}
    </span>
  );
}

/**
 * A small pill you can press — the interactive sibling of `Tag`.
 *
 * Seven of these were hand-rolled across the recall strip, the lane list and
 * the evidence footer, each at its own height and padding, and the smallest
 * were 20px tall. One component means one hit area (24px) and one focus ring.
 */
export function Chip({
  children,
  selected,
  quiet,
  className,
  ...props
}: ComponentProps<"button"> & {
  selected?: boolean;
  /**
   * For a long row of filters, where a border on every unselected option is
   * more grid than the page can carry. The selected state then reads as a
   * raised plate rather than an accent fill.
   */
  quiet?: boolean;
}) {
  return (
    <button
      type="button"
      aria-pressed={selected}
      {...props}
      className={cx(
        "inline-flex h-6 shrink-0 items-center gap-1.5 rounded-sm border px-2",
        "font-mono text-2xs whitespace-nowrap transition-colors",
        "disabled:pointer-events-none disabled:opacity-40",
        quiet
          ? selected
            ? "border-line-strong bg-raised text-ink"
            : "border-transparent text-ink-4 hover:bg-raised/60 hover:text-ink-2"
          : selected
            ? "border-accent bg-accent text-on-accent"
            : "border-line bg-panel text-ink-3 hover:border-accent hover:text-accent",
        className,
      )}
    >
      {children}
    </button>
  );
}

/**
 * A labelled figure. The value uses tabular figures so a column of these lines
 * up, and the label sits *under* the number — a reader scanning a strip of
 * these is looking for the digits, not the words.
 *
 * `size` reads from the display scale rather than from a number at the call
 * site. This component existed and was used nowhere, while nineteen hand-rolled
 * copies of it ran at six different sizes.
 */
export function Stat({
  label,
  value,
  tone,
  hint,
  size = "lg",
  labelPosition = "below",
  children,
}: {
  label: ReactNode;
  value: ReactNode;
  tone?: string;
  hint?: string;
  /** `md` for a strip of many, `lg` for the two or three that lead a panel. */
  size?: "md" | "lg";
  /**
   * Where the caption sits. `below` is the default — a reader scanning a strip
   * of figures wants the digits first. `above` is for a card in a grid, where
   * the captions have to line up across cards to be comparable at all.
   * `inline` is the dense header run.
   */
  labelPosition?: "below" | "above" | "inline";
  /** Trend, threshold, sparkline — whatever qualifies the number. */
  children?: ReactNode;
}) {
  const figure = (
    <span
      className={cx(
        "num leading-none font-medium tracking-[-.03em] whitespace-nowrap",
        size === "lg" ? "text-3xl" : "text-xl",
      )}
      style={{ color: tone ?? "var(--color-ink)" }}
    >
      {value}
    </span>
  );

  const caption = <span className="label block truncate">{label}</span>;

  return (
    <div className="min-w-0" title={hint}>
      {labelPosition === "inline" ? (
        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
          {figure}
          {caption}
          {children}
        </div>
      ) : (
        <>
          {labelPosition === "above" && caption}
          <div
            className={cx(
              "flex flex-wrap items-end gap-x-2 gap-y-1",
              labelPosition === "above" && "mt-1",
            )}
          >
            {figure}
            {children}
          </div>
          {labelPosition === "below" && <div className="mt-1.5">{caption}</div>}
        </>
      )}
    </div>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex h-[18px] min-w-[18px] items-center justify-center rounded-[4px] border border-line-strong bg-overlay px-1 font-sans text-[10.5px] font-medium text-ink-3">
      {children}
    </kbd>
  );
}

/** Thin separator for inline metadata runs. */
export function Sep() {
  return <span className="text-ink-4/50">·</span>;
}
