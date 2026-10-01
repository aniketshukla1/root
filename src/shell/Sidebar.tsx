import type { ComponentType } from "react";

import { healthColor, healthLabel } from "../catalog";
import { initials } from "../model";
import type { EstateModel } from "../model";
import { rootOf, type View } from "../nav";
import type { TeamState } from "../types";
import { cx, Dot, Kbd, RootMark } from "../ui/primitives";
import {
  AlertsIcon,
  AssistantIcon,
  ScorecardIcon,
  ChevronLeft,
  LogsIcon,
  MetricsIcon,
  OverviewIcon,
  SearchIcon,
  ServicesIcon,
  SettingsIcon,
} from "../ui/icons";

interface Entry {
  key: string;
  label: string;
  icon: ComponentType<{ size?: number }>;
  view: View;
  count?: number;
  /** Renders the count in the warning hue when something is unhandled. */
  alarm?: boolean;
}

export function Sidebar({
  view,
  model,
  team,
  collapsed,
  markHidden = false,
  onNavigate,
  onToggleCollapsed,
  onOpenPalette,
  onOpenWorkspace,
}: {
  view: View;
  model: EstateModel;
  /** Null until the first read; local mode is `{ hasSession: false }`. */
  team: TeamState | null;
  collapsed: boolean;
  /** While the splash's mark is flying to this one's place. */
  markHidden?: boolean;
  onNavigate: (view: View) => void;
  onToggleCollapsed: () => void;
  onOpenPalette: () => void;
  onOpenWorkspace: () => void;
}) {
  const current = rootOf(view);

  const primary: Entry[] = [
    { key: "overview", label: "Overview", icon: OverviewIcon, view: { name: "overview" } },
    {
      key: "services",
      label: "Services",
      icon: ServicesIcon,
      view: { name: "services" },
      count: model.counts.connections,
    },
    {
      key: "alerts",
      label: "Alerts",
      icon: AlertsIcon,
      view: { name: "alerts" },
      // The same number the Alerts screen opens on — its default filter is
      // Open, which is the unacknowledged set. Counting every active alert
      // here put a 5 beside a screen that then showed 3.
      count: model.counts.unacknowledged,
      alarm: model.counts.unacknowledged > 0,
    },
    { key: "metrics", label: "Metrics", icon: MetricsIcon, view: { name: "metrics" } },
    { key: "logs", label: "Logs", icon: LogsIcon, view: { name: "logs" } },
    { key: "assistant", label: "Assistant", icon: AssistantIcon, view: { name: "assistant" } },
    // Last in the list rather than promoted: it is how the tool is doing, not
    // how the estate is doing, and the estate is what somebody opened this for.
    { key: "scorecard", label: "Scorecard", icon: ScorecardIcon, view: { name: "scorecard" } },
  ];

  const item = (entry: Entry) => {
    const active = current === entry.key;
    const Glyph = entry.icon;
    return (
      <button
        key={entry.key}
        onClick={() => onNavigate(entry.view)}
        title={collapsed ? entry.label : undefined}
        aria-current={active ? "page" : undefined}
        className={cx(
          "group relative flex h-[30px] items-center rounded-md text-base transition-colors",
          collapsed ? "w-8 justify-center" : "w-full gap-2.5 px-2.5",
          active ? "bg-sunk text-ink" : "text-ink-2 hover:bg-sunk/60 hover:text-ink",
        )}
      >
        {/* Current-page marker that does not rely on the fill alone. */}
        {active && (
          <span className="absolute top-2 -left-2.5 h-[14px] w-[2px] bg-accent" />
        )}
        <span className={active ? "flex text-accent" : "flex"}>
          <Glyph size={15} />
        </span>
        {!collapsed && (
          <>
            <span className="truncate">{entry.label}</span>
            {entry.count !== undefined && entry.count > 0 && (
              <span
                className={cx(
                  "num ml-auto text-2xs",
                  entry.alarm ? "text-warn" : "text-ink-4",
                )}
              >
                {entry.count}
              </span>
            )}
          </>
        )}
      </button>
    );
  };

  return (
    <nav
      className={cx(
        // `overflow-hidden` matters during the width transition: the labels
        // switch instantly while the rail is still animating, so without it the
        // expanded content spills past the narrow rail for 150ms.
        "flex shrink-0 flex-col gap-1 overflow-hidden border-r border-line bg-panel py-3 transition-[width] duration-150",
        collapsed ? "w-13 items-center px-2.5" : "w-52 px-3",
      )}
      style={{ width: collapsed ? 52 : 208 }}
    >
      {/* The toggle lives here in BOTH states. Moving it to the bottom rail
          when collapsed meant the control you just clicked was no longer where
          you left it, so undoing the action required hunting for it. Collapsed,
          it also carries the health dot, which would otherwise have no room. */}
      <div
        className={cx(
          "mb-2 flex h-8 shrink-0 items-center",
          collapsed ? "justify-center" : "gap-2 px-1",
        )}
      >
        {!collapsed && (
          <>
            {/* Where the splash's mark lands when it leaves. Hidden until it
                has, then shown in its place — one mark, not two. */}
            <span
              data-splash-target
              className="grid h-[26px] w-[26px] shrink-0 place-items-center rounded-sm bg-accent text-on-accent transition-opacity duration-150"
              style={{ opacity: markHidden ? 0 : 1 }}
            >
              <RootMark size={16} />
            </span>
            <span className="font-mono text-xs tracking-[.18em] text-ink uppercase">root</span>
            <span
              className="ml-auto h-1.5 w-1.5 shrink-0"
              style={{ background: healthColor(model.health) }}
              title={`All connections: ${healthLabel(model.health)}`}
            />
          </>
        )}
        <button
          onClick={onToggleCollapsed}
          title={
            collapsed
              ? `Expand sidebar — all connections: ${healthLabel(model.health)}`
              : "Collapse sidebar"
          }
          aria-expanded={!collapsed}
          className={cx(
            "flex h-7 items-center justify-center rounded-md text-ink-4 transition-colors hover:bg-raised hover:text-ink",
            collapsed ? "w-8 gap-1" : "ml-auto w-6",
          )}
        >
          {collapsed && <Dot health={model.health} size={7} />}
          <ChevronLeft size={13} className={collapsed ? "rotate-180" : undefined} />
        </button>
      </div>

      {/* Present in both states for the same reason as the toggle: collapsing
          should narrow the rail, not remove controls from it. */}
      <button
        onClick={onOpenPalette}
        title={collapsed ? "Search  ⌘K" : undefined}
        className={cx(
          "mb-2 flex h-7 shrink-0 items-center rounded-md border border-line-strong bg-page text-xs text-ink-4 transition-colors hover:text-ink",
          collapsed ? "w-8 justify-center self-center" : "w-full gap-2 px-2",
        )}
      >
        <SearchIcon size={13} />
        {!collapsed && (
          <>
            <span>Search</span>
            <Kbd>⌘K</Kbd>
          </>
        )}
      </button>

      {primary.map(item)}

      <div className="mt-auto flex flex-col gap-1 pt-2">
        {item({
          key: "settings",
          label: "Settings",
          icon: SettingsIcon,
          view: { name: "settings" },
        })}
        {!collapsed && (
          // Whatever is actually true. This block used to read "Acme Platform
          // / aniket · admin" on every install, which is a stranger's name on
          // your own machine — and it asserted a workspace and a role for a
          // product that, by default, has neither.
          <button
            className="mt-2.5 flex w-full items-center gap-2.5 border-t border-line px-1.5 pt-2.5 text-left transition-colors hover:text-ink"
            title={
              team?.account
                ? `${team.account.email} on ${team.serverUrl}`
                : "root is running locally — open Settings to join a workspace"
            }
            onClick={onOpenWorkspace}
          >
            <span
              className="grid h-6 w-6 shrink-0 place-items-center rounded-sm font-mono text-2xs font-semibold"
              style={{
                color: team?.account ? "var(--color-series-6)" : "var(--color-ink-4)",
                boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${
                  team?.account ? "var(--color-series-6)" : "var(--color-ink-4)"
                } 45%, transparent)`,
                background: team?.account
                  ? "color-mix(in srgb, var(--color-series-6) 15%, transparent)"
                  : "transparent",
              }}
            >
              {team?.account ? initials(team.account.name) : <RootMark size={13} />}
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-xs text-ink">
                {team?.account?.name ?? "Local"}
              </span>
              <span className="num block truncate text-2xs text-ink-4">
                {team?.account
                  ? `${hostOf(team.serverUrl)} · ${team.account.role}`
                  : "this machine only"}
              </span>
            </span>
            <ChevronLeft size={12} className="-rotate-90 text-ink-4" />
          </button>
        )}
      </div>
    </nav>
  );
}

/** Just the host, so a long URL does not push the role out of the sidebar. */
function hostOf(url?: string): string {
  if (!url) return "workspace";
  try {
    return new URL(url).host;
  } catch {
    return url.replace(/^https?:\/\//, "");
  }
}
