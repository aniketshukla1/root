import { useMemo, useRef, useState } from "react";

import { healthColor, healthLabel, markColor, type Catalog } from "../catalog";
import type { Entity, Issue } from "../model";
import { HEALTH_ORDER } from "../types";
import { cx, ServiceMark } from "./primitives";

/**
 * Every monitored thing in the estate, one cell each.
 *
 * This is the fastest read in the product and it is deliberately not a chart.
 * At a glance you count what is broken; you do not interpret an axis. That
 * matters most exactly when the estate is large — three hundred containers
 * fit in the space one line chart would take, and the red ones find your eye
 * without being sorted for.
 *
 * The interaction follows the same principle. Hovering names the thing.
 * Clicking a healthy one takes you straight to it, because there is nothing to
 * explain; clicking a failing one opens the summary first, because that is the
 * question you actually had.
 *
 * Each group is drawn as its own recessed block. That is not decoration: the
 * blocks used to flow as bare cell grids separated by whitespace, and two
 * neighbouring sets of containers — same colour, same 14px square, a gap
 * between them barely wider than the gap inside them — read as one grid. A
 * count you cannot bound is a count you cannot trust, so the boundary is
 * drawn rather than implied.
 */
export interface HeatmapGroup {
  key: string;
  profileId: string;
  profileName: string;
  kind: string;
  /** The section label — Nodes, Workers, Containers, Indices. */
  section: string;
  entities: Entity[];
  down: number;
  degraded: number;
  unknown: number;
}

/** Grouped by connection and then by what kind of part it is. */
export function groupEntities(entities: Entity[]): HeatmapGroup[] {
  const groups = new Map<string, HeatmapGroup>();

  for (const entity of entities) {
    const key = `${entity.profileId}:${entity.section}`;
    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        profileId: entity.profileId,
        profileName: entity.profileName,
        kind: entity.kind,
        section: entity.section,
        entities: [],
        down: 0,
        degraded: 0,
        unknown: 0,
      };
      groups.set(key, group);
    }
    group.entities.push(entity);
    if (entity.health === "down") group.down += 1;
    else if (entity.health === "degraded") group.degraded += 1;
    else if (entity.health === "unknown") group.unknown += 1;
  }

  for (const group of groups.values()) {
    // Worst first, so a failure sits at the top-left of its block rather than
    // somewhere in the middle of two hundred green cells.
    group.entities.sort(
      (a, b) =>
        HEALTH_ORDER.indexOf(a.health) - HEALTH_ORDER.indexOf(b.health) ||
        a.name.localeCompare(b.name, undefined, { numeric: true }),
    );
  }

  return [...groups.values()].sort(
    (a, b) =>
      b.down - a.down ||
      b.degraded - a.degraded ||
      b.unknown - a.unknown ||
      b.entities.length - a.entities.length ||
      a.profileName.localeCompare(b.profileName),
  );
}

/** Cells beyond this in one block stop helping and start costing frames. */
const CELL_CAP = 480;

/**
 * A block has to be wide enough for its own caption, not merely for its cells:
 * a group of one is still labelled `demo-cache-healthy / Slow commands`. This
 * is what that caption needs at 10px mono, and it happens to hold nine cells
 * to a row, which is a fair shape for the small blocks that make up most of
 * an estate.
 */
const MIN_BLOCK = 184;

/**
 * Past this many cells a block earns a second track. Left in one it becomes a
 * narrow column twenty rows deep, which is the one shape this display is bad
 * at — you can no longer take the whole group in at a glance, which is the
 * entire point of drawing it as squares.
 */
const WIDE_BLOCK_AT = 24;

export function Heatmap({
  entities,
  issues,
  catalog,
  onSelect,
}: {
  entities: Entity[];
  issues: Issue[];
  catalog: Catalog;
  /** The issue is present only when this cell has something wrong with it. */
  onSelect: (entity: Entity, issue?: Issue) => void;
}) {
  const groups = useMemo(() => groupEntities(entities), [entities]);
  const issueBy = useMemo(() => {
    const map = new Map<string, Issue>();
    for (const issue of issues) map.set(issue.id, issue);
    return map;
  }, [issues]);

  const [hover, setHover] = useState<{ entity: Entity; x: number; y: number } | null>(null);

  return (
    <div className="relative">
      {/* Equal tracks rather than a wrapping row. Blocks used to be as wide as
          their own caption, so a long connection name pushed its neighbour
          half a screen away while a short one sat flush against it — the
          spacing encoded the length of a label and nothing else. */}
      <div
        className="grid items-start gap-2"
        style={{ gridTemplateColumns: `repeat(auto-fill, minmax(${MIN_BLOCK}px, 1fr))` }}
      >
        {groups.map((group) => (
          <Block
            key={group.key}
            group={group}
            catalog={catalog}
            onHover={setHover}
            onLeave={() => setHover(null)}
            onSelect={(entity) => onSelect(entity, issueBy.get(entity.id))}
          />
        ))}
      </div>

      {hover && <Tip entity={hover.entity} x={hover.x} y={hover.y} />}
    </div>
  );
}

/**
 * One connection's parts, and one stop on the way through the page.
 *
 * Every cell used to be its own tab stop, so reaching the alerts under a
 * 364-component estate meant 364 presses of Tab. A block is one stop and the
 * arrow keys move inside it, which is how a grid of cells is meant to be
 * walked — the same shape a spreadsheet or a calendar uses.
 */
function Block({
  group,
  catalog,
  onHover,
  onLeave,
  onSelect,
}: {
  group: HeatmapGroup;
  catalog: Catalog;
  onHover: (hover: { entity: Entity; x: number; y: number }) => void;
  onLeave: () => void;
  onSelect: (entity: Entity) => void;
}) {
  const cells = group.entities.slice(0, CELL_CAP);
  // The cell the block hands focus to when it is tabbed into. It starts at the
  // first one, which sorting has already made the worst one.
  const [cursor, setCursor] = useState(0);
  const grid = useRef<HTMLDivElement>(null);

  const focusCell = (index: number) => {
    const next = Math.max(0, Math.min(cells.length - 1, index));
    setCursor(next);
    const node = grid.current?.querySelectorAll<HTMLButtonElement>("button")[next];
    node?.focus();
  };

  /** Cells wrap, so "down" is however many fit on a row right now. */
  const perRow = () => {
    const buttons = grid.current?.querySelectorAll<HTMLButtonElement>("button");
    if (!buttons?.length) return 1;
    const top = buttons[0].offsetTop;
    let n = 0;
    for (const button of buttons) {
      if (button.offsetTop !== top) break;
      n += 1;
    }
    return Math.max(1, n);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const keys = ["ArrowRight", "ArrowLeft", "ArrowDown", "ArrowUp", "Home", "End"];
    if (!keys.includes(e.key)) return;
    e.preventDefault();
    if (e.key === "ArrowRight") focusCell(cursor + 1);
    else if (e.key === "ArrowLeft") focusCell(cursor - 1);
    else if (e.key === "ArrowDown") focusCell(cursor + perRow());
    else if (e.key === "ArrowUp") focusCell(cursor - perRow());
    else if (e.key === "Home") focusCell(0);
    else focusCell(cells.length - 1);
  };

  return (
    <div
      className={cx(
        "min-w-0 rounded-sm border border-line-soft bg-sunk",
        // Only once the container is wide enough that a second track
        // certainly exists; spanning into a track that isn't there
        // grows the grid instead of the block.
        group.entities.length > WIDE_BLOCK_AT && "@3xl:col-span-2",
      )}
    >
      <div className="border-b border-line-soft px-2 py-1.5">
        <div className="flex items-center gap-1.5">
          <ServiceMark descriptor={catalog.get(group.kind)} size={14} />
          <span className="num truncate text-2xs text-ink-2">{group.profileName}</span>
          <span className="num ml-auto shrink-0 text-2xs text-ink-4">
            {group.entities.length}
          </span>
        </div>
        <div className="mt-0.5 flex items-center gap-1.5">
          <span className="num truncate text-2xs text-ink-4">{group.section}</span>
          {(group.down > 0 || group.degraded > 0) && (
            <span className="num ml-auto shrink-0 text-2xs">
              {group.down > 0 && <span className="text-crit">{group.down} down</span>}
              {group.down > 0 && group.degraded > 0 && <span className="text-ink-4"> · </span>}
              {group.degraded > 0 && <span className="text-warn">{group.degraded} degraded</span>}
            </span>
          )}
        </div>
      </div>

      <div
        ref={grid}
        role="grid"
        aria-label={`${group.profileName} · ${group.section}`}
        className="flex flex-wrap gap-[3px] p-2"
        onMouseLeave={onLeave}
        onKeyDown={onKeyDown}
      >
        {cells.map((entity, i) => (
          <Cell
            key={entity.id}
            entity={entity}
            tabIndex={i === cursor ? 0 : -1}
            onHover={(x, y) => onHover({ entity, x, y })}
            onFocused={() => setCursor(i)}
            onClick={() => onSelect(entity)}
          />
        ))}
        {group.entities.length > CELL_CAP && (
          <span className="num self-center pl-1 text-2xs text-ink-4">
            +{group.entities.length - CELL_CAP}
          </span>
        )}
      </div>
    </div>
  );
}

function Cell({
  entity,
  tabIndex,
  onHover,
  onFocused,
  onClick,
}: {
  entity: Entity;
  tabIndex: number;
  onHover: (x: number, y: number) => void;
  onFocused: () => void;
  onClick: () => void;
}) {
  const color = healthColor(entity.health);
  const healthy = entity.health === "healthy";

  return (
    <button
      onClick={onClick}
      onMouseEnter={(e) => {
        const r = e.currentTarget.getBoundingClientRect();
        onHover(r.left + r.width / 2, r.top);
      }}
      onFocus={(e) => {
        const r = e.currentTarget.getBoundingClientRect();
        onHover(r.left + r.width / 2, r.top);
        onFocused();
      }}
      tabIndex={tabIndex}
      aria-label={`${entity.name} — ${healthLabel(entity.health)}`}
      className={cx(
        "h-[14px] w-[14px] shrink-0 rounded-xs transition-transform",
        "hover:z-10 hover:scale-[1.35] focus-visible:z-10 focus-visible:scale-[1.35]",
      )}
      style={
        healthy
          ? {
              // Mixed against the block's own ground, not the panel's. A
              // healthy cell is meant to be a tint of the surface it sits on;
              // against the wrong surface it reads as a faint lump instead.
              //
              // 12% fill / 45% ring, down from 26 / 55. At the old values 357
              // healthy cells were the largest area of colour on Overview, and
              // the seven that were actually wrong had to compete with them —
              // a display whose whole argument is that the red ones find your
              // eye. The ring still measures 3.10:1 against the block ground,
              // above the 3:1 floor for a mark carrying meaning, so quiet did
              // not become invisible. Re-measure if either number moves.
              background: `color-mix(in srgb, ${color} 12%, var(--color-sunk))`,
              boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${color} 45%, transparent)`,
            }
          : { background: color, boxShadow: `inset 0 0 0 1px ${color}` }
      }
    />
  );
}

/**
 * Fixed to the viewport rather than the block, so a cell near the right edge
 * still gets a readable label instead of one clipped by its own panel.
 */
function Tip({ entity, x, y }: { entity: Entity; x: number; y: number }) {
  return (
    <div
      className="pointer-events-none fixed z-50 -translate-x-1/2 -translate-y-full pb-2"
      style={{ left: x, top: y }}
    >
      <div className="panel max-w-64 px-2.5 py-1.5 overlay-plate">
        <div className="flex items-center gap-1.5">
          <span
            className="h-2 w-2 shrink-0 rounded-xs"
            style={{ background: healthColor(entity.health) }}
          />
          <span className="num truncate text-2xs text-ink">{entity.name}</span>
        </div>
        <div className="num mt-1 text-2xs text-ink-4">
          {entity.profileName} · {entity.section}
        </div>
        {entity.summary && (
          <div className="num mt-1 line-clamp-2 text-2xs text-ink-3">{entity.summary}</div>
        )}
      </div>
    </div>
  );
}

export { markColor };
