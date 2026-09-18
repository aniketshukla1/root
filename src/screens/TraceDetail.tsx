// One distributed trace, as a waterfall.
//
// The question someone opens this page with is never "how long did it take" —
// they already saw that in the list. It is "where did the time go", so that is
// what the page leads with: a per-service breakdown by *self* time, which is
// duration minus whatever the children accounted for. A span that took 900ms
// because its one child took 890ms did not spend 900ms, and ranking by total
// duration would point at the wrong service every time.
//
// The tree arrives already assembled from the backend (see `traces.rs`), so
// nothing here walks parent ids. This file positions bars and nothing else.

import { useCallback, useEffect, useMemo, useState } from "react";

import { api, errorMessage } from "../api";
import { seriesColor } from "../catalog";
import { formatDateTime, formatDuration } from "../format";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import type { Trace, TraceNode } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Button, cx, Empty, Panel, PanelHeader } from "../ui/primitives";
import { ChevronLeft } from "../ui/icons";

/** Narrowest bar that still reads as a bar rather than as a gap. */
const MIN_BAR_PERCENT = 0.4;

/** Fetches one trace and renders it. Screens own their own loading here. */
export function TraceDetail({
  profileId,
  traceId,
  model,
  onNavigate,
}: {
  profileId: string;
  traceId: string;
  model: EstateModel;
  onNavigate: (view: View) => void;
}) {
  const [trace, setTrace] = useState<Trace | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const service = model.services.find((s) => s.snapshot.profileId === profileId);
  const profileName = service?.snapshot.profileName ?? profileId;

  const load = useCallback(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .getTrace(profileId, traceId)
      .then((result) => {
        if (cancelled) return;
        setTrace(result);
        setLoading(false);
      })
      .catch((cause) => {
        if (cancelled) return;
        setTrace(null);
        setError(errorMessage(cause));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [profileId, traceId]);

  useEffect(() => load(), [load]);

  const onBack = () => onNavigate({ name: "service", profileId, tab: "components" });
  const onRetry = () => load();

  return (
    <TraceBody
      trace={trace}
      loading={loading}
      error={error}
      profileName={profileName}
      onNavigate={onNavigate}
      onBack={onBack}
      onRetry={onRetry}
    />
  );
}

function TraceBody({
  trace,
  loading,
  error,
  profileName,
  onNavigate,
  onBack,
  onRetry,
}: {
  trace: Trace | null;
  loading: boolean;
  error: string | null;
  profileName: string;
  onNavigate: (view: View) => void;
  onBack: () => void;
  onRetry: () => void;
}) {
  const [openSpan, setOpenSpan] = useState<string | null>(null);

  // `seriesColor` rather than a palette of this screen's own: it hands out
  // slots in the order that keeps the hues nearest a status colour until last,
  // which is what stops a span bar reading as a health signal. A second
  // ordering living here would drift from it the first time either changed.
  //
  // Assigned by rank in the breakdown, so the service holding the most time
  // always takes slot one and the eye lands on the same colour as the top row
  // of the table beside it.
  const colorOf = useMemo(() => {
    const map = new Map<string, string>();
    trace?.services.forEach((service, i) => {
      map.set(service.service, seriesColor(i));
    });
    return (service: string) => map.get(service) ?? "var(--color-unknown)";
  }, [trace]);

  const flat = useMemo(() => (trace ? flatten(trace.roots) : []), [trace]);

  if (loading) {
    return (
      <Screen>
        <Empty title="Loading the trace" hint="Fetching spans from the store." />
      </Screen>
    );
  }

  if (error || !trace) {
    return (
      <Screen>
        <Empty
          title="That trace could not be opened"
          hint={
            <span className="flex flex-col items-center gap-3">
              <span>{error ?? "No spans came back."}</span>
              <span className="flex gap-2">
                <Button onClick={onBack}>Back</Button>
                <Button variant="primary" onClick={onRetry}>
                  Try again
                </Button>
              </span>
            </span>
          }
        />
      </Screen>
    );
  }

  const total = Math.max(trace.durationMs, 0.001);

  return (
    <>
      <ScreenHeader
        title={trace.rootName || "Trace"}
        meta={
          <>
            <Button variant="ghost" onClick={onBack} title="Back">
              <ChevronLeft />
            </Button>
            <span className="num text-2xs tracking-[.18em] text-ink-3 uppercase">
              {trace.rootService || "unknown"}
            </span>
            <Sep />
            <span className="num">{formatDuration(trace.durationMs)}</span>
            <Sep />
            <span className="num">
              {trace.spanCount} span{trace.spanCount === 1 ? "" : "s"}
            </span>
            {trace.errorCount > 0 && (
              <>
                <Sep />
                <span className="num text-crit">
                  {trace.errorCount} error{trace.errorCount === 1 ? "" : "s"}
                </span>
              </>
            )}
            <Sep />
            <span className="num text-ink-4">{formatDateTime(trace.startedAt)}</span>
          </>
        }
      />

      <Screen>
        {trace.orphanCount > 0 && (
          // Said plainly rather than left for the reader to infer from a tree
          // that looks oddly wide: some parents never arrived, so the shape
          // below is a fragment and the timings inside it may not nest.
          <p className="mb-3 border border-warn/30 bg-warn/5 px-3 py-2 text-xs text-ink-2">
            <span className="num tracking-[.18em] text-warn uppercase">Partial</span>{" "}
            {trace.orphanCount} span{trace.orphanCount === 1 ? "" : "s"} name a parent that
            is not in this trace, so {trace.orphanCount === 1 ? "it hangs" : "they hang"} at
            the root. Sampling or a dropped export is the usual cause.
          </p>
        )}

        <div className="mb-3 grid gap-3 @3xl:grid-cols-[minmax(0,320px)_minmax(0,1fr)]">
          <Panel>
            <PanelHeader title="Where the time went" />
            <table className="w-full">
              <tbody>
                {trace.services.map((service) => {
                  const share = (service.selfMs / total) * 100;
                  return (
                    <tr key={service.service} className="border-t border-line-soft">
                      <td className="cell-pad-x py-1.5">
                        <span className="flex min-w-0 items-center gap-2">
                          <span
                            aria-hidden
                            className="size-[9px] shrink-0 rounded-[1px] ring-1 ring-inset ring-black/25"
                            style={{ background: colorOf(service.service) }}
                          />
                          <span className="min-w-0 truncate text-xs text-ink">
                            {service.service}
                          </span>
                          {service.errorCount > 0 && (
                            <span className="num shrink-0 text-2xs text-crit">
                              {service.errorCount} err
                            </span>
                          )}
                        </span>
                      </td>
                      <td className="cell-pad-x py-1.5 text-right">
                        <span className="num text-xs text-ink-2">
                          {formatDuration(service.selfMs)}
                        </span>
                        <span className="num ml-2 text-2xs text-ink-4">
                          {share.toFixed(0)}%
                        </span>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            <p className="border-t border-line-soft px-3 py-2 text-2xs leading-relaxed text-ink-4">
              Self time: each span's duration minus what its children accounted for. It adds
              up to the trace, and it is the number that says which service to look at.
            </p>
          </Panel>

          <Panel>
            <PanelHeader title="Spans" />
            <div className="overflow-x-auto">
              <table className="w-full">
                <thead>
                  <tr className="border-b border-line">
                    <th className="cell-pad-x py-1.5 text-left num text-2xs tracking-[.18em] text-ink-4 uppercase">
                      Span
                    </th>
                    <th className="w-[45%] cell-pad-x py-1.5 text-left num text-2xs tracking-[.18em] text-ink-4 uppercase">
                      {formatDuration(total)}
                    </th>
                    <th className="cell-pad-x py-1.5 text-right num text-2xs tracking-[.18em] text-ink-4 uppercase">
                      Self
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {flat.map((node) => (
                    <SpanRow
                      key={node.span.spanId}
                      node={node}
                      total={total}
                      color={colorOf(node.span.service)}
                      open={openSpan === node.span.spanId}
                      onToggle={() =>
                        setOpenSpan((current) =>
                          current === node.span.spanId ? null : node.span.spanId,
                        )
                      }
                      onCorrelate={() =>
                        onNavigate({ name: "correlate", at: node.span.startedAt })
                      }
                    />
                  ))}
                </tbody>
              </table>
            </div>
            <p className="border-t border-line-soft px-3 py-2 text-2xs leading-relaxed text-ink-4">
              Bars are positioned against the whole trace. Colour is the service, matching
              the breakdown; a red bar is a span whose status is error. Click a span for its
              attributes.
            </p>
          </Panel>
        </div>

        <p className="num px-1 text-2xs text-ink-4">
          {trace.traceId} · via {profileName}
        </p>
      </Screen>
    </>
  );
}

function SpanRow({
  node,
  total,
  color,
  open,
  onToggle,
  onCorrelate,
}: {
  node: TraceNode;
  total: number;
  color: string;
  open: boolean;
  onToggle: () => void;
  onCorrelate: () => void;
}) {
  const failed = node.span.status === "error";
  const left = Math.min((node.offsetMs / total) * 100, 100);
  const width = Math.max((node.span.durationMs / total) * 100, MIN_BAR_PERCENT);
  const attributes = Object.entries(node.span.attributes ?? {});
  const events = node.span.events ?? [];

  return (
    <>
      <tr
        className={cx(
          "group cursor-pointer border-t border-line-soft transition-colors hover:bg-raised/60",
          open && "bg-raised/60",
        )}
        onClick={onToggle}
      >
        <td className="max-w-[260px] cell-pad-x py-1">
          <span
            className="flex min-w-0 items-center gap-1.5"
            // Depth as padding rather than nested tables: the bars must share
            // one time axis, and nesting would give each level its own.
            style={{ paddingLeft: `${node.depth * 12}px` }}
          >
            <span
              aria-hidden
              className="size-[7px] shrink-0 rounded-[1px]"
              style={{ background: failed ? "var(--color-crit)" : color }}
            />
            <span
              className={cx(
                "min-w-0 truncate text-xs",
                failed ? "text-crit" : "text-ink-2 group-hover:text-ink",
              )}
              title={`${node.span.service} · ${node.span.name}`}
            >
              {node.span.name}
            </span>
            {node.orphaned && (
              <span
                className="num shrink-0 text-2xs tracking-[.18em] text-warn uppercase"
                title="This span's parent is not in the trace"
              >
                orphan
              </span>
            )}
          </span>
        </td>
        <td className="cell-pad-x py-1">
          <span className="relative block h-[11px] w-full bg-sunk">
            <span
              className="absolute top-0 h-full rounded-[1px]"
              style={{
                left: `${left}%`,
                width: `${width}%`,
                background: failed ? "var(--color-crit)" : color,
              }}
            />
          </span>
        </td>
        <td className="cell-pad-x py-1 text-right">
          <span className="num text-2xs text-ink-2">{formatDuration(node.span.durationMs)}</span>
          <span className="num ml-2 text-2xs text-ink-4" title="Self time">
            {formatDuration(node.selfMs)}
          </span>
        </td>
      </tr>

      {open && (
        <tr className="border-t border-line-soft bg-sunk/40">
          <td colSpan={3} className="cell-pad-x py-2">
            <div className="flex flex-col gap-2" style={{ paddingLeft: `${node.depth * 12}px` }}>
              <div className="flex flex-wrap items-center gap-2">
                <span className="num text-2xs tracking-[.18em] text-ink-4 uppercase">
                  {node.span.service}
                </span>
                <span className="num text-2xs text-ink-4">{node.span.spanId}</span>
                <Button onClick={onCorrelate} title="What every system was doing at this moment">
                  Lanes at this moment
                </Button>
              </div>

              {node.span.statusMessage && (
                <p className="text-xs text-crit">{node.span.statusMessage}</p>
              )}

              {attributes.length > 0 ? (
                <dl className="grid gap-x-4 gap-y-0.5 @xl:grid-cols-2">
                  {attributes.map(([key, value]) => (
                    <div key={key} className="flex min-w-0 gap-2 border-t border-line-soft py-1">
                      <dt className="num w-[38%] shrink-0 truncate text-2xs text-ink-4" title={key}>
                        {key}
                      </dt>
                      {/* Wrapped, not truncated. The one attribute anybody
                          opens a database span to read is `db.statement`, and
                          an ellipsis after four words hides exactly the query
                          they came for. */}
                      <dd className="num min-w-0 flex-1 text-2xs break-words text-ink-2">
                        {value || "—"}
                      </dd>
                    </div>
                  ))}
                </dl>
              ) : (
                <p className="text-2xs text-ink-4">This span carries no attributes.</p>
              )}

              {events.length > 0 && (
                <ul className="flex flex-col gap-0.5 border-t border-line-soft pt-1">
                  {events.map((event, i) => (
                    <li key={i} className="num flex gap-2 text-2xs text-ink-3">
                      <span className="text-ink-4">{formatDateTime(event.at)}</span>
                      <span>{event.name}</span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </td>
        </tr>
      )}
    </>
  );
}

function Sep() {
  return <span className="text-line-strong">·</span>;
}

/** Depth-first, which is the order a waterfall is read in. */
function flatten(nodes: TraceNode[]): TraceNode[] {
  const out: TraceNode[] = [];
  const walk = (list: TraceNode[]) => {
    for (const node of list) {
      out.push(node);
      walk(node.children);
    }
  };
  walk(nodes);
  return out;
}
