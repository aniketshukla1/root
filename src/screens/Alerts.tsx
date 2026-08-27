import { useMemo, useState } from "react";

import type { Catalog } from "../catalog";
import { markColor } from "../catalog";
import { formatDateTime, formatRelative } from "../format";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import type { Alert, AlertSeverity } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { compareAlerts, SEVERITY } from "../ui/AlertList";
import {
  Button,
  cx,
  Empty,
  Input,
  Panel,
  Segmented,
  ServiceMark,
  Tag,
} from "../ui/primitives";
import { AssistantIcon, SearchIcon } from "../ui/icons";

type StateFilter = "open" | "acknowledged" | "resolved" | "all";

/** Connection first, because "what is wrong with X" is the usual question. */
type GroupBy = "connection" | "severity" | "team" | "none";

const GROUP_LABELS: Record<GroupBy, string> = {
  connection: "Connection",
  severity: "Severity",
  team: "Team",
  none: "Flat",
};

interface AlertGroup {
  key: string;
  label: string;
  kind?: string;
  alerts: Alert[];
  unacknowledged: number;
  worst: AlertSeverity;
}

export function Alerts({
  alerts,
  model,
  catalog,
  syncedAt,
  onNavigate,
  onRefresh,
  onAcknowledge,
  onAsk,
  onExplain,
}: {
  alerts: Alert[];
  model: EstateModel;
  catalog: Catalog;
  syncedAt?: string;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAcknowledge: (alertId: string) => void;
  onAsk: () => void;
  onExplain: (alert: Alert) => void;
}) {
  const [state, setState] = useState<StateFilter>("open");
  const [severity, setSeverity] = useState<AlertSeverity | "any">("any");
  const [groupBy, setGroupBy] = useState<GroupBy>("connection");
  const [query, setQuery] = useState("");

  const teamOf = useMemo(() => {
    const map = new Map<string, string>();
    for (const service of model.services) {
      map.set(service.snapshot.profileId, service.ownership.team);
    }
    return map;
  }, [model.services]);

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return alerts
      .filter((alert) => {
        if (state === "open" && (!alert.active || alert.acknowledged)) return false;
        if (state === "acknowledged" && !alert.acknowledged) return false;
        if (state === "resolved" && alert.active) return false;
        if (severity !== "any" && alert.severity !== severity) return false;
        if (!needle) return true;
        return (
          alert.title.toLowerCase().includes(needle) ||
          alert.detail.toLowerCase().includes(needle) ||
          alert.profileName.toLowerCase().includes(needle) ||
          (alert.component ?? "").toLowerCase().includes(needle)
        );
      })
      .sort(compareAlerts);
  }, [alerts, state, severity, query]);

  const groups = useMemo<AlertGroup[]>(() => {
    if (groupBy === "none") {
      return [
        {
          key: "all",
          label: "All alerts",
          alerts: visible,
          unacknowledged: visible.filter((a) => !a.acknowledged).length,
          worst: visible[0]?.severity ?? "info",
        },
      ];
    }

    const buckets = new Map<string, { label: string; kind?: string; alerts: Alert[] }>();
    for (const alert of visible) {
      const key =
        groupBy === "connection"
          ? alert.profileId
          : groupBy === "severity"
            ? alert.severity
            : (teamOf.get(alert.profileId) ?? "Unassigned");
      const label =
        groupBy === "connection"
          ? alert.profileName
          : groupBy === "severity"
            ? SEVERITY[alert.severity].label
            : key;
      const bucket = buckets.get(key) ?? {
        label,
        kind: groupBy === "connection" ? alert.kind : undefined,
        alerts: [],
      };
      bucket.alerts.push(alert);
      buckets.set(key, bucket);
    }

    return [...buckets.entries()]
      .map<AlertGroup>(([key, bucket]) => ({
        key,
        label: bucket.label,
        kind: bucket.kind,
        alerts: bucket.alerts,
        unacknowledged: bucket.alerts.filter((a) => !a.acknowledged).length,
        worst: bucket.alerts.reduce<AlertSeverity>(
          (worst, a) => (SEVERITY[a.severity].rank < SEVERITY[worst].rank ? a.severity : worst),
          "info",
        ),
      }))
      // Worst severity first, then most unhandled — the group needing a
      // responder is always on top.
      .sort(
        (a, b) =>
          SEVERITY[a.worst].rank - SEVERITY[b.worst].rank ||
          b.unacknowledged - a.unacknowledged ||
          b.alerts.length - a.alerts.length ||
          a.label.localeCompare(b.label),
      );
  }, [visible, groupBy, teamOf]);

  const tally = useMemo(
    () => ({
      open: alerts.filter((a) => a.active && !a.acknowledged).length,
      acknowledged: alerts.filter((a) => a.acknowledged).length,
      resolved: alerts.filter((a) => !a.active).length,
    }),
    [alerts],
  );

  const unacked = visible.filter((a) => !a.acknowledged);

  return (
    <>
      <ScreenHeader
        title="Alerts"
        meta={
          <>
            <span className="num">{tally.open}</span> open
            <span className="text-ink-4/50">·</span>
            <span className="num">{tally.acknowledged}</span> acknowledged
            <span className="text-ink-4/50">·</span>
            <span className="num">{tally.resolved}</span> resolved
          </>
        }
        syncedAt={syncedAt}
        live
        onRefresh={onRefresh}
        onAsk={onAsk}
      >
        <div className="relative">
          <SearchIcon
            size={13}
            className="pointer-events-none absolute top-1/2 left-2 -translate-y-1/2 text-ink-4"
          />
          <Input
            className="w-44 pl-7"
            placeholder="Filter alerts…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <Segmented
          value={severity}
          onChange={setSeverity}
          options={[
            { value: "any", label: "Any" },
            { value: "critical", label: "Critical" },
            { value: "warning", label: "Warning" },
            { value: "info", label: "Info" },
          ]}
        />
        <Segmented
          value={state}
          onChange={setState}
          options={[
            { value: "open", label: "Open" },
            { value: "acknowledged", label: "Acked" },
            { value: "resolved", label: "Resolved" },
            { value: "all", label: "All" },
          ]}
        />
        <Segmented
          value={groupBy}
          onChange={setGroupBy}
          options={(Object.keys(GROUP_LABELS) as GroupBy[]).map((key) => ({
            value: key,
            label: GROUP_LABELS[key],
            title: key === "none" ? "No grouping" : `Group by ${GROUP_LABELS[key].toLowerCase()}`,
          }))}
        />
      </ScreenHeader>

      <Screen className="flex flex-col gap-3">
        {unacked.length > 1 && (
          <div className="flex items-center gap-2 rounded-lg border border-line bg-panel px-3 py-2">
            <span className="text-xs text-ink-3">
              <span className="num text-ink">{unacked.length}</span> alerts in this view have not
              been acknowledged.
            </span>
            <Button
              className="ml-auto"
              onClick={() => unacked.forEach((alert) => onAcknowledge(alert.id))}
              title="Acknowledging silences notifications. Alerts stay listed until their collector reports recovery."
            >
              Acknowledge all
            </Button>
          </div>
        )}

        {visible.length === 0 ? (
          <Panel>
            <Empty
              title={state === "open" ? "No open alerts" : "Nothing matches"}
              hint={
                state === "open"
                  ? "Every collector is reporting within threshold."
                  : "Adjust the filters to widen the view."
              }
            />
          </Panel>
        ) : (
          /* One table across every group so the columns stay aligned down the
             whole page and the header is read once. */
          <div className="panel overflow-hidden">
            <table className="w-full border-collapse">
              <thead className="sticky top-0 z-[1] bg-panel">
                <tr className="border-b border-line">
                  <th className="label px-3 py-1.5 text-left font-semibold">Severity</th>
                  <th className="label px-3 py-1.5 text-left font-semibold">Alert</th>
                  <th className="label hidden px-3 py-1.5 text-left font-semibold lg:table-cell">
                    Component
                  </th>
                  {groupBy !== "connection" && (
                    <th className="label px-3 py-1.5 text-left font-semibold">Connection</th>
                  )}
                  <th className="label hidden px-3 py-1.5 text-left font-semibold xl:table-cell">
                    Owner
                  </th>
                  <th className="label px-3 py-1.5 text-right font-semibold">First seen</th>
                  <th className="label px-3 py-1.5 text-right font-semibold">Last seen</th>
                  <th className="w-px" />
                </tr>
              </thead>

              {groups.map((group) => (
                <tbody key={group.key}>
                  {groupBy !== "none" && (
                    <tr
                      className="border-y border-line border-l-2 bg-page/60"
                      style={
                        groupBy === "connection" && group.kind
                          ? { borderLeftColor: markColor(catalog.get(group.kind).accent) }
                          : { borderLeftColor: SEVERITY[group.worst].color }
                      }
                    >
                      <td colSpan={8} className="px-3 py-1.5">
                        <div className="flex items-center gap-2">
                          {groupBy === "connection" && group.kind && (
                            <ServiceMark descriptor={catalog.get(group.kind)} size={15} />
                          )}
                          {groupBy === "connection" ? (
                            <button
                              onClick={() =>
                                onNavigate({
                                  name: "service",
                                  profileId: group.key,
                                  tab: "alerts",
                                })
                              }
                              className="text-xs font-semibold text-ink transition-colors hover:text-accent"
                            >
                              {group.label}
                            </button>
                          ) : (
                            <h2 className="text-xs font-semibold text-ink">{group.label}</h2>
                          )}
                          <span className="num text-2xs text-ink-4">{group.alerts.length}</span>
                          {group.unacknowledged > 0 && (
                            <Tag color="var(--color-warn)">
                              {group.unacknowledged} unacknowledged
                            </Tag>
                          )}
                          {groupBy === "connection" && group.kind && (
                            <span className="ml-auto text-2xs text-ink-4">
                              {catalog.label(group.kind)}
                            </span>
                          )}
                        </div>
                      </td>
                    </tr>
                  )}

                  {group.alerts.map((alert) => {
                    const severityMeta = SEVERITY[alert.severity];
                    const owner = teamOf.get(alert.profileId);

                    return (
                      <tr
                        key={alert.id}
                        className={cx(
                          "group border-b border-line/50 hover:bg-white/[0.035]",
                          !alert.active && "opacity-55",
                        )}
                        style={{ height: "var(--row-h)" }}
                      >
                        <td className="px-3 whitespace-nowrap">
                          <span className="flex items-center gap-1.5">
                            <span
                              className="h-1.5 w-1.5 shrink-0 rounded-full"
                              style={{ background: severityMeta.color }}
                              aria-hidden
                            />
                            <span className="text-xs" style={{ color: severityMeta.color }}>
                              {severityMeta.label}
                            </span>
                          </span>
                        </td>

                        <td className="max-w-md px-3">
                          <div className="flex min-w-0 items-center gap-1.5">
                            <span className="truncate text-xs font-medium text-ink">
                              {alert.title}
                            </span>
                            {alert.acknowledged && alert.active && <Tag>Acked</Tag>}
                            {!alert.active && <Tag color="var(--color-ok)">Resolved</Tag>}
                          </div>
                          <p className="truncate text-2xs text-ink-4" title={alert.detail}>
                            {alert.detail}
                          </p>
                        </td>

                        <td className="hidden max-w-40 px-3 lg:table-cell">
                          {alert.component ? (
                            <span className="num truncate text-xs text-ink-3">
                              {alert.component}
                            </span>
                          ) : (
                            <span className="text-ink-4/60">—</span>
                          )}
                        </td>

                        {groupBy !== "connection" && (
                          <td className="max-w-44 px-3">
                            <button
                              onClick={() =>
                                onNavigate({
                                  name: "service",
                                  profileId: alert.profileId,
                                  tab: "alerts",
                                })
                              }
                              className="flex min-w-0 items-center gap-1.5 text-xs text-ink-2 transition-colors hover:text-accent"
                            >
                              <ServiceMark descriptor={catalog.get(alert.kind)} size={15} />
                              <span className="truncate">{alert.profileName}</span>
                            </button>
                          </td>
                        )}

                        <td className="hidden max-w-40 px-3 xl:table-cell">
                          <span
                            className={cx(
                              "truncate text-xs",
                              owner && owner !== "Unassigned"
                                ? "text-ink-3"
                                : "text-ink-4 italic",
                            )}
                          >
                            {owner ?? "Unassigned"}
                          </span>
                        </td>

                        <td
                          className="num px-3 text-right whitespace-nowrap text-ink-4"
                          title={formatDateTime(alert.firstSeen)}
                        >
                          {formatRelative(alert.firstSeen)}
                        </td>
                        <td
                          className="num px-3 text-right whitespace-nowrap text-ink-4"
                          title={formatDateTime(alert.lastSeen)}
                        >
                          {formatRelative(alert.lastSeen)}
                        </td>

                        <td className="px-3">
                          <div className="flex items-center justify-end gap-1 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
                            <Button
                              variant="ghost"
                              onClick={() => onExplain(alert)}
                              title="Explain this alert"
                              className="gap-1.5 text-accent"
                            >
                              <AssistantIcon size={12} />
                              Explain
                            </Button>
                            {alert.active && !alert.acknowledged && (
                              <Button
                                variant="ghost"
                                onClick={() => onAcknowledge(alert.id)}
                                title="Acknowledgement is not resolution — the alert stays listed until the collector reports recovery."
                              >
                                Ack
                              </Button>
                            )}
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              ))}
            </table>
          </div>
        )}
      </Screen>
    </>
  );
}
