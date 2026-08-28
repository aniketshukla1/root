import { useMemo, useState } from "react";

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
      <div className="flex flex-wrap gap-x-6 gap-y-4">
        {groups.map((group) => (
          <div key={group.key} className="min-w-0">
            <div className="mb-2 flex items-center gap-2">
              <ServiceMark descriptor={catalog.get(group.kind)} size={14} />
              <span className="num truncate text-2xs text-ink-2">{group.profileName}</span>
              <span className="num text-2xs text-ink-4">{group.section}</span>
              <span className="num text-2xs text-ink-4">{group.entities.length}</span>
              {group.down > 0 && (
                <span className="num text-2xs text-crit">{group.down} down</span>
              )}
              {group.degraded > 0 && (
                <span className="num text-2xs text-warn">{group.degraded} degraded</span>
              )}
            </div>

            <div
              className="flex flex-wrap gap-[3px]"
              style={{ maxWidth: cellsWide(group.entities.length) * 17 }}
              onMouseLeave={() => setHover(null)}
            >
              {group.entities.slice(0, CELL_CAP).map((entity) => (
                <Cell
                  key={entity.id}
                  entity={entity}
                  onHover={(x, y) => setHover({ entity, x, y })}
                  onClick={() => onSelect(entity, issueBy.get(entity.id))}
                />
              ))}
              {group.entities.length > CELL_CAP && (
                <span className="num self-center pl-1 text-2xs text-ink-4">
                  +{group.entities.length - CELL_CAP}
                </span>
              )}
            </div>
          </div>
        ))}
      </div>

      {hover && <Tip entity={hover.entity} x={hover.x} y={hover.y} />}
    </div>
  );
}

/** Blocks stay squarish, so a group of 200 does not become one long ribbon. */
function cellsWide(count: number): number {
  return Math.max(6, Math.min(24, Math.ceil(Math.sqrt(count * 1.6))));
}

function Cell({
  entity,
  onHover,
  onClick,
}: {
  entity: Entity;
  onHover: (x: number, y: number) => void;
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
      }}
      aria-label={`${entity.name} — ${healthLabel(entity.health)}`}
      className={cx(
        "h-[14px] w-[14px] shrink-0 rounded-[1px] transition-transform",
        "hover:z-10 hover:scale-[1.35] focus-visible:z-10 focus-visible:scale-[1.35]",
      )}
      style={
        healthy
          ? {
              background: `color-mix(in srgb, ${color} 26%, var(--color-panel))`,
              boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${color} 55%, transparent)`,
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
      <div className="panel max-w-64 px-2.5 py-1.5 shadow-lg">
        <div className="flex items-center gap-1.5">
          <span
            className="h-2 w-2 shrink-0 rounded-[1px]"
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
