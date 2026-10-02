import { useEffect, useState } from "react";

import { api } from "../api";
import { formatRelative } from "../format";
import type { Alert, Incident, Notice } from "../types";
import { ChannelMark } from "./ChannelMark";
import { useTick } from "./charts";
import { cx } from "./primitives";

export const minutesUntil = (iso: string) =>
  Math.max(0, Math.ceil((Date.parse(iso) - Date.now()) / 60_000));

/**
 * Who each alert has reached, and who it will reach next, keyed by alert id —
 * and how many channels are switched on at all, so "nobody was told" can be
 * told apart from "nobody's rules matched".
 *
 * Polled: the answer changes with the clock as well as with the alerts. An
 * escalation comes due while nothing else on the screen moves.
 */
export function useNotices(alerts: Alert[]) {
  const [notices, setNotices] = useState<Record<string, Notice[]>>({});
  const [channelCount, setChannelCount] = useState<number | null>(null);
  useTick(15_000);

  useEffect(() => {
    let live = true;
    const load = () =>
      api
        .getAlertNotices()
        // `?? {}`: a server from before notifications answers null, and one
        // missing map must not take the screen down with it.
        .then((next) => live && setNotices(next ?? {}))
        .catch(() => {});
    void load();
    const timer = setInterval(load, 10_000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [alerts]);

  useEffect(() => {
    let live = true;
    api
      .getNotifications()
      .then((view) => live && setChannelCount(view.channels.filter((c) => c.enabled).length))
      .catch(() => live && setChannelCount(null));
    return () => {
      live = false;
    };
  }, []);

  return { notices, channelCount };
}

/** One alert's channels: told, coming, failing, or held back by an acknowledgement. */
export function NoticeMarks({ notices }: { notices?: Notice[] }) {
  if (!notices?.length) return <span className="text-ink-4/60">—</span>;
  return (
    <div className="flex items-center gap-2">
      {notices.slice(0, 4).map((notice) => (
        <NoticeMark key={notice.channelId} notice={notice} />
      ))}
      {notices.length > 4 && (
        <span className="num text-2xs text-ink-4">+{notices.length - 4}</span>
      )}
    </div>
  );
}

export function NoticeMark({ notice }: { notice: Notice }) {
  const pager = notice.kind === "pagerduty";
  const mins = minutesUntil(notice.at);
  const clock = new Date(notice.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const title = {
    sent: `${notice.channelName} was told ${formatRelative(notice.at)}`,
    due:
      notice.delayMinutes > 0
        ? `${notice.channelName} ${pager ? "pages" : "is told"} in ${mins} min unless someone acknowledges`
        : `${notice.channelName} is told in a moment`,
    retrying: `Delivery to ${notice.channelName} failed — trying again at ${clock}`,
    held: `Acknowledged in time — ${notice.channelName} was not ${pager ? "paged" : "told"}`,
  }[notice.status];

  return (
    <span className="relative inline-flex items-center gap-1" title={title}>
      <ChannelMark
        kind={notice.kind}
        size={16}
        title={title}
        className={cx(
          notice.status === "due" && "opacity-50",
          notice.status === "held" && "opacity-30 grayscale",
        )}
      />
      {(notice.status === "sent" || notice.status === "retrying") && (
        <span
          aria-hidden
          className="absolute top-[11px] left-[11px] h-[7px] w-[7px] rounded-full ring-2 ring-panel"
          style={{
            background: notice.status === "sent" ? "var(--color-ok)" : "var(--color-crit)",
          }}
        />
      )}
      {notice.status === "due" && notice.delayMinutes > 0 && (
        <span className="num text-2xs font-semibold" style={{ color: "var(--color-warn)" }}>
          {mins}m
        </span>
      )}
      {notice.status === "held" && <span className="text-2xs text-ink-4">held</span>}
    </span>
  );
}

// ---------------------------------------------------------------- who knows

const RANK: Record<Notice["status"], number> = { sent: 0, retrying: 1, due: 2, held: 3 };

/**
 * One incident's standing with every channel: who has been told, who is
 * about to be, and what is holding back. Folded per channel across the
 * incident's alerts — the best news per channel, so "told" wins over "due".
 */
function standing(incident: Incident, notices: Record<string, Notice[]>): Notice[] {
  const best = new Map<string, Notice>();
  for (const id of incident.alertIds) {
    for (const notice of notices[id] ?? []) {
      const held = best.get(notice.channelId);
      if (
        !held ||
        RANK[notice.status] < RANK[held.status] ||
        (RANK[notice.status] === RANK[held.status] && notice.at < held.at)
      ) {
        best.set(notice.channelId, notice);
      }
    }
  }
  return [...best.values()].sort(
    (a, b) => a.delayMinutes - b.delayMinutes || a.at.localeCompare(b.at),
  );
}

/**
 * Who knows about this incident.
 *
 * The question an on-call asks right after "what broke": has anybody been
 * told, and is somebody about to be paged? Said per channel, with the
 * countdown for an escalation — the one thing on the screen an
 * acknowledgement would change.
 */
export function WhoKnows({
  incident,
  notices,
  channelCount,
  layout = "list",
  onSetUp,
}: {
  incident: Incident;
  notices: Record<string, Notice[]>;
  channelCount: number | null;
  layout?: "list" | "strip";
  onSetUp: () => void;
}) {
  const rows = standing(incident, notices);

  const empty =
    channelCount === null ? null : channelCount === 0 ? (
      <span className="text-xs text-ink-3">
        Nobody is notified — no channel is set up.{" "}
        <button
          type="button"
          onClick={onSetUp}
          className="text-ink-2 underline-offset-2 hover:text-ink hover:underline"
        >
          Set up notifications →
        </button>
      </span>
    ) : rows.length === 0 ? (
      <span className="text-xs" style={{ color: "var(--color-warn)" }}>
        No channel's rules match this incident — nobody has been told.{" "}
        <button
          type="button"
          onClick={onSetUp}
          className="text-ink-2 underline-offset-2 hover:text-ink hover:underline"
        >
          Review channels →
        </button>
      </span>
    ) : null;

  const describe = (n: Notice) => {
    const pager = n.kind === "pagerduty";
    switch (n.status) {
      case "sent":
        return { text: `told ${formatRelative(n.at)}`, color: "var(--color-ok)" };
      case "retrying":
        return { text: "delivery failing — retrying", color: "var(--color-crit)" };
      case "held":
        return { text: "held — acknowledged in time", color: "var(--color-ink-4)" };
      case "due": {
        const mins = minutesUntil(n.at);
        const when = mins === 0 ? "any moment" : `in ${mins} min`;
        return {
          text:
            n.delayMinutes > 0
              ? `${pager ? "pages" : "is told"} ${when} unless acknowledged`
              : `is told ${when}`,
          color: "var(--color-warn)",
        };
      }
    }
  };

  if (layout === "strip") {
    return (
      <div className="panel flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-2.5">
        <span className="label">Who knows</span>
        {empty ??
          rows.map((n) => {
            const d = describe(n);
            return (
              <span key={n.channelId} className="flex items-center gap-1.5 text-xs">
                <ChannelMark kind={n.kind} size={16} />
                <span className="text-ink-2">{n.channelName}</span>
                <span className="text-2xs" style={{ color: d.color }}>
                  {d.text}
                </span>
              </span>
            );
          })}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      {empty ??
        rows.map((n) => {
          const d = describe(n);
          return (
            <div
              key={n.channelId}
              className={cx(
                "-mx-1.5 flex min-w-0 items-center gap-2.5 rounded-md px-1.5 py-1",
                // The row an acknowledgement would change, lit as such.
                n.status === "due" && n.delayMinutes > 0 && "hue",
              )}
              style={{ "--hue": "var(--color-warn)" } as React.CSSProperties}
            >
              <ChannelMark
                kind={n.kind}
                size={22}
                className={cx(n.status === "held" && "opacity-40 grayscale")}
              />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-xs text-ink-2">{n.channelName}</span>
                <span className="block text-2xs" style={{ color: d.color }}>
                  {d.text}
                </span>
              </span>
            </div>
          );
        })}
    </div>
  );
}
