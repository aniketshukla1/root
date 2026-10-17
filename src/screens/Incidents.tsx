// Incidents as records that outlast their alerts: who owns one, where it
// stands, what happened in order, what was said to customers, and a
// postmortem draft when it is over — and the status page that shows the
// public half of all of it.

import { useCallback, useEffect, useState, type ReactNode } from "react";

import { api, errorMessage } from "../api";
import { formatDateTime, formatRelative } from "../format";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import type {
  AppConfig,
  Health,
  IncidentEntry,
  IncidentRecord,
  IncidentStatus,
  StatusPageSettings,
  StatusPageView,
} from "../types";
import { Button, cx, Empty, Input, Segmented, Tag } from "../ui/primitives";

const SEVERITY_COLOR = { critical: "var(--color-crit)", warning: "var(--color-warn)", info: "var(--color-ink-3)" } as const;
const STATUS_LABEL: Record<IncidentStatus, string> = {
  investigating: "Investigating",
  identified: "Identified",
  monitoring: "Monitoring",
  resolved: "Resolved",
};
const ENTRY_COLOR: Partial<Record<IncidentEntry["kind"], string>> = {
  opened: "var(--color-crit)",
  escalated: "var(--color-crit)",
  resolved: "var(--color-ok)",
  reopened: "var(--color-warn)",
  update: "var(--color-accent)",
};

function lasted(record: IncidentRecord): string {
  const end = record.resolvedAt ? Date.parse(record.resolvedAt) : Date.now();
  const minutes = Math.max(1, Math.round((end - Date.parse(record.openedAt)) / 60_000));
  const span = minutes >= 60 ? `${Math.floor(minutes / 60)}h ${minutes % 60}m` : `${minutes}m`;
  return record.resolvedAt ? `lasted ${span}` : `open ${span}`;
}

export function Incidents({
  model,
  selected,
  serverUrl,
  onConfigChange,
  onNavigate,
  onAsk,
}: {
  model: EstateModel;
  selected?: string;
  /** Where root-server publishes the status page, when there is one. */
  serverUrl?: string;
  onConfigChange: (config: AppConfig) => void;
  onNavigate: (view: View) => void;
  onAsk: () => void;
}) {
  const [tab, setTab] = useState<"incidents" | "status">("incidents");
  const [records, setRecords] = useState<IncidentRecord[] | null>(null);
  const [error, setError] = useState<string>();
  const [openId, setOpenId] = useState<string | undefined>(selected);

  const load = useCallback(() => {
    api
      .getIncidentLog()
      .then((r) => {
        setRecords(r);
        setError(undefined);
      })
      .catch((e) => setError(errorMessage(e)));
  }, []);
  useEffect(() => {
    load();
    const timer = setInterval(load, 20_000);
    return () => clearInterval(timer);
  }, [load]);

  const nameOf = (id: string) => model.services.find((s) => s.snapshot.profileId === id)?.snapshot.profileName ?? id;
  const open = records?.find((r) => r.id === openId) ?? records?.find((r) => !r.resolvedAt) ?? records?.[0];
  const replace = (record: IncidentRecord) => setRecords((all) => all?.map((r) => (r.id === record.id ? record : r)) ?? null);
  const active = (records ?? []).filter((r) => !r.resolvedAt).length;

  return (
    <>
      <ScreenHeader title="Incidents" meta={records ? `${active} open · ${records.length} on record` : undefined} onRefresh={load} onAsk={onAsk}>
        <Segmented
          label="View"
          value={tab}
          onChange={setTab}
          options={[
            { value: "incidents", label: "Incidents" },
            { value: "status", label: "Status page" },
          ]}
        />
      </ScreenHeader>
      <Screen className="@container">
        {error && <p className="mb-3 text-xs text-crit">{error}</p>}
        {tab === "status" ? (
          <StatusPageTab model={model} serverUrl={serverUrl} onConfigChange={onConfigChange} />
        ) : records && records.length === 0 ? (
          <Empty title="No incidents on record" hint="When alerts are grouped into an incident, it is recorded here — and stays after its alerts clear." />
        ) : (
          <div className="grid gap-4 @4xl:grid-cols-[22rem_minmax(0,1fr)]">
            <ul className="flex flex-col gap-1.5 self-start" aria-label="Incidents">
              {(records ?? []).map((r) => (
                <li key={r.id}>
                  <button
                    type="button"
                    onClick={() => setOpenId(r.id)}
                    className={cx(
                      "panel row-hover flex w-full flex-col gap-1 p-2.5 text-left",
                      open?.id === r.id && "border-ink-4/60",
                    )}
                  >
                    <span className="flex items-center gap-2">
                      <span className="h-2 w-2 shrink-0 rounded-full" style={{ background: r.resolvedAt ? "var(--color-ok)" : SEVERITY_COLOR[r.severity] }} />
                      <span className="min-w-0 flex-1 truncate text-xs text-ink">{r.title}</span>
                    </span>
                    <span className="num flex flex-wrap gap-x-2 text-2xs text-ink-4">
                      <span className={r.resolvedAt ? "text-ok" : "text-ink-3"}>{STATUS_LABEL[r.status]}</span>
                      <span>{r.owner ?? "no owner"}</span>
                      <span>{formatRelative(r.openedAt)}</span>
                      <span>{lasted(r)}</span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            {open && <IncidentDetail key={open.id} record={open} nameOf={nameOf} onChange={replace} onNavigate={onNavigate} />}
          </div>
        )}
      </Screen>
    </>
  );
}

function IncidentDetail({
  record,
  nameOf,
  onChange,
  onNavigate,
}: {
  record: IncidentRecord;
  nameOf: (id: string) => string;
  onChange: (record: IncidentRecord) => void;
  onNavigate: (view: View) => void;
}) {
  const [owner, setOwner] = useState(record.owner ?? "");
  const [composer, setComposer] = useState<"note" | "update">("note");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string>();
  const resolved = Boolean(record.resolvedAt);

  const run = async (what: string, work: () => Promise<IncidentRecord>) => {
    setBusy(what);
    setError(undefined);
    try {
      onChange(await work());
      return true;
    } catch (e) {
      setError(errorMessage(e));
      return false;
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="panel flex min-w-0 flex-col gap-4 p-4" aria-label={record.title}>
      <div className="flex flex-col gap-2">
        <h2 className="text-base text-ink">{record.title}</h2>
        <div className="flex flex-wrap items-center gap-2 text-2xs text-ink-4">
          <Tag color={SEVERITY_COLOR[record.severity]}>{record.severity}</Tag>
          <span className="num">
            {formatDateTime(record.openedAt)} · {lasted(record)}
          </span>
          {record.profileIds.map((id) => (
            <button
              key={id}
              type="button"
              onClick={() => onNavigate({ name: "service", profileId: id })}
              className="row-hover rounded-sm px-1.5 py-0.5 font-mono text-ink-3"
            >
              {nameOf(id)}
            </button>
          ))}
          <Button size="sm" variant="ghost" onClick={() => onNavigate({ name: "map" })}>
            On the map
          </Button>
        </div>
      </div>

      <div className="flex flex-wrap items-end gap-4">
        <label className="flex flex-col gap-1 text-2xs text-ink-4">
          Status
          {resolved ? (
            <span className="text-xs text-ok">Resolved {formatDateTime(record.resolvedAt!)}</span>
          ) : (
            <Segmented
              label="Status"
              value={record.status}
              onChange={(status) => void run("status", () => api.updateIncident(record.id, { kind: "status", status: status as Exclude<IncidentStatus, "resolved"> }))}
              options={(["investigating", "identified", "monitoring"] as const).map((s) => ({ value: s, label: STATUS_LABEL[s] }))}
            />
          )}
        </label>
        <label className="flex flex-col gap-1 text-2xs text-ink-4">
          Owner
          <span className="flex items-center gap-1.5">
            <Input size="sm" value={owner} onChange={(e) => setOwner(e.target.value)} placeholder="Who has it" className="w-44" />
            <Button
              size="sm"
              disabled={(owner.trim() || null) === (record.owner ?? null)}
              loading={busy === "owner"}
              onClick={() => void run("owner", () => api.updateIncident(record.id, { kind: "owner", owner: owner.trim() || null }))}
            >
              {owner.trim() ? "Assign" : "Clear"}
            </Button>
          </span>
        </label>
      </div>
      {error && <p className="text-xs text-crit">{error}</p>}

      <div className="flex flex-col gap-2">
        <h3 className="text-2xs tracking-[0.08em] text-ink-4 uppercase">Timeline</h3>
        <ol className="flex flex-col">
          {record.timeline.map((entry, i) => (
            <li key={`${entry.at}-${i}`} className="grid grid-cols-[4.5rem_0.75rem_minmax(0,1fr)] gap-2 py-1">
              <time className="num pt-px text-2xs text-ink-4" title={formatDateTime(entry.at)}>
                {new Date(entry.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })}
              </time>
              <span className="mt-1.5 h-2 w-2 rounded-full" style={{ background: ENTRY_COLOR[entry.kind] ?? "var(--color-ink-4)" }} />
              <span className="text-xs text-ink-2">
                {entry.kind === "update" && <Tag className="mr-1.5">Public</Tag>}
                {entry.text}
                {entry.author && <span className="text-ink-4"> — {entry.author}</span>}
              </span>
            </li>
          ))}
        </ol>
        {!resolved && (
          <div className="flex flex-col gap-2 rounded-md border border-line p-2.5">
            <Segmented
              label="Write"
              value={composer}
              onChange={setComposer}
              options={[
                { value: "note", label: "Note for the team" },
                { value: "update", label: "Public update" },
              ]}
            />
            <textarea
              value={text}
              onChange={(e) => setText(e.target.value)}
              rows={2}
              placeholder={composer === "note" ? "What you found, what you tried" : "What customers see on the status page"}
              className="rounded-md border border-line-strong bg-sunk px-2.5 py-2 text-xs text-ink placeholder:text-ink-4 focus:border-ink/45 focus:outline-none"
            />
            <div className="flex justify-end">
              <Button
                size="sm"
                variant="primary"
                disabled={!text.trim()}
                loading={busy === "post"}
                onClick={() =>
                  void run("post", () => api.updateIncident(record.id, { kind: composer, text })).then((ok) => ok && setText(""))
                }
              >
                {composer === "note" ? "Add note" : "Post update"}
              </Button>
            </div>
          </div>
        )}
      </div>

      <div className="flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <h3 className="text-2xs tracking-[0.08em] text-ink-4 uppercase">Postmortem</h3>
          <span className="ml-auto flex gap-1">
            {record.postmortem && (
              <Button size="sm" variant="ghost" onClick={() => void navigator.clipboard.writeText(record.postmortem!).catch(() => undefined)}>
                Copy
              </Button>
            )}
            <Button size="sm" loading={busy === "postmortem"} onClick={() => void run("postmortem", () => api.draftPostmortem(record.id))}>
              {record.postmortem ? "Redraft" : "Draft a postmortem"}
            </Button>
          </span>
        </div>
        {record.postmortem ? (
          <Markdown text={record.postmortem} />
        ) : (
          <p className="text-2xs text-ink-4">
            Drafted by the assistant from this record alone — the timeline, owner, notes and updates — with anything it
            cannot know marked “To confirm”.
          </p>
        )}
      </div>
    </section>
  );
}

/** The few shapes a postmortem draft uses: headings, bullets, paragraphs. */
function Markdown({ text }: { text: string }) {
  const blocks: ReactNode[] = [];
  let bullets: string[] = [];
  const flush = () => {
    if (bullets.length === 0) return;
    blocks.push(
      <ul key={`ul-${blocks.length}`} className="list-disc pl-5 text-xs text-ink-2">
        {bullets.map((b, i) => (
          <li key={i}>{b}</li>
        ))}
      </ul>,
    );
    bullets = [];
  };
  for (const line of text.split("\n")) {
    if (/^\s*[-*] /.test(line)) {
      bullets.push(line.replace(/^\s*[-*] /, ""));
      continue;
    }
    flush();
    if (line.startsWith("#")) blocks.push(<h4 key={blocks.length} className="mt-2 text-xs text-ink">{line.replace(/^#+\s*/, "")}</h4>);
    else if (line.trim()) blocks.push(<p key={blocks.length} className="text-xs leading-relaxed text-ink-2">{line}</p>);
  }
  flush();
  return <div className="flex flex-col gap-1 rounded-md border border-line bg-page p-3">{blocks}</div>;
}

// ------------------------------------------------------------ status page

const HEALTH_WORD: Record<Health, [string, string]> = {
  healthy: ["Operational", "text-ok"],
  degraded: ["Degraded", "text-warn"],
  down: ["Down", "text-crit"],
  unknown: ["Unknown", "text-ink-4"],
};

function StatusPageTab({
  model,
  serverUrl,
  onConfigChange,
}: {
  model: EstateModel;
  serverUrl?: string;
  onConfigChange: (config: AppConfig) => void;
}) {
  const [view, setView] = useState<StatusPageView | null>(null);
  const [draft, setDraft] = useState<StatusPageSettings | null>(null);
  const [error, setError] = useState<string>();
  const [saving, setSaving] = useState(false);

  const load = useCallback(() => {
    api
      .getStatusPage()
      .then((v) => {
        setView(v);
        setDraft((d) => d ?? v.settings);
      })
      .catch((e) => setError(errorMessage(e)));
  }, []);
  useEffect(() => load(), [load]);

  if (!draft || !view) return error ? <p className="text-xs text-crit">{error}</p> : null;

  const save = async () => {
    setSaving(true);
    setError(undefined);
    try {
      onConfigChange(await api.saveStatusPage(draft));
      load();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSaving(false);
    }
  };
  const [headline, tone] =
    view.overall === "healthy"
      ? ["All systems operational", "bg-ok/15 text-ok"]
      : view.overall === "degraded"
        ? ["Some systems are degraded", "bg-warn/15 text-warn"]
        : view.overall === "down"
          ? ["Some systems are down", "bg-crit/15 text-crit"]
          : ["Status unknown", "bg-raised text-ink-3"];

  return (
    <div className="grid gap-4 @4xl:grid-cols-[22rem_minmax(0,1fr)]">
      <section className="panel flex flex-col gap-3 self-start p-4" aria-label="Status page settings">
        <label className="flex items-center gap-2 text-xs text-ink-2">
          <input type="checkbox" checked={draft.enabled} onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })} className="accent-accent" />
          Publish the status page
        </label>
        <label className="flex flex-col gap-1 text-2xs text-ink-4">
          Title
          <Input value={draft.title} onChange={(e) => setDraft({ ...draft, title: e.target.value })} placeholder="Acme status" />
        </label>
        <fieldset className="flex flex-col gap-1 text-2xs text-ink-4">
          <legend className="mb-1">Shows — none ticked shows every connection</legend>
          {model.services.map((s) => (
            <label key={s.snapshot.profileId} className="flex items-center gap-2 text-xs text-ink-2">
              <input
                type="checkbox"
                className="accent-accent"
                checked={draft.profileIds.includes(s.snapshot.profileId)}
                onChange={(e) =>
                  setDraft({
                    ...draft,
                    profileIds: e.target.checked
                      ? [...draft.profileIds, s.snapshot.profileId]
                      : draft.profileIds.filter((id) => id !== s.snapshot.profileId),
                  })
                }
              />
              {s.snapshot.profileName}
            </label>
          ))}
        </fieldset>
        <p className="text-2xs leading-relaxed text-ink-4">
          {serverUrl ? (
            <>
              Published at <span className="font-mono text-ink-3">{serverUrl.replace(/\/$/, "")}/status</span> while on. It shows only
              the connections ticked and the updates written as public — never notes or names.
            </>
          ) : (
            <>root-server publishes it at /status while on. It shows only the connections ticked and the updates written as public — never notes or names.</>
          )}
        </p>
        {error && <p className="text-xs text-crit">{error}</p>}
        <div>
          <Button variant="primary" loading={saving} onClick={() => void save()}>
            Save
          </Button>
        </div>
      </section>

      <section className="panel flex flex-col gap-4 p-5" aria-label="Status page preview">
        <span className="text-2xs tracking-[0.08em] text-ink-4 uppercase">Preview</span>
        <h2 className="text-lg text-ink">{view.title}</h2>
        <div className={cx("rounded-md px-3 py-2.5 text-sm font-semibold", tone)}>{headline}</div>
        <ul className="divide-y divide-line rounded-md border border-line">
          {view.components.map((c) => (
            <li key={c.name} className="flex justify-between px-3 py-2 text-xs">
              <span className="text-ink-2">{c.name}</span>
              <span className={HEALTH_WORD[c.health][1]}>{HEALTH_WORD[c.health][0]}</span>
            </li>
          ))}
        </ul>
        <div className="flex flex-col gap-2">
          <h3 className="text-xs text-ink-3">Incidents</h3>
          {view.incidents.length === 0 ? (
            <p className="text-2xs text-ink-4">No incidents in the last seven days.</p>
          ) : (
            view.incidents.map((i) => (
              <article key={`${i.title}-${i.openedAt}`} className="rounded-md border border-line p-3">
                <h4 className="text-xs text-ink">{i.title}</h4>
                <p className="num text-2xs text-ink-4">
                  {i.resolvedAt ? "Resolved" : STATUS_LABEL[i.status]} · since {formatDateTime(i.openedAt)}
                </p>
                {(i.updates.length > 0 ? i.updates : [{ at: i.openedAt, text: "We are looking into it." }]).map((u) => (
                  <p key={u.at} className="mt-1 text-xs text-ink-2">
                    <span className="num mr-1.5 text-2xs text-ink-4">{formatDateTime(u.at)}</span>
                    {u.text}
                  </p>
                ))}
              </article>
            ))
          )}
        </div>
      </section>
    </div>
  );
}
