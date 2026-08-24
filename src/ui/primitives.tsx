import type { ComponentProps, ReactNode } from "react";

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
      <span
        className="font-mono text-2xs tracking-[.05em]"
        style={{ color: healthColor(health) }}
      >
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
        <header className="flex h-[33px] shrink-0 items-center gap-2.5 border-b border-line px-3">
          <h2 className="label truncate whitespace-nowrap">{title}</h2>
          {subtitle && (
            <span className="truncate font-mono text-2xs text-ink-4">{subtitle}</span>
          )}
          {actions && <div className="ml-auto flex items-center gap-1.5">{actions}</div>}
        </header>
      )}
      <div className={cx("min-h-0 flex-1", flush ? "" : "p-3", bodyClassName)}>{children}</div>
    </section>
  );
}

export function Empty({ title, hint }: { title: string; hint?: ReactNode }) {
  return (
    <div className="flex h-full min-h-24 flex-col items-center justify-center gap-1 px-4 py-8 text-center">
      <p className="text-[13px] font-medium text-ink-2">{title}</p>
      {hint && <p className="max-w-md text-xs text-ink-4">{hint}</p>}
    </div>
  );
}

// ------------------------------------------------------------------ controls

type ButtonVariant = "default" | "primary" | "danger" | "ghost";

export function Button({
  variant = "default",
  className,
  ...props
}: ComponentProps<"button"> & { variant?: ButtonVariant }) {
  const styles: Record<ButtonVariant, string> = {
    default:
      "border-line bg-panel text-ink-2 hover:border-accent hover:text-accent",
    primary:
      "border-accent bg-accent text-on-accent hover:bg-accent-dim hover:border-accent-dim",
    danger:
      "border-crit/45 bg-crit/10 text-crit hover:border-crit",
    ghost:
      "border-transparent text-ink-4 hover:text-ink",
  };
  return (
    <button
      type="button"
      {...props}
      className={cx(
        "inline-flex h-7 shrink-0 items-center justify-center gap-1.5 rounded-md border px-2.5",
        "font-mono text-xs tracking-[.04em]",
        "transition-colors disabled:pointer-events-none disabled:opacity-40",
        styles[variant],
        className,
      )}
    />
  );
}

export function Input({ className, ...props }: ComponentProps<"input">) {
  return (
    <input
      {...props}
      className={cx(
        "h-7 min-w-0 rounded-md border border-line bg-sunk px-2 font-mono text-xs text-ink",
        "placeholder:text-ink-4 focus:border-accent focus:outline-none",
        className,
      )}
    />
  );
}

export function Select({ className, children, ...props }: ComponentProps<"select">) {
  return (
    <select
      {...props}
      className={cx(
        "h-7 min-w-0 cursor-pointer rounded-md border border-line bg-sunk px-2 font-mono text-xs text-ink",
        "focus:border-accent focus:outline-none",
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
}: {
  value: T;
  options: { value: T; label: ReactNode; title?: string }[];
  onChange: (value: T) => void;
  className?: string;
}) {
  return (
    <div
      role="tablist"
      className={cx(
        // `w-fit` so it hugs its options in a column layout too, where the
        // default cross-axis stretch would otherwise pull it full-width.
        "inline-flex h-7 w-fit shrink-0 items-center gap-0.5 rounded-md border border-line bg-panel p-[3px]",
        className,
      )}
    >
      {options.map((option) => (
        <button
          key={option.value}
          role="tab"
          aria-selected={option.value === value}
          title={option.title}
          onClick={() => onChange(option.value)}
          className={cx(
            "h-[21px] rounded-sm px-2.5 font-mono text-2xs tracking-[.06em] transition-colors",
            option.value === value
              ? "bg-accent text-on-accent"
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
}: {
  value: T;
  options: { value: T; label: ReactNode; count?: number }[];
  onChange: (value: T) => void;
}) {
  return (
    <div role="tablist" className="flex items-center gap-4 border-b border-line px-3">
      {options.map((option) => (
        <button
          key={option.value}
          role="tab"
          aria-selected={option.value === value}
          onClick={() => onChange(option.value)}
          className={cx(
            "-mb-px flex h-8 items-center gap-1.5 border-b-2 font-mono text-2xs uppercase tracking-[.1em] transition-colors",
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
        "inline-flex h-[18px] shrink-0 items-center rounded-sm border bg-panel px-1.5 font-mono text-2xs tracking-[.05em] whitespace-nowrap",
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
 * A labelled figure. The value uses tabular figures so a column of these lines
 * up, and the label sits *under* the number — a reader scanning a strip of
 * these is looking for the digits, not the words.
 */
export function Stat({
  label,
  value,
  tone,
  hint,
}: {
  label: string;
  value: ReactNode;
  tone?: string;
  hint?: string;
}) {
  return (
    <div className="min-w-0" title={hint}>
      <div
        className="num text-[23px] leading-none font-medium tracking-[-.03em]"
        style={{ color: tone ?? "var(--color-ink)" }}
      >
        {value}
      </div>
      <div className="label mt-1.5 truncate">{label}</div>
    </div>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex h-[17px] min-w-[17px] items-center justify-center rounded-[3px] border border-line-strong bg-page px-1 text-2xs text-ink-4">
      {children}
    </kbd>
  );
}

/** Thin separator for inline metadata runs. */
export function Sep() {
  return <span className="text-ink-4/50">·</span>;
}
