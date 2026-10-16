// Alert rules people write, previewed against last week before they are
// saved, and silenced from the same screen. A rule watches a metric root
// collects or a pattern in a connection's log lines; it fires once its
// condition has held for its "for", and its alert goes into the same list,
// notifications and history as every other. A silence keeps alerts listed
// but off every channel; one that starts later is a maintenance window.

import { useEffect, useMemo, useState, type ReactNode } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { DATA_INK } from "../catalog";
import { newId } from "../dashboards";
import { fromLocalInput, toLocalInput } from "../explore";
import { formatDateTime, formatMetricValue } from "../format";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import type { Alert, AlertRule, AlertSeverity, AppConfig, MetricUnit, RuleCondition, RulePreview, Silence } from "../types";
import { TimeSeries, type Annotation } from "../ui/charts";
import { Button, cx, Empty, Input, Segmented, Select, Tag } from "../ui/primitives";

const WAITS = [
  { secs: 0, label: "At once" },
  { secs: 60, label: "1m" },
  { secs: 300, label: "5m" },
  { secs: 600, label: "10m" },
  { secs: 900, label: "15m" },
  { secs: 1800, label: "30m" },
  { secs: 3600, label: "1h" },
];

const WINDOWS = [
  { secs: 60, label: "1 minute" },
  { secs: 300, label: "5 minutes" },
  { secs: 900, label: "15 minutes" },
  { secs: 3600, label: "1 hour" },
];

const SEVERITY_COLOR: Record<AlertSeverity, string> = {
  critical: "var(--color-crit)",
  warning: "var(--color-warn)",
  info: "var(--color-ink-3)",
};

const HOUR = 3_600_000;

function waitWords(secs: number): string {
  return WAITS.find((w) => w.secs === secs)?.label ?? `${Math.round(secs / 60)}m`;
}

/** "Error rate on orders-api above 2.0% for 5m" — a rule in one line. */
function describe(rule: AlertRule, model: EstateModel): string {
  const c = rule.condition;
  const service = model.services.find((s) => s.snapshot.profileId === c.profileId);
  const name = service?.snapshot.profileName ?? c.profileId;
  const held = rule.forSecs > 0 ? ` for ${waitWords(rule.forSecs)}` : "";
  if (c.source === "metric") {
    const metric = service?.snapshot.metrics.find((m) => m.key === c.metricKey);
    const value = metric ? formatMetricValue(c.threshold, metric.unit) : String(c.threshold);
    return `${metric?.label ?? c.metricKey} on ${name} ${c.op} ${value}${held}`;
  }
  const window = WINDOWS.find((w) => w.secs === c.windowSecs)?.label ?? `${c.windowSecs}s`;
  return `“${c.pattern}” on ${name} ${c.op} ${c.threshold} lines in ${window}${held}`;
}

function labelsText(labels: Record<string, string>): string {
  return Object.entries(labels)
    .map(([k, v]) => `${k}=${v}`)
    .join(", ");
}

function parseLabels(text: string): Record<string, string> {
  const labels: Record<string, string> = {};
  for (const part of text.split(",")) {
    const at = part.indexOf("=");
    if (at <= 0) continue;
    const key = part.slice(0, at).trim();
    if (key) labels[key] = part.slice(at + 1).trim();
  }
  return labels;
}

export function AlertRules({
  config,
  model,
  alerts,
  syncedAt,
  onConfigChange,
  onNavigate,
  onRefresh,
  onAsk,
}: {
  config: AppConfig | null;
  model: EstateModel;
  catalog: Catalog;
  alerts: Alert[];
  syncedAt?: string;
  onConfigChange: (config: AppConfig) => void;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onAsk: () => void;
}) {
  const rules = config?.alertRules ?? [];
  const silences = config?.silences ?? [];
  const [editing, setEditing] = useState<AlertRule | null>(null);
  const [error, setError] = useState<string>();

  const firing = useMemo(() => {
    const byRule = new Map<string, Alert>();
    for (const a of alerts) if (a.active && a.fingerprint.startsWith("rule:")) byRule.set(a.fingerprint.slice(5), a);
    return byRule;
  }, [alerts]);

  const act = async (work: () => Promise<AppConfig>) => {
    setError(undefined);
    try {
      onConfigChange(await work());
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  const silenceRule = (rule: AlertRule, hours: number) => {
    const now = Date.now();
    void act(() =>
      api.saveSilence({
        id: newId(),
        ruleId: rule.id,
        startsAt: new Date(now).toISOString(),
        endsAt: new Date(now + hours * HOUR).toISOString(),
        comment: "",
      }),
    );
  };

  const firstService = model.services[0]?.snapshot.profileId ?? "";
  const blank = (): AlertRule => ({
    id: newId(),
    name: "",
    enabled: true,
    severity: "warning",
    labels: {},
    condition: { source: "metric", profileId: firstService, metricKey: "", op: "above", threshold: 0 },
    forSecs: 300,
  });

  const firingCount = rules.filter((r) => firing.has(r.id)).length;
  return (
    <>
      <ScreenHeader
        title="Alert rules"
        meta={`${rules.length} rule${rules.length === 1 ? "" : "s"} · ${firingCount} firing · ${silences.length} silence${silences.length === 1 ? "" : "s"}`}
        syncedAt={syncedAt}
        onRefresh={onRefresh}
        onAsk={onAsk}
      >
        <Button variant="ghost" onClick={() => onNavigate({ name: "alerts" })}>
          All alerts
        </Button>
        <Button variant="primary" onClick={() => setEditing(blank())} disabled={!firstService}>
          New rule
        </Button>
      </ScreenHeader>
      <Screen className="flex flex-col gap-4">
        {error && <p className="text-xs text-crit">{error}</p>}

        {editing && (
          <RuleEditor
            key={editing.id}
            initial={editing}
            model={model}
            isNew={!rules.some((r) => r.id === editing.id)}
            onCancel={() => setEditing(null)}
            onSave={async (rule) => {
              onConfigChange(await api.saveAlertRule(rule));
              setEditing(null);
            }}
          />
        )}

        <section className="flex flex-col gap-2" aria-label="Rules">
          {rules.length === 0 ? (
            !editing && (
              <Empty
                title="No rules yet"
                hint="Write one for what the collectors cannot know: an error rate that matters to you, or a log line that should never appear. Each is checked against last week before you save it."
              />
            )
          ) : (
            rules.map((rule) => {
              const alert = firing.get(rule.id);
              const quietUntil = silences
                .filter((s) => s.ruleId === rule.id && Date.parse(s.startsAt) <= Date.now() && Date.now() < Date.parse(s.endsAt))
                .map((s) => s.endsAt)
                .sort()
                .pop();
              const rowSilence = silences.find((s) => s.ruleId === rule.id && s.endsAt === quietUntil);
              return (
                <div key={rule.id} className={cx("panel flex flex-wrap items-center gap-x-3 gap-y-2 p-3", !rule.enabled && "opacity-60")}>
                  <span className="h-2 w-2 shrink-0 rounded-full" style={{ background: SEVERITY_COLOR[rule.severity] }} title={rule.severity} />
                  <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                    <span className="flex flex-wrap items-center gap-2">
                      <span className="text-sm text-ink">{rule.name}</span>
                      {!rule.enabled && <Tag>Off</Tag>}
                      {alert ? (
                        <Tag color={SEVERITY_COLOR[rule.severity]} title={alert.detail}>
                          Firing since {formatDateTime(alert.firstSeen)}
                        </Tag>
                      ) : (
                        rule.enabled && <Tag color="var(--color-ok)">Quiet</Tag>
                      )}
                      {quietUntil && <Tag title="Listed, but kept off every channel">Silenced until {formatDateTime(quietUntil)}</Tag>}
                      {Object.entries(rule.labels).map(([k, v]) => (
                        <Tag key={k}>
                          {k}={v}
                        </Tag>
                      ))}
                    </span>
                    <span className="num truncate text-2xs text-ink-4" title={describe(rule, model)}>
                      {describe(rule, model)}
                    </span>
                  </div>
                  <span className="flex shrink-0 flex-wrap items-center gap-1">
                    {rowSilence ? (
                      <Button size="sm" variant="ghost" onClick={() => void act(() => api.deleteSilence(rowSilence.id))}>
                        Lift silence
                      </Button>
                    ) : (
                      <span className="flex items-center gap-0.5 text-2xs text-ink-4">
                        Silence
                        {[1, 4, 24].map((h) => (
                          <Button key={h} size="sm" variant="ghost" onClick={() => silenceRule(rule, h)} title={`Keep this rule off every channel for ${h}h`}>
                            {h === 24 ? "1d" : `${h}h`}
                          </Button>
                        ))}
                      </span>
                    )}
                    <Button size="sm" variant="ghost" onClick={() => void act(() => api.saveAlertRule({ ...rule, enabled: !rule.enabled }))}>
                      {rule.enabled ? "Turn off" : "Turn on"}
                    </Button>
                    <Button size="sm" onClick={() => setEditing(rule)}>
                      Edit
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => {
                        if (window.confirm(`Delete the rule "${rule.name}"?`)) void act(() => api.deleteAlertRule(rule.id));
                      }}
                    >
                      Delete
                    </Button>
                  </span>
                </div>
              );
            })
          )}
        </section>

        <SilencesPanel silences={silences} rules={rules} model={model} onChange={act} />
      </Screen>
    </>
  );
}

// ----------------------------------------------------------------- editor

function RuleEditor({
  initial,
  model,
  isNew,
  onSave,
  onCancel,
}: {
  initial: AlertRule;
  model: EstateModel;
  isNew: boolean;
  onSave: (rule: AlertRule) => Promise<void>;
  onCancel: () => void;
}) {
  const [rule, setRule] = useState<AlertRule>(initial);
  const [thresholdText, setThresholdText] = useState(String(initial.condition.threshold));
  const [labels, setLabels] = useState(labelsText(initial.labels));
  const [preview, setPreview] = useState<{ result?: RulePreview; error?: string; loading: boolean }>({ loading: false });
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string>();

  const c = rule.condition;
  const service = model.services.find((s) => s.snapshot.profileId === c.profileId);
  const metric = c.source === "metric" ? service?.snapshot.metrics.find((m) => m.key === c.metricKey) : undefined;
  const unit: MetricUnit = metric?.unit ?? "count";
  const threshold = Number(thresholdText);
  const thresholdOk = thresholdText.trim() !== "" && Number.isFinite(threshold);
  const ready =
    thresholdOk && (c.source === "metric" ? c.metricKey !== "" : c.pattern.trim() !== "") && Boolean(c.profileId);

  const setCondition = (next: Partial<RuleCondition>) =>
    setRule((r) => ({ ...r, condition: { ...r.condition, ...next } as RuleCondition }));

  // The preview follows the form: what this rule would have done last week
  // is the thing to know before saving it, so nobody should have to ask.
  const asked = JSON.stringify({ ...rule, name: "", labels: {}, condition: { ...c, threshold } });
  useEffect(() => {
    if (!ready) {
      setPreview({ loading: false });
      return;
    }
    let live = true;
    setPreview((p) => ({ ...p, loading: true, error: undefined }));
    const timer = setTimeout(() => {
      api
        .previewAlertRule({ ...rule, name: rule.name.trim() || "Preview", condition: { ...c, threshold } as RuleCondition })
        .then((result) => live && setPreview({ result, loading: false }))
        .catch((e) => live && setPreview({ error: errorMessage(e), loading: false }));
    }, 400);
    return () => {
      live = false;
      clearTimeout(timer);
    };
    // `asked` is the whole form, minus what does not change the replay.
  }, [asked, ready]); // eslint-disable-line react-hooks/exhaustive-deps

  const save = async () => {
    setSaving(true);
    setSaveError(undefined);
    try {
      await onSave({ ...rule, name: rule.name.trim(), labels: parseLabels(labels), condition: { ...c, threshold } as RuleCondition });
    } catch (e) {
      setSaveError(errorMessage(e));
      setSaving(false);
    }
  };

  const now = Date.now();
  const firings = preview.result?.firings ?? [];
  const annotations: Annotation[] = firings.slice(0, 40).map((f, i) => ({
    id: `f${i}`,
    t: Date.parse(f.start),
    color: SEVERITY_COLOR[rule.severity],
    label: "fires",
    detail: `${formatDateTime(f.start)} → ${f.ongoing ? "now" : formatDateTime(f.end)}`,
  }));

  return (
    <section className="panel flex flex-col gap-3 p-4" aria-label={isNew ? "New rule" : "Edit rule"}>
      <div className="grid gap-3 @3xl:grid-cols-[1fr_1fr]">
        <Field label="Name">
          <Input
            value={rule.name}
            onChange={(e) => setRule({ ...rule, name: e.target.value })}
            placeholder="Checkout error rate"
            autoFocus
          />
        </Field>
        <Field label="Connection">
          <Select
            value={c.profileId}
            onChange={(e) =>
              setCondition(c.source === "metric" ? { profileId: e.target.value, metricKey: "" } : { profileId: e.target.value })
            }
          >
            {model.services.map((s) => (
              <option key={s.snapshot.profileId} value={s.snapshot.profileId}>
                {s.snapshot.profileName}
              </option>
            ))}
          </Select>
        </Field>
      </div>

      <div className="flex flex-wrap items-end gap-3">
        <Field label="Watch">
          <Segmented
            label="Watch"
            value={c.source}
            onChange={(source) =>
              setRule((r) => ({
                ...r,
                condition:
                  source === "metric"
                    ? { source, profileId: c.profileId, metricKey: "", op: c.op, threshold }
                    : { source, profileId: c.profileId, pattern: "", windowSecs: 300, op: "above", threshold },
              }))
            }
            options={[
              { value: "metric", label: "A metric" },
              { value: "log", label: "Log lines" },
            ]}
          />
        </Field>
        {c.source === "metric" ? (
          <Field label="Metric">
            <Select value={c.metricKey} onChange={(e) => setCondition({ metricKey: e.target.value })}>
              <option value="">Choose…</option>
              {(service?.snapshot.metrics ?? []).map((m) => (
                <option key={m.key} value={m.key}>
                  {m.label} · now {formatMetricValue(m.value, m.unit)}
                </option>
              ))}
            </Select>
          </Field>
        ) : (
          <>
            <Field label="Lines containing" hint="Text, or /a regex/">
              <Input
                className="w-56 font-mono"
                value={c.pattern}
                onChange={(e) => setCondition({ pattern: e.target.value })}
                placeholder="payment declined"
              />
            </Field>
            <Field label="Counted over">
              <Select value={String(c.windowSecs)} onChange={(e) => setCondition({ windowSecs: Number(e.target.value) })}>
                {WINDOWS.map((w) => (
                  <option key={w.secs} value={w.secs}>
                    {w.label}
                  </option>
                ))}
              </Select>
            </Field>
          </>
        )}
        <Field label="Fires when it is">
          <span className="flex items-center gap-2">
            <Segmented
              label="Direction"
              value={c.op}
              onChange={(op) => setCondition({ op })}
              options={[
                { value: "above", label: "Above" },
                { value: "below", label: "Below" },
              ]}
            />
            <Input
              className="w-24 num"
              inputMode="decimal"
              value={thresholdText}
              onChange={(e) => setThresholdText(e.target.value)}
              invalid={!thresholdOk}
              aria-label="Threshold"
            />
            <span className="text-2xs text-ink-4">{c.source === "log" ? "lines" : unitWord(unit)}</span>
          </span>
        </Field>
        <Field label="For">
          <Select value={String(rule.forSecs)} onChange={(e) => setRule({ ...rule, forSecs: Number(e.target.value) })}>
            {WAITS.map((w) => (
              <option key={w.secs} value={w.secs}>
                {w.label}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Severity">
          <Segmented
            label="Severity"
            value={rule.severity}
            onChange={(severity) => setRule({ ...rule, severity })}
            options={[
              { value: "warning", label: "Warning" },
              { value: "critical", label: "Critical" },
              { value: "info", label: "Info" },
            ]}
          />
        </Field>
      </div>

      <Field label="Labels" hint="team=payments, runbook=https://…">
        <Input value={labels} onChange={(e) => setLabels(e.target.value)} placeholder="team=payments" />
      </Field>

      <div className="flex flex-col gap-2 rounded-md border border-line bg-page p-3">
        <p className="text-xs text-ink-2">
          {!ready
            ? "Pick what to watch and a threshold, and the last week replays here."
            : preview.loading && !preview.result
              ? "Replaying the last week…"
              : preview.error
                ? <span className="text-crit">{preview.error}</span>
                : preview.result?.summary}
        </p>
        {ready && preview.result?.series && (
          <TimeSeries
            series={[{ key: "preview", label: preview.result.series.label, color: DATA_INK, points: preview.result.series.points, warnAbove: preview.result.series.warnAbove }]}
            unit={preview.result.series.unit}
            domain={[now - 7 * 24 * HOUR, now]}
            height={180}
            showLegend={false}
            annotations={annotations}
          />
        )}
        {firings.length > 0 && (
          <ul className="flex flex-col gap-0.5 text-2xs text-ink-3">
            {firings.slice(-6).reverse().map((f) => (
              <li key={f.start} className="num">
                {formatDateTime(f.start)} → {f.ongoing ? "now" : formatDateTime(f.end)} · {c.op === "above" ? "peak" : "low"}{" "}
                {c.source === "log" ? `${f.peak} lines` : formatMetricValue(f.peak, unit)}
              </li>
            ))}
            {firings.length > 6 && <li>and {firings.length - 6} earlier</li>}
          </ul>
        )}
      </div>

      {saveError && <p className="text-xs text-crit">{saveError}</p>}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button variant="primary" loading={saving} disabled={!ready || !rule.name.trim()} onClick={() => void save()}>
          {isNew ? "Save rule" : "Save changes"}
        </Button>
      </div>
    </section>
  );
}

function unitWord(unit: MetricUnit): string {
  return { count: "", percent: "%", bytes: "bytes", millis: "ms", seconds: "s", per_second: "/s", bytes_per_second: "bytes/s" }[unit];
}

function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="flex min-w-0 flex-col gap-1 text-2xs text-ink-4">
      <span>
        {label}
        {hint && <span className="ml-1.5 text-ink-4/70">{hint}</span>}
      </span>
      {children}
    </label>
  );
}

// --------------------------------------------------------------- silences

function SilencesPanel({
  silences,
  rules,
  model,
  onChange,
}: {
  silences: Silence[];
  rules: AlertRule[];
  model: EstateModel;
  onChange: (work: () => Promise<AppConfig>) => Promise<void>;
}) {
  const now = Date.now();
  const [profileId, setProfileId] = useState(model.services[0]?.snapshot.profileId ?? "");
  const [from, setFrom] = useState(() => toLocalInput(now));
  const [to, setTo] = useState(() => toLocalInput(now + 2 * HOUR));
  const [comment, setComment] = useState("");
  const start = fromLocalInput(from);
  const end = fromLocalInput(to);
  const valid = Boolean(profileId) && start !== null && end !== null && end > start && end > now;

  const target = (s: Silence) => {
    const parts: string[] = [];
    if (s.ruleId) parts.push(`rule “${rules.find((r) => r.id === s.ruleId)?.name ?? "deleted"}”`);
    if (s.profileId) parts.push(model.services.find((x) => x.snapshot.profileId === s.profileId)?.snapshot.profileName ?? s.profileId);
    if (s.fingerprint) parts.push(s.fingerprint);
    return parts.join(" on ");
  };

  const sorted = [...silences].sort((a, b) => a.startsAt.localeCompare(b.startsAt));
  return (
    <section className="panel flex flex-col gap-3 p-4" aria-label="Silences and maintenance windows">
      <div>
        <h2 className="text-sm text-ink">Silences and maintenance windows</h2>
        <p className="mt-0.5 text-2xs text-ink-4">
          Silenced alerts still open, resolve and show in the list; no channel is told about them.
        </p>
      </div>
      {sorted.length > 0 && (
        <ul className="flex flex-col divide-y divide-line rounded-md border border-line">
          {sorted.map((s) => {
            const running = Date.parse(s.startsAt) <= now;
            return (
              <li key={s.id} className="flex flex-wrap items-center gap-2 px-3 py-2 text-xs">
                <Tag color={running ? "var(--color-warn)" : undefined}>{running ? "Silencing" : "Scheduled"}</Tag>
                <span className="text-ink-2">{target(s)}</span>
                <span className="num text-2xs text-ink-4">
                  {running ? `until ${formatDateTime(s.endsAt)}` : `${formatDateTime(s.startsAt)} → ${formatDateTime(s.endsAt)}`}
                </span>
                {s.comment && <span className="text-2xs text-ink-3">— {s.comment}</span>}
                <span className="ml-auto">
                  <Button size="sm" variant="ghost" onClick={() => void onChange(() => api.deleteSilence(s.id))}>
                    {running ? "End now" : "Cancel"}
                  </Button>
                </span>
              </li>
            );
          })}
        </ul>
      )}
      <div className="flex flex-wrap items-end gap-3">
        <Field label="Maintenance on">
          <Select value={profileId} onChange={(e) => setProfileId(e.target.value)}>
            {model.services.map((s) => (
              <option key={s.snapshot.profileId} value={s.snapshot.profileId}>
                {s.snapshot.profileName}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="From">
          <Input type="datetime-local" value={from} onChange={(e) => setFrom(e.target.value)} />
        </Field>
        <Field label="To">
          <Input type="datetime-local" value={to} onChange={(e) => setTo(e.target.value)} invalid={end !== null && start !== null && end <= start} />
        </Field>
        <Field label="Why">
          <Input value={comment} onChange={(e) => setComment(e.target.value)} placeholder="Postgres 16 upgrade" className="w-56" />
        </Field>
        <Button
          disabled={!valid}
          onClick={() =>
            void onChange(() =>
              api.saveSilence({
                id: newId(),
                profileId,
                startsAt: new Date(start ?? now).toISOString(),
                endsAt: new Date(end ?? now).toISOString(),
                comment: comment.trim(),
              }),
            ).then(() => setComment(""))
          }
        >
          Schedule
        </Button>
      </div>
    </section>
  );
}
