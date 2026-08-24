// The generic section renderer.
//
// Collectors declare sections of typed columns; this draws them and nothing
// else. It is the floor that guarantees a service type this build has never
// heard of still gets a correct, domain-shaped table with no frontend change,
// so it knows about cell *kinds* only — never about Docker, Jenkins, or
// databases.

import { useMemo, useState } from "react";

import { formatDateTime, formatDuration, formatMetricValue, formatRelative } from "../format";
import type { CellValue, ColumnSpec, Row, RowAction, Section } from "../types";
import { Meter } from "./charts";
import { Button, cx, Dot, Empty, HealthBadge, Input, Tag } from "./primitives";

/** Sortable scalar for a cell, or null when it cannot be ordered. */
function sortKey(value: CellValue | undefined): number | string | null {
  if (!value) return null;
  switch (value.type) {
    case "number":
    case "duration":
    case "gauge":
      return value.value;
    case "timestamp":
      return new Date(value.value).getTime();
    case "text":
      return value.value.toLowerCase();
    case "status":
      return value.value;
    default:
      return null;
  }
}

function Cell({ column, value }: { column: ColumnSpec; value: CellValue | undefined }) {
  if (!value || value.type === "null") return <span className="text-ink-4/60">—</span>;

  switch (value.type) {
    case "status":
      return <HealthBadge health={value.value} />;

    case "tags": {
      const tags = value.value.filter(Boolean);
      if (tags.length === 0) return <span className="text-ink-4/60">—</span>;
      return (
        <span className="flex flex-wrap gap-1">
          {tags.map((tag, i) => (
            <Tag key={`${tag}-${i}`}>{tag}</Tag>
          ))}
        </span>
      );
    }

    case "duration":
      return <span className="num">{formatDuration(value.value * 1000)}</span>;

    case "timestamp":
      return (
        <span className="num" title={formatDateTime(value.value)}>
          {formatRelative(value.value)}
        </span>
      );

    case "gauge":
      return (
        <Meter
          value={value.value}
          max={value.max}
          warnAbove={value.warnAbove}
          unit={column.kind.type === "gauge" ? column.kind.unit : "count"}
        />
      );

    case "number":
      return (
        <span className="num">
          {formatMetricValue(value.value, column.kind.type === "metric" ? column.kind.unit : "count")}
        </span>
      );

    case "text":
    default: {
      const text = (value as { value: string }).value;
      // A `code` column wraps in monospace, which is what lets a full SQL
      // statement stay readable instead of being truncated into a shared
      // detail column.
      return column.kind.type === "code" ? (
        <code className="block max-w-xl break-words whitespace-pre-wrap text-ink-2">{text}</code>
      ) : (
        <span className="block truncate" title={text}>
          {text}
        </span>
      );
    }
  }
}

export function SectionTable({
  section,
  onAction,
  onOpenRow,
  initialLimit = 25,
  /** Prefixed to the primary column, for tables that span connections. */
  originOf,
}: {
  section: Section;
  onAction?: (action: RowAction, row: Row) => void;
  /** Drill into one component. Omit to leave rows inert. */
  onOpenRow?: (row: Row) => void;
  initialLimit?: number;
  originOf?: (row: Row) => React.ReactNode;
}) {
  const [sort, setSort] = useState<{ key: string; desc: boolean } | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [filter, setFilter] = useState("");

  const hasActions = section.rows.some((r) => r.actions.length > 0);

  const filtered = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return section.rows;
    return section.rows.filter(
      (row) =>
        row.name.toLowerCase().includes(needle) ||
        row.summary.toLowerCase().includes(needle) ||
        Object.values(row.cells).some(
          (cell) => cell.type === "text" && cell.value.toLowerCase().includes(needle),
        ),
    );
  }, [section.rows, filter]);

  const sorted = useMemo(() => {
    if (!sort) return filtered;
    return [...filtered].sort((a, b) => {
      const left = sortKey(a.cells[sort.key]);
      const right = sortKey(b.cells[sort.key]);
      // Nulls sink, whichever direction is active.
      if (left === null && right === null) return 0;
      if (left === null) return 1;
      if (right === null) return -1;
      const cmp =
        typeof left === "number" && typeof right === "number"
          ? left - right
          : String(left).localeCompare(String(right));
      return sort.desc ? -cmp : cmp;
    });
  }, [filtered, sort]);

  const visible = expanded ? sorted : sorted.slice(0, initialLimit);
  const problems = section.rows.filter((r) => r.health === "down" || r.health === "degraded").length;

  const toggleSort = (column: ColumnSpec) => {
    if (!column.sortable) return;
    setSort((current) =>
      current?.key === column.key
        ? current.desc
          ? null
          : { key: column.key, desc: true }
        : { key: column.key, desc: false },
    );
  };

  return (
    <section className="panel overflow-hidden">
      <header className="flex h-9 items-center gap-2 border-b border-line px-3">
        <h3 className="text-[13px] font-semibold text-ink">{section.label}</h3>
        <span className="num text-2xs text-ink-4">{section.rows.length}</span>
        {problems > 0 && (
          <Tag color="var(--color-warn)">{problems} need attention</Tag>
        )}
        {section.rows.length > 8 && (
          <Input
            className="ml-auto w-48"
            placeholder={`Filter ${section.label.toLowerCase()}…`}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        )}
      </header>

      {sorted.length === 0 ? (
        <Empty
          title={filter ? "No matches" : section.label}
          hint={filter ? `Nothing matches “${filter}”.` : section.emptyText}
        />
      ) : (
        <>
          <div className="overflow-x-auto">
            <table className="w-full border-collapse">
              <thead>
                <tr className="border-b border-line">
                  {originOf && <th className="label px-3 py-1.5 text-left font-semibold">Connection</th>}
                  {section.columns.map((column) => (
                    <th
                      key={column.key}
                      onClick={() => toggleSort(column)}
                      className={cx(
                        "label px-3 py-1.5 font-semibold whitespace-nowrap",
                        column.align === "end" ? "text-right" : "text-left",
                        column.secondary && "hidden lg:table-cell",
                        column.sortable && "cursor-pointer select-none hover:text-ink-2",
                        sort?.key === column.key && "text-ink-2",
                      )}
                    >
                      {column.label}
                      {sort?.key === column.key && (
                        <span className="ml-1">{sort.desc ? "↓" : "↑"}</span>
                      )}
                    </th>
                  ))}
                  {hasActions && <th className="w-px" />}
                </tr>
              </thead>
              <tbody>
                {visible.map((row) => (
                  <tr
                    key={row.id}
                    className="group border-b border-line/50 last:border-0 hover:bg-white/[0.035]"
                    style={{ height: "var(--row-h)" }}
                  >
                    {originOf && (
                      <td className="max-w-40 px-3 text-xs text-ink-3">{originOf(row)}</td>
                    )}
                    {section.columns.map((column) => (
                      <td
                        key={column.key}
                        className={cx(
                          "max-w-72 px-3 text-xs text-ink-2",
                          column.align === "end" ? "text-right" : "text-left",
                          column.secondary && "hidden lg:table-cell",
                        )}
                      >
                        {column.primary ? (
                          <span className="flex min-w-0 items-center gap-2">
                            <span title={row.health}>
                              <Dot health={row.health} />
                            </span>
                            {/* The identifying cell is the drill-in. Making the
                                whole row clickable would swallow the action
                                buttons that sit at its other end. */}
                            {onOpenRow ? (
                              <button
                                onClick={() => onOpenRow(row)}
                                title={`Open ${row.name}`}
                                className="min-w-0 truncate text-left font-medium text-ink transition-colors group-hover:text-accent"
                              >
                                <Cell column={column} value={row.cells[column.key]} />
                              </button>
                            ) : (
                              <span className="min-w-0 truncate font-medium text-ink">
                                <Cell column={column} value={row.cells[column.key]} />
                              </span>
                            )}
                          </span>
                        ) : (
                          <Cell column={column} value={row.cells[column.key]} />
                        )}
                      </td>
                    ))}
                    {hasActions && (
                      <td className="px-3 whitespace-nowrap">
                        <span className="flex justify-end gap-1 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
                          {row.actions.map((action) => (
                            <Button
                              key={action.tool}
                              variant={action.destructive ? "danger" : "default"}
                              onClick={() => onAction?.(action, row)}
                              disabled={!onAction}
                              title={
                                action.destructive
                                  ? `${action.label} — you will be asked to confirm`
                                  : action.label
                              }
                            >
                              {action.label}
                            </Button>
                          ))}
                        </span>
                      </td>
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {sorted.length > initialLimit && (
            <div className="border-t border-line px-3 py-1.5">
              <Button variant="ghost" onClick={() => setExpanded((v) => !v)}>
                {expanded ? `Show first ${initialLimit}` : `Show all ${sorted.length}`}
              </Button>
            </div>
          )}
        </>
      )}
    </section>
  );
}

/** All of a snapshot's sections, in the order the collector asked for. */
export function SectionList({
  sections,
  onAction,
  onOpenRow,
}: {
  sections: Section[];
  onAction?: (action: RowAction, row: Row) => void;
  onOpenRow?: (section: Section, row: Row) => void;
}) {
  const ordered = useMemo(
    () => [...sections].sort((a, b) => a.priority - b.priority),
    [sections],
  );
  if (ordered.length === 0) return null;

  return (
    <div className="flex flex-col gap-3">
      {ordered.map((section) => (
        <SectionTable
          key={section.id}
          section={section}
          onAction={onAction}
          onOpenRow={onOpenRow ? (row) => onOpenRow(section, row) : undefined}
        />
      ))}
    </div>
  );
}
