// A service's objectives on its own page: how good it has been over each
// window, how much error budget is left, how fast it is going, and the
// budget's line across the window — with the form to set one.

import { useCallback, useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import { DATA_INK } from "../catalog";
import { newId } from "../dashboards";
import { formatMetricValue } from "../format";
import type { Metric, RuleOp, Slo, SloIndicator, SloStatus } from "../types";
import { TimeSeries } from "./charts";
import { Button, cx, Empty, Input, Segmented, Select } from "./primitives";

const OBJECTIVES = [99, 99.5, 99.9, 99.95, 99.99];
const WINDOWS = [7, 28, 30];

/** Burn speeds as the alerts read them: 14.4× pages, 6× warns, under 1× is within budget. */
function burnTone(rate: number | null): string {
  if (rate === null) return "text-ink-4";
  if (rate >= 14.4) return "text-crit";
  if (rate >= 6) return "text-warn";
  return rate > 1 ? "text-ink" : "text-ok";
}

function budgetTone(remaining: number | null): string {
  if (remaining === null) return "text-ink-4";
  if (remaining < 0) return "text-crit";
  return remaining < 0.25 ? "text-warn" : "text-ok";
}

function goodWhen(slo: Slo, metrics: Metric[]): string {
  if (slo.indicator.kind === "up") return "good while up";
  const { metricKey, op, threshold } = slo.indicator;
  const metric = metrics.find((m) => m.key === metricKey);
  return `good while ${metric?.label ?? metricKey} is ${op} ${metric ? formatMetricValue(threshold, metric.unit) : threshold}`;
}

export function Reliability({ profileId, metrics }: { profileId: string; metrics: Metric[] }) {
  const [statuses, setStatuses] = useState<SloStatus[] | null>(null);
  const [error, setError] = useState<string>();
  const [editing, setEditing] = useState<Slo | null>(null);

  const load = useCallback(() => {
    api
      .getSloStatus(profileId)
      .then((s) => {
        setStatuses(s);
        setError(undefined);
      })
      .catch((e) => setError(errorMessage(e)));
  }, [profileId]);

  useEffect(() => {
    load();
    const timer = setInterval(load, 60_000);
    return () => clearInterval(timer);
  }, [load]);

  const blank = (): Slo => ({
    id: newId(),
    profileId,
    name: "",
    objective: 99.9,
    windowDays: 30,
    indicator: { kind: "up" },
  });

  return (
    <div className="flex flex-col gap-3">
      {error && <p className="text-xs text-crit">{error}</p>}
      {statuses && statuses.length === 0 && !editing && (
        <Empty
          title="No objectives yet"
          hint="Say how often this should be good — up 99.9% of 30 days, or error rate under 1% — and root keeps the error budget, warns when it burns fast, and pages when it burns faster."
        />
      )}
      {editing && (
        <SloEditor
          key={editing.id}
          initial={editing}
          metrics={metrics}
          onCancel={() => setEditing(null)}
          onSave={async (slo) => {
            await api.saveSlo(slo);
            setEditing(null);
            load();
          }}
        />
      )}
      {(statuses ?? []).map((status) => {
        const { slo } = status;
        const now = Date.now();
        return (
          <div key={slo.id} className="rounded-md border border-line bg-page p-3">
            <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
              <span className="text-sm text-ink">{slo.name}</span>
              <span className="num text-2xs text-ink-4">
                {slo.objective}% of {slo.windowDays} days, {goodWhen(slo, metrics)}
              </span>
              <span className="ml-auto flex gap-1">
                <Button size="sm" variant="ghost" onClick={() => setEditing(slo)}>
                  Edit
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    if (window.confirm(`Delete the objective "${slo.name}"?`))
                      void api.deleteSlo(slo.id).then(load, (e) => setError(errorMessage(e)));
                  }}
                >
                  Delete
                </Button>
              </span>
            </div>
            <div className="mt-2 grid gap-3 @3xl:grid-cols-[minmax(0,15rem)_1fr]">
              <dl className="grid grid-cols-2 gap-x-4 gap-y-2 self-start">
                <div>
                  <dt className="text-2xs text-ink-4">Good</dt>
                  <dd className={cx("num text-lg", status.sli !== null && status.sli < slo.objective ? "text-warn" : "text-ink")}>
                    {status.sli === null ? "—" : `${status.sli.toFixed(status.sli >= 99 ? 3 : 2)}%`}
                  </dd>
                </div>
                <div>
                  <dt className="text-2xs text-ink-4">Budget left</dt>
                  <dd className={cx("num text-lg", budgetTone(status.remaining))}>
                    {status.remaining === null ? "—" : `${Math.round(status.remaining * 100)}%`}
                  </dd>
                </div>
                <div className="col-span-2">
                  <dt className="text-2xs text-ink-4">Burning, over the last</dt>
                  <dd className="num flex gap-3 text-xs">
                    {(
                      [
                        ["1h", status.burn1h],
                        ["6h", status.burn6h],
                        ["3d", status.burn3d],
                      ] as const
                    ).map(([label, rate]) => (
                      <span key={label} className={burnTone(rate)} title="1× spends the budget exactly over the window">
                        {label} {rate === null ? "—" : `${rate < 10 ? rate.toFixed(1) : Math.round(rate)}×`}
                      </span>
                    ))}
                  </dd>
                </div>
                {status.covered < 0.95 && (
                  <p className="col-span-2 text-2xs text-ink-4">
                    Measured over the {Math.round(status.covered * slo.windowDays * 24)} hours of these {slo.windowDays} days that
                    root has a record of.
                  </p>
                )}
              </dl>
              {status.budget.length > 1 ? (
                <TimeSeries
                  series={[{ key: `budget:${slo.id}`, label: "Budget left", color: DATA_INK, points: status.budget }]}
                  unit="percent"
                  domain={[now - slo.windowDays * 86_400_000, now]}
                  height={120}
                  showLegend={false}
                />
              ) : (
                <p className="self-center text-2xs text-ink-4">No record in this window yet.</p>
              )}
            </div>
          </div>
        );
      })}
      {!editing && (
        <div>
          <Button size="sm" onClick={() => setEditing(blank())}>
            Add an objective
          </Button>
        </div>
      )}
    </div>
  );
}

function SloEditor({
  initial,
  metrics,
  onSave,
  onCancel,
}: {
  initial: Slo;
  metrics: Metric[];
  onSave: (slo: Slo) => Promise<void>;
  onCancel: () => void;
}) {
  const [slo, setSlo] = useState<Slo>(initial);
  const [thresholdText, setThresholdText] = useState(slo.indicator.kind === "metric" ? String(slo.indicator.threshold) : "1");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const indicator = slo.indicator;
  const threshold = Number(thresholdText);
  const ready =
    slo.name.trim() !== "" && (indicator.kind === "up" || (indicator.metricKey !== "" && thresholdText.trim() !== "" && Number.isFinite(threshold)));

  const setIndicator = (next: SloIndicator) => setSlo({ ...slo, indicator: next });

  const save = async () => {
    setSaving(true);
    setError(undefined);
    try {
      await onSave({
        ...slo,
        name: slo.name.trim(),
        indicator: indicator.kind === "metric" ? { ...indicator, threshold } : indicator,
      });
    } catch (e) {
      setError(errorMessage(e));
      setSaving(false);
    }
  };

  return (
    <div className="flex flex-col gap-3 rounded-md border border-line-strong bg-page p-3">
      <div className="flex flex-wrap items-end gap-3">
        <label className="flex min-w-0 flex-col gap-1 text-2xs text-ink-4">
          Name
          <Input value={slo.name} onChange={(e) => setSlo({ ...slo, name: e.target.value })} placeholder="Checkout availability" className="w-60" autoFocus />
        </label>
        <label className="flex flex-col gap-1 text-2xs text-ink-4">
          Good
          <Segmented
            label="Good when"
            value={indicator.kind}
            onChange={(kind) =>
              setIndicator(kind === "up" ? { kind: "up" } : { kind: "metric", metricKey: "", op: "below", threshold: Number(thresholdText) || 0 })
            }
            options={[
              { value: "up", label: "While up" },
              { value: "metric", label: "While a metric holds" },
            ]}
          />
        </label>
        {indicator.kind === "metric" && (
          <>
            <label className="flex flex-col gap-1 text-2xs text-ink-4">
              Metric
              <Select value={indicator.metricKey} onChange={(e) => setIndicator({ ...indicator, metricKey: e.target.value })}>
                <option value="">Choose…</option>
                {metrics.map((m) => (
                  <option key={m.key} value={m.key}>
                    {m.label} · now {formatMetricValue(m.value, m.unit)}
                  </option>
                ))}
              </Select>
            </label>
            <label className="flex flex-col gap-1 text-2xs text-ink-4">
              Is
              <span className="flex items-center gap-2">
                <Segmented
                  label="Direction"
                  value={indicator.op}
                  onChange={(op: RuleOp) => setIndicator({ ...indicator, op })}
                  options={[
                    { value: "below", label: "Below" },
                    { value: "above", label: "Above" },
                  ]}
                />
                <Input className="num w-20" inputMode="decimal" value={thresholdText} onChange={(e) => setThresholdText(e.target.value)} aria-label="Threshold" />
              </span>
            </label>
          </>
        )}
        <label className="flex flex-col gap-1 text-2xs text-ink-4">
          For
          <Select value={String(slo.objective)} onChange={(e) => setSlo({ ...slo, objective: Number(e.target.value) })}>
            {OBJECTIVES.map((o) => (
              <option key={o} value={o}>
                {o}% of the time
              </option>
            ))}
          </Select>
        </label>
        <label className="flex flex-col gap-1 text-2xs text-ink-4">
          Over
          <Select value={String(slo.windowDays)} onChange={(e) => setSlo({ ...slo, windowDays: Number(e.target.value) })}>
            {WINDOWS.map((d) => (
              <option key={d} value={d}>
                the last {d} days
              </option>
            ))}
          </Select>
        </label>
      </div>
      <p className="text-2xs text-ink-4">
        {slo.objective}% of {slo.windowDays} days leaves {Math.round(((100 - slo.objective) / 100) * slo.windowDays * 24 * 60)} minutes of
        budget. Burning it 14.4× as fast pages; 6× warns.
      </p>
      {error && <p className="text-xs text-crit">{error}</p>}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button variant="primary" loading={saving} disabled={!ready} onClick={() => void save()}>
          Save objective
        </Button>
      </div>
    </div>
  );
}
