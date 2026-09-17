import { useEffect, useMemo, useRef, useState } from "react";

import type { Catalog } from "../catalog";
import { markColor } from "../catalog";
import { formatDateTime } from "../format";
import type { EstateModel, ServiceView } from "../model";
import type { View } from "../nav";
import type { LogEvent, LogLevel, LogStream } from "../types";
import { LOG_LEVELS } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { LogHistogram, levelColor, type Bucket } from "../ui/charts";
import { Button, Chip, cx, Empty, Input, Panel, Segmented, ServiceMark, Tag } from "../ui/primitives";
import { AssistantIcon, SearchIcon, TimelineIcon } from "../ui/icons";

/** Rendering the whole buffer would jank the scroll; this is well past a screen. */
const MERGED_CAP = 600;
const LANE_CAP = 150;

const STREAM_FILTERS: { value: string; label: string; streams: LogStream[] | null }[] = [
  { value: "default", label: "Runtime + events", streams: ["runtime", "event"] },
  { value: "runtime", label: "Runtime", streams: ["runtime"] },
  { value: "event", label: "Events", streams: ["event"] },
  { value: "build", label: "Build output", streams: ["build"] },
  { value: "all", label: "Everything", streams: null },
];

const LEVEL_RANK: Record<LogLevel, number> = {
  trace: 0, debug: 1, info: 2, warn: 3, error: 4, fatal: 5,
};

/**
 * `merged` is the unified stream — the thing that makes a Postgres lock wait
 * and an Nginx 502 legible as one event. `split` gives every connection its own
 * lane for when you are reading one system rather than correlating several.
 * Both are useful; neither replaces the other.
 */
type Mode = "merged" | "split";

export function Logs({
  logs,
  buckets,
  model,
  catalog,
  minutes,
  onMinutesChange,
  onClear,
  onNavigate,
  onAsk,
  onExplain,
}: {
  logs: LogEvent[];
  buckets: Bucket[];
  model: EstateModel;
  catalog: Catalog;
  minutes: number;
  onMinutesChange: (minutes: number) => void;
  onClear: () => void;
  onNavigate: (view: View) => void;
  onAsk: () => void;
  onExplain: (event: LogEvent) => void;
}) {
  const [mode, setMode] = useState<Mode>("merged");
  const [stream, setStream] = useState("default");
  const [query, setQuery] = useState("");
  // Empty means every connection — so a newly added connection appears without
  // the user having to opt it in.
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [levels, setLevels] = useState<Set<LogLevel>>(new Set());
  const [follow, setFollow] = useState(true);
  const listRef = useRef<HTMLDivElement>(null);

  const streamFilter = STREAM_FILTERS.find((s) => s.value === stream)?.streams ?? null;

  /** Stream + search + level, but *not* connection — the rail counts need this. */
  const preScope = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return logs.filter((event) => {
      if (streamFilter && !streamFilter.includes(event.stream)) return false;
      if (levels.size > 0 && !levels.has(event.level)) return false;
      if (!needle) return true;
      return (
        event.message.toLowerCase().includes(needle) ||
        event.source.toLowerCase().includes(needle) ||
        event.profileName.toLowerCase().includes(needle)
      );
    });
  }, [logs, streamFilter, levels, query]);

  const perConnection = useMemo(() => {
    const counts = new Map<string, { total: number; errors: number }>();
    for (const event of preScope) {
      const entry = counts.get(event.profileId) ?? { total: 0, errors: 0 };
      entry.total += 1;
      if (LEVEL_RANK[event.level] >= 4) entry.errors += 1;
      counts.set(event.profileId, entry);
    }
    return counts;
  }, [preScope]);

  // Facets ignore the level filter itself, so a facet never reports zero for
  // the thing you just turned on.
  const facets = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const counts = Object.fromEntries(LOG_LEVELS.map((l) => [l, 0])) as Record<LogLevel, number>;
    for (const event of logs) {
      if (streamFilter && !streamFilter.includes(event.stream)) continue;
      if (selected.size > 0 && !selected.has(event.profileId)) continue;
      if (
        needle &&
        !event.message.toLowerCase().includes(needle) &&
        !event.source.toLowerCase().includes(needle) &&
        !event.profileName.toLowerCase().includes(needle)
      ) {
        continue;
      }
      counts[event.level] += 1;
    }
    return counts;
  }, [logs, streamFilter, selected, query]);

  const visible = useMemo(
    () =>
      selected.size === 0
        ? preScope
        : preScope.filter((event) => selected.has(event.profileId)),
    [preScope, selected],
  );

  const lanes = useMemo(() => {
    const byProfile = new Map<string, LogEvent[]>();
    for (const event of visible) {
      byProfile.set(event.profileId, [...(byProfile.get(event.profileId) ?? []), event]);
    }
    return model.services
      .map((service) => ({
        service,
        events: byProfile.get(service.snapshot.profileId) ?? [],
      }))
      .filter((lane) => lane.events.length > 0)
      .sort((a, b) => b.events.length - a.events.length);
  }, [visible, model.services]);

  useEffect(() => {
    if (follow && mode === "merged" && listRef.current) listRef.current.scrollTop = 0;
  }, [visible, follow, mode]);

  const toggleLevel = (level: LogLevel) =>
    setLevels((current) => {
      const next = new Set(current);
      if (next.has(level)) next.delete(level);
      else next.add(level);
      return next;
    });

  const toggleConnection = (profileId: string) =>
    setSelected((current) => {
      // The first click on a rail row narrows to that one connection rather
      // than removing it — "show me this" is overwhelmingly the intent.
      if (current.size === 0) return new Set([profileId]);
      const next = new Set(current);
      if (next.has(profileId)) next.delete(profileId);
      else next.add(profileId);
      return next;
    });

  return (
    <>
      <ScreenHeader
        onAsk={onAsk}
        title="Logs"
        meta={
          <>
            <span className="num">{visible.length}</span> of
            <span className="num">{logs.length}</span> buffered lines
            {selected.size > 0 && (
              <>
                <span className="text-ink-4/50">·</span>
                <span className="num">{selected.size}</span> of
                <span className="num">{model.services.length}</span> connections
              </>
            )}
          </>
        }
      >
        <Segmented
          label="View"
          value={mode}
          onChange={setMode}
          options={[
            { value: "merged", label: "Merged", title: "One timeline across every connection" },
            { value: "split", label: "Split", title: "A separate lane per connection" },
          ]}
        />
        <Segmented
          label="Time range"
          value={String(minutes)}
          onChange={(v) => onMinutesChange(Number(v))}
          options={[
            { value: "15", label: "15m" },
            { value: "60", label: "1h" },
            { value: "360", label: "6h" },
            { value: "1440", label: "24h" },
          ]}
        />
        <Button onClick={onClear} title="Empties the in-memory buffer on both sides">
          Clear buffer
        </Button>
      </ScreenHeader>

      <Screen scroll={false} className="flex flex-col gap-3">
        <Panel
          title="Activity"
          subtitle="stacked by severity"
          className="shrink-0"
          bodyClassName="pb-2"
        >
          <LogHistogram buckets={buckets} height={72} />
          <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
            {LOG_LEVELS.map((level) => (
              <span key={level} className="flex items-center gap-1 text-2xs text-ink-4 capitalize">
                <span
                  className="h-1.5 w-1.5 shrink-0 rounded-full"
                  style={{ background: levelColor(level) }}
                />
                {level}
              </span>
            ))}
          </div>
        </Panel>

        <Panel flush className="min-h-0 flex-1" bodyClassName="flex min-h-0 flex-col">
          <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-line px-3 py-2">
            <div className="relative">
              <SearchIcon
                size={13}
                className="pointer-events-none absolute top-1/2 left-2 -translate-y-1/2 text-ink-4"
              />
              <Input
                className="w-60 pl-7"
                placeholder="Search messages, sources, connections…"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </div>

            <Segmented
              label="Stream"
              value={stream}
              onChange={setStream}
              options={STREAM_FILTERS.map((s) => ({ value: s.value, label: s.label }))}
            />

            <div className="flex items-center gap-1">
              {LOG_LEVELS.map((level) => {
                const on = levels.has(level);
                const count = facets[level];
                return (
                  <Chip
                    key={level}
                    quiet
                    selected={on}
                    onClick={() => toggleLevel(level)}
                    disabled={count === 0 && !on}
                    title={`${count} ${level} line${count === 1 ? "" : "s"}`}
                    className="rounded-md px-1.5"
                  >
                    <span
                      className="h-1.5 w-1.5 shrink-0 rounded-full"
                      style={{ background: levelColor(level) }}
                    />
                    <span className="capitalize">{level}</span>
                    <span className="num text-ink-4">{count}</span>
                  </Chip>
                );
              })}
            </div>

            {mode === "merged" && (
              <label className="ml-auto flex cursor-pointer items-center gap-1.5 text-xs text-ink-3">
                <input
                  type="checkbox"
                  checked={follow}
                  onChange={(e) => setFollow(e.target.checked)}
                  className="accent-accent"
                />
                Follow tail
              </label>
            )}
          </div>

          <div className="flex min-h-0 flex-1">
            <ConnectionRail
              model={model}
              catalog={catalog}
              counts={perConnection}
              selected={selected}
              onToggle={toggleConnection}
              onAll={() => setSelected(new Set())}
              onOnly={(profileId) => setSelected(new Set([profileId]))}
            />

            <div
              ref={listRef}
              className="min-w-0 flex-1 overflow-x-auto overflow-y-auto"
            >
              {visible.length === 0 ? (
                <Empty
                  title="No lines match"
                  hint={
                    logs.length === 0
                      ? "Nothing has been collected yet. Lines appear as connections poll."
                      : "Widen the search, connection, stream, or severity filters."
                  }
                />
              ) : mode === "merged" ? (
                <LineTable
                  events={visible.slice(0, MERGED_CAP)}
                  catalog={catalog}
                  showConnection
                  onExplain={onExplain}
                  onNavigate={onNavigate}
                />
              ) : (
                <div className="flex flex-col gap-3 p-3">
                  {lanes.map(({ service, events }) => (
                    <Lane
                      key={service.snapshot.profileId}
                      service={service}
                      events={events}
                      catalog={catalog}
                      onNavigate={onNavigate}
                      onExplain={onExplain}
                    />
                  ))}
                </div>
              )}

              {mode === "merged" && visible.length > MERGED_CAP && (
                <p className="border-t border-line px-3 py-2 text-2xs text-ink-4">
                  Showing the newest {MERGED_CAP} of {visible.length} matching lines. Narrow the
                  filters to reach older ones.
                </p>
              )}
            </div>
          </div>
        </Panel>
      </Screen>
    </>
  );
}

// ------------------------------------------------------------------ the rail

function ConnectionRail({
  model,
  catalog,
  counts,
  selected,
  onToggle,
  onAll,
  onOnly,
}: {
  model: EstateModel;
  catalog: Catalog;
  counts: Map<string, { total: number; errors: number }>;
  selected: Set<string>;
  onToggle: (profileId: string) => void;
  onAll: () => void;
  onOnly: (profileId: string) => void;
}) {
  const all = selected.size === 0;

  const ordered = [...model.services].sort((a, b) => {
    const left = counts.get(a.snapshot.profileId) ?? { total: 0, errors: 0 };
    const right = counts.get(b.snapshot.profileId) ?? { total: 0, errors: 0 };
    return (
      right.errors - left.errors ||
      right.total - left.total ||
      a.snapshot.profileName.localeCompare(b.snapshot.profileName)
    );
  });

  return (
    <aside className="flex w-52 shrink-0 flex-col border-r border-line">
      <div className="flex h-7 shrink-0 items-center gap-1 border-b border-line px-2">
        <span className="label">Connections</span>
        {!all && (
          <button
            onClick={onAll}
            className="ml-auto flex h-6 items-center px-1 text-2xs text-accent transition-colors hover:underline"
          >
            Show all
          </button>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto py-1">
        {ordered.map((service) => {
          const id = service.snapshot.profileId;
          const count = counts.get(id) ?? { total: 0, errors: 0 };
          const on = all || selected.has(id);
          const descriptor = catalog.get(service.snapshot.kind);

          return (
            <div
              key={id}
              className={cx(
                "group flex h-7 items-center gap-1.5 border-l-2 pr-2 pl-1.5 transition-colors",
                on ? "text-ink-2" : "text-ink-4",
              )}
              style={{
                borderLeftColor: on ? markColor(descriptor.accent) : "transparent",
                background: on && !all ? "var(--color-wash-strong)" : undefined,
              }}
            >
              <button
                onClick={() => onToggle(id)}
                className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                title={
                  all
                    ? `Show only ${service.snapshot.profileName}`
                    : on
                      ? `Hide ${service.snapshot.profileName}`
                      : `Add ${service.snapshot.profileName}`
                }
              >
                <ServiceMark descriptor={descriptor} size={14} />
                <span className="min-w-0 truncate text-xs">{service.snapshot.profileName}</span>
              </button>

              {count.errors > 0 && (
                <span className="num shrink-0 text-2xs text-serious" title={`${count.errors} at error or above`}>
                  {count.errors}
                </span>
              )}
              <span className="num shrink-0 text-2xs text-ink-4">{count.total}</span>

              {!all && (
                <button
                  onClick={() => onOnly(id)}
                  title={`Show only ${service.snapshot.profileName}`}
                  className="shrink-0 text-2xs text-ink-4 opacity-0 transition-opacity group-hover:opacity-100 hover:text-accent focus-visible:opacity-100"
                >
                  only
                </button>
              )}
            </div>
          );
        })}
      </div>
    </aside>
  );
}

// ------------------------------------------------------------------- a lane

function Lane({
  service,
  events,
  catalog,
  onNavigate,
  onExplain,
}: {
  service: ServiceView;
  events: LogEvent[];
  catalog: Catalog;
  onNavigate: (view: View) => void;
  onExplain?: (event: LogEvent) => void;
}) {
  const descriptor = catalog.get(service.snapshot.kind);
  const errors = events.filter((e) => LEVEL_RANK[e.level] >= 4).length;

  return (
    <section
      className="overflow-hidden rounded-md border border-line border-l-2"
      style={{ borderLeftColor: markColor(descriptor.accent) }}
    >
      <header className="flex h-7 items-center gap-2 border-b border-line bg-page/50 px-2.5">
        <ServiceMark descriptor={descriptor} size={14} />
        <button
          onClick={() =>
            onNavigate({ name: "service", profileId: service.snapshot.profileId, tab: "logs" })
          }
          className="text-xs font-medium text-ink transition-colors hover:text-accent"
        >
          {service.snapshot.profileName}
        </button>
        <span className="num text-2xs text-ink-4">{events.length}</span>
        {errors > 0 && <Tag color="var(--color-serious)">{errors} errors</Tag>}
        <span className="ml-auto text-2xs text-ink-4">{descriptor.label}</span>
      </header>

      <div className="max-h-60 overflow-x-auto overflow-y-auto">
        <LineTable
          events={events.slice(0, LANE_CAP)}
          catalog={catalog}
          showConnection={false}
          onExplain={onExplain}
          onNavigate={onNavigate}
        />
      </div>

      {events.length > LANE_CAP && (
        <p className="border-t border-line px-2.5 py-1 text-2xs text-ink-4">
          Newest {LANE_CAP} of {events.length}.
        </p>
      )}
    </section>
  );
}

// ------------------------------------------------------------- the line table

function LineTable({
  events,
  catalog,
  showConnection,
  onExplain,
  onNavigate,
}: {
  events: LogEvent[];
  catalog: Catalog;
  showConnection: boolean;
  onExplain?: (event: LogEvent) => void;
  onNavigate: (view: View) => void;
}) {
  return (
    // `min-w` is what stops the message column collapsing to a ribbon when the
    // assistant dock takes its width; the pane scrolls instead.
    <table className="w-full min-w-[640px] border-collapse">
      <tbody>
        {events.map((event) => (
          <tr
            key={event.id}
            className="group border-b border-line/40 align-baseline row-hover"
          >
            {/* A 2px stripe in the connection's identity colour. Colour is on
                the rule, never on the text — which stays neutral ink. */}
            {showConnection && (
              <td
                className="w-0.5 p-0"
                style={{ background: markColor(catalog.get(event.kind).accent) }}
              />
            )}
            <td className="w-px px-3 py-1 whitespace-nowrap">
              <button
                onClick={() => onNavigate({ name: "correlate", at: event.timestamp })}
                title={`${formatDateTime(event.timestamp)} — correlate across every connection`}
                className="num flex items-center gap-1 text-2xs text-ink-4 transition-colors hover:text-accent"
              >
                {new Date(event.timestamp).toLocaleTimeString([], { hour12: false })}
                <TimelineIcon
                  size={11}
                  className="opacity-0 transition-opacity group-hover:opacity-100"
                />
              </button>
            </td>
            <td className="w-px px-1 py-1">
              <span
                className="num text-2xs"
                style={{ color: levelColor(event.level) }}
                title={`Severity: ${event.level}`}
              >
                {event.level.toUpperCase()}
              </span>
            </td>
            {showConnection && (
              <td className="w-px max-w-32 px-2 py-1">
                <span
                  className="num block truncate text-2xs text-ink-3"
                  title={`${event.profileName} · ${catalog.label(event.kind)}`}
                >
                  {event.profileName}
                </span>
              </td>
            )}
            <td className="w-px max-w-32 px-2 py-1">
              <span className="num block truncate text-2xs text-ink-4" title={event.source}>
                {event.source}
              </span>
            </td>
            <td className="px-2 py-1">
              <span
                className={cx(
                  "num block break-all",
                  LEVEL_RANK[event.level] >= 4 ? "text-ink" : "text-ink-2",
                )}
              >
                {event.message}
              </span>
            </td>
            {event.stream !== "runtime" && (
              <td className="w-px px-2 py-1">
                <Tag>{event.stream}</Tag>
              </td>
            )}
            {/* Its own column: an action inlined next to the message competes
                with the thing you are trying to read. */}
            <td className="w-px px-2 py-1">
              {onExplain && (
                <Button
                  variant="ghost"
                  onClick={() => onExplain(event)}
                  title="Explain this line"
                  className="gap-1.5 px-1.5 text-accent opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
                >
                  <AssistantIcon size={11} />
                </Button>
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
