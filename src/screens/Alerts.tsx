import { useMemo, useState } from "react";

import type { Catalog } from "../catalog";
import { markColor } from "../catalog";
import { formatDateTime, formatRelative } from "../format";
import { confidenceMeaning } from "../incident";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import type { Alert, AlertSeverity, Incident } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { compareAlerts, SEVERITY } from "../ui/AlertList";
import { PredictionList } from "../ui/PredictionList";
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

/**
 * Incident first, because it is the only grouping that answers "how many
 * things are actually happening". Four rows on one connection are one problem;
 * four rows across four connections may also be one problem. Connection,
 * severity and team all answer narrower questions and stay available for them.
 */
type GroupBy = "incident" | "connection" | "severity" | "team" | "none";

const GROUP_LABELS: Record<GroupBy, string> = {
  incident: "Incident",
  connection: "Connection",
  severity: "Severity",
  team: "Team",
  none: "Flat",
};

interface AlertGroup {
  key: string;
  label: string;
  kind?: string;
  /** Present only when grouping by incident — drives the confidence tag. */
  incident?: Incident;
  alerts: Alert[];
  unacknowledged: number;
  worst: AlertSeverity;
}

export function Alerts({
  alerts,
  incidents,
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
  incidents: Incident[];
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
  const [groupBy, setGroupBy] = useState<GroupBy>("incident");
  const [query, setQuery] = useState("");

  /** Which incident an alert belongs to, from the backend's own grouping. */
  const incidentOf = useMemo(() => {
    const map = new Map<string, Incident>();
    for (const incident of incidents) {
      for (const alertId of incident.alertIds) map.set(alertId, incident);
    }
    return map;
  }, [incidents]);

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

    const buckets = new Map<
      string,
      { label: string; kind?: string; incident?: Incident; alerts: Alert[] }
    >();
    for (const alert of visible) {
      const incident = incidentOf.get(alert.id);
      // An alert with no incident is not an error: incidents cover the ACTIVE
      // set, so a resolved alert being read in the history has none. It gets
      // its own bucket rather than being dropped or silently folded in.
      const key =
        groupBy === "incident"
          ? (incident?.id ?? `unattached:${alert.profileId}`)
          : groupBy === "connection"
            ? alert.profileId
            : groupBy === "severity"
              ? alert.severity
              : (teamOf.get(alert.profileId) ?? "Unassigned");
      const label =
        groupBy === "incident"
          ? (incident?.title ?? `${alert.profileName} — no longer firing`)
          : groupBy === "connection"
            ? alert.profileName
            : groupBy === "severity"
              ? SEVERITY[alert.severity].label
              : key;
      const bucket = buckets.get(key) ?? {
        label,
        kind: groupBy === "connection" || groupBy === "incident" ? alert.kind : undefined,
        incident: groupBy === "incident" ? incident : undefined,
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
        incident: bucket.incident,
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
  }, [visible, groupBy, teamOf, incidentOf]);

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
          label="Severity"
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
          label="State"
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
          label="Group by"
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
        {/* Above the table, and outside it. This screen is about what is
            firing; a forecast is not, and putting one in the table — even in
            its own row, even in its own colour — would make every filter,
            count and "acknowledge all" on this page quietly wrong. */}
        {model.predictions.length > 0 && (
          <Panel
            title="What breaks next"
            subtitle="not firing — none of this is in the table below"
            flush
            bodyClassName="overflow-y-auto max-h-[260px]"
          >
            <PredictionList
              predictions={model.predictions}
              catalog={catalog}
              onNavigate={onNavigate}
              onExplain={onExplain}
            />
          </Panel>
        )}

        {unacked.length > 1 && (
          <div className="flex items-center gap-2 rounded-md border border-line bg-panel px-3 py-2">
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
                  <th className="label cell-pad-x py-1.5 text-left font-semibold">Severity</th>
                  <th className="label cell-pad-x py-1.5 text-left font-semibold">Alert</th>
                  <th className="label hidden cell-pad-x py-1.5 text-left font-semibold lg:table-cell">
                    Component
                  </th>
                  {groupBy !== "connection" && (
                    <th className="label cell-pad-x py-1.5 text-left font-semibold">Connection</th>
                  )}
                  <th className="label hidden cell-pad-x py-1.5 text-left font-semibold xl:table-cell">
                    Owner
                  </th>
                  <th className="label cell-pad-x py-1.5 text-right font-semibold">First seen</th>
                  <th className="label cell-pad-x py-1.5 text-right font-semibold">Last seen</th>
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
                      <td colSpan={8} className="cell-pad-x py-1.5">
                        {/* Wraps. Every item in this row is nowrap — a title,
                            three counts, two tags — and in a table cell that
                            is a width the whole table has to honour: at 1100px
                            it pushed First seen and Last seen off the edge. */}
                        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
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
                              className="max-w-xl truncate text-xs font-semibold text-ink transition-colors hover:text-accent"
                              title={group.label}
                            >
                              {group.label}
                            </button>
                          ) : groupBy === "incident" && group.incident ? (
                            <button
                              onClick={() => onNavigate({ name: "overview" })}
                              className="max-w-xl truncate text-xs font-semibold text-ink transition-colors hover:text-accent"
                              title={group.label}
                            >
                              {group.label}
                            </button>
                          ) : (
                            <h2
                              className="max-w-xl truncate text-xs font-semibold text-ink"
                              title={group.label}
                            >
                              {group.label}
                            </h2>
                          )}
                          {/* A noun on every number. A bare "2" beside "2
                              unacknowledged" beside "1 more hidden" was three
                              counts of the same group and none said of what. */}
                          <span className="num shrink-0 text-2xs text-ink-4">
                            {group.incident &&
                            group.incident.alertIds.length > group.alerts.length
                              ? `${group.alerts.length} of ${group.incident.alertIds.length} alerts`
                              : `${group.alerts.length} alert${group.alerts.length === 1 ? "" : "s"}`}
                          </span>
                          {group.unacknowledged > 0 &&
                            group.unacknowledged < group.alerts.length && (
                              <Tag color="var(--color-warn)">
                                {group.unacknowledged} unacknowledged
                              </Tag>
                            )}
                          {group.unacknowledged > 0 &&
                            group.unacknowledged === group.alerts.length && (
                              <Tag color="var(--color-warn)">unacknowledged</Tag>
                            )}
                          {/* Grouping is a claim, so it carries its own
                              caveat wherever it is shown. A merged row an
                              operator cannot argue with is one they cannot
                              correct. */}
                          {group.incident && group.incident.profileIds.length > 1 && (
                            <span className="num text-2xs text-ink-4">
                              across {group.incident.profileIds.length} connections
                            </span>
                          )}
                          {/* The filter can hide part of an incident, and a
                              group that shows three of five rows without
                              saying so lets an operator believe they have
                              seen the whole thing. */}
                          {group.incident &&
                            group.incident.alertIds.length > group.alerts.length && (
                              <span className="num text-2xs text-ink-4">
                                {group.incident.alertIds.length - group.alerts.length} hidden by
                                the filter
                              </span>
                            )}
                          {group.incident && group.incident.confidence !== "high" && (
                            <Tag
                              color={
                                group.incident.confidence === "low"
                                  ? "var(--color-serious)"
                                  : "var(--color-warn)"
                              }
                              title={confidenceMeaning(group.incident)}
                            >
                              {group.incident.confidence.toUpperCase()} CONFIDENCE
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
                          "group border-b border-line/50 row-hover",
                          !alert.active && "opacity-55",
                        )}
                        style={{ height: "var(--row-h)" }}
                      >
                        <td className="cell-pad-x whitespace-nowrap">
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

                        {/* The cap is what lets the table fit a narrow
                            column: a td is at least as wide as its content,
                            and an alert title is as long as the collector
                            wrote it. Below ~1024px of screen the first-seen
                            and last-seen columns were pushed off the right
                            edge by this one cell. */}
                        <td className="max-w-56 cell-pad-x @5xl:max-w-md">
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

                        {/* Both of the cells below hold a `div`, not a
                            `span`, and that is load-bearing: `truncate` is
                            overflow-hidden, overflow does not apply to an
                            inline box, and a component name is whatever the
                            collector called the thing. A Redis slow command
                            arrives 170 characters long and used to paint
                            straight across the four columns to its right. */}
                        <td className="hidden max-w-44 cell-pad-x lg:table-cell">
                          {alert.component ? (
                            <div className="num truncate text-xs text-ink-3" title={alert.component}>
                              {alert.component}
                            </div>
                          ) : (
                            <span className="text-ink-4/60">—</span>
                          )}
                        </td>

                        {groupBy !== "connection" && (
                          <td className="max-w-44 cell-pad-x">
                            <button
                              onClick={() =>
                                onNavigate({
                                  name: "service",
                                  profileId: alert.profileId,
                                  tab: "alerts",
                                })
                              }
                              /* `w-full`, because a button is shrink-to-fit:
                                 without it the button grows past its own
                                 cell to fit the name and the truncate inside
                                 it never has a bound to clip against. */
                              className="flex w-full min-w-0 items-center gap-1.5 text-xs text-ink-2 transition-colors hover:text-accent"
                            >
                              <ServiceMark descriptor={catalog.get(alert.kind)} size={15} />
                              <span className="truncate">{alert.profileName}</span>
                            </button>
                          </td>
                        )}

                        <td className="hidden max-w-40 cell-pad-x xl:table-cell">
                          <div
                            className={cx(
                              "truncate text-xs",
                              owner && owner !== "Unassigned"
                                ? "text-ink-3"
                                : "text-ink-4 italic",
                            )}
                          >
                            {owner ?? "Unassigned"}
                          </div>
                        </td>

                        <td
                          className="num cell-pad-x text-right whitespace-nowrap text-ink-4"
                          title={formatDateTime(alert.firstSeen)}
                        >
                          {formatRelative(alert.firstSeen)}
                        </td>
                        <td
                          className="num cell-pad-x text-right whitespace-nowrap text-ink-4"
                          title={formatDateTime(alert.lastSeen)}
                        >
                          {formatRelative(alert.lastSeen)}
                        </td>

                        <td className="cell-pad-x">
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
