import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { listen } from "../events";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { formatDuration, formatRelative } from "../format";
import { suggestions, type Starter } from "../incident";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import type { AgentEvent, AgentStep, AppConfig, ChatEntry, Incident } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Button, cx, Empty, Panel, ServiceMark, Tag } from "../ui/primitives";
import {
  CheckIcon,
  ChevronDown,
  ChevronRight,
  MetricsIcon,
  RefreshIcon,
  SearchIcon,
  SendIcon,
  ShieldIcon,
  SparkleIcon,
} from "../ui/icons";

export function Assistant({
  config,
  conversationId,
  incidents,
  model,
  catalog,
  onOpenSettings,
  onNavigate,
}: {
  config: AppConfig | null;
  conversationId: string;
  incidents: Incident[];
  model: EstateModel;
  catalog: Catalog;
  onOpenSettings: () => void;
  onNavigate: (view: View) => void;
}) {
  const [entries, setEntries] = useState<ChatEntry[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [live, setLive] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  /**
   * Starters that know what is happening. Asking "what is failing right now?"
   * of a model, on a screen that already knows, spends a tool call to be told
   * something the estate could have said for free.
   */
  const SUGGESTIONS = useMemo(() => suggestions(incidents), [incidents]);

  const configured = Boolean(config?.llm.hasApiKey || config?.llm.provider === "ollama");
  const readOnly = config?.llm.agentMode === "read_only";

  // Live progress while a turn is in flight — otherwise a multi-tool
  // investigation looks like the app has hung.
  useEffect(() => {
    const subscription = listen<AgentEvent>("agent://step", (event) => {
      const payload = event.payload;
      if (payload.conversationId !== conversationId) return;
      if (payload.type === "thinking") setLive(payload.text);
      else if (payload.type === "toolStart") setLive(`Running ${payload.tool}…`);
      else if (payload.type === "toolEnd") setLive(`${payload.tool} — ${payload.summary}`);
      else setLive(null);
    });
    return () => {
      subscription.then((unlisten) => unlisten());
    };
  }, [conversationId]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [entries, live]);

  const send = async (message: string) => {
    const text = message.trim();
    if (!text || busy) return;

    const id = `${Date.now()}`;
    setEntries((current) => [
      ...current,
      { id: `u-${id}`, role: "user", text },
      { id: `a-${id}`, role: "assistant", text: "", pending: true },
    ]);
    setDraft("");
    setBusy(true);

    try {
      const turn = await api.sendMessage(conversationId, text);
      setEntries((current) =>
        current.map((entry) =>
          entry.id === `a-${id}`
            ? {
                ...entry,
                text: turn.text,
                steps: turn.steps,
                usage: turn.usage,
                pending: false,
              }
            : entry,
        ),
      );
    } catch (error) {
      setEntries((current) =>
        current.map((entry) =>
          entry.id === `a-${id}`
            ? { ...entry, text: errorMessage(error), error: true, pending: false }
            : entry,
        ),
      );
    } finally {
      setBusy(false);
      setLive(null);
    }
  };

  const clear = async () => {
    await api.clearConversation(conversationId).catch(() => undefined);
    setEntries([]);
  };

  return (
    <>
      <ScreenHeader
        title="Assistant"
        meta={
          config && (
            <>
              {config.llm.model || config.llm.provider}
              <span className="text-ink-4/50">·</span>
              <span className="flex items-center gap-1">
                <ShieldIcon size={11} />
                {readOnly ? "Read-only" : "Actions require approval"}
              </span>
            </>
          )
        }
      >
        {entries.length > 0 && <Button onClick={clear}>Clear conversation</Button>}
      </ScreenHeader>

      <Screen className="flex flex-col">
        {!configured ? (
          <Panel>
            <Empty
              title="No model configured"
              hint="Choose a provider and enter a key in Settings. Ollama runs locally and needs no key."
            />
            <div className="flex justify-center pb-4">
              <Button variant="primary" onClick={onOpenSettings}>
                Open settings
              </Button>
            </div>
          </Panel>
        ) : (
          <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col">
            <div className="min-h-0 flex-1">
              {entries.length === 0 ? (
                <Welcome
                  config={config}
                  readOnly={readOnly}
                  starters={SUGGESTIONS}
                  incidents={incidents}
                  model={model}
                  catalog={catalog}
                  onAsk={send}
                />
              ) : (
                <div className="flex flex-col gap-4 py-1">
                  {entries.map((entry) => (
                    <Message
                      key={entry.id}
                      entry={entry}
                      modelName={config?.llm.model || config?.llm.provider}
                      services={model.services.map((s) => s.snapshot)}
                      catalog={catalog}
                      onOpen={(profileId) => onNavigate({ name: "service", profileId })}
                    />
                  ))}
                  {busy && live && (
                    <p className="num flex items-center gap-2 text-xs text-ink-4">
                      <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
                      {live}
                    </p>
                  )}
                  <div ref={endRef} />
                </div>
              )}
            </div>

            <form
              className="sticky bottom-0 mt-3 flex items-end gap-2 rounded-md border border-line bg-panel p-2"
              onSubmit={(e) => {
                e.preventDefault();
                send(draft);
              }}
            >
              <textarea
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    send(draft);
                  }
                }}
                rows={1}
                placeholder="Ask about your infrastructure…"
                className="max-h-40 min-h-7 flex-1 resize-none bg-transparent px-1 py-1 text-base text-ink placeholder:text-ink-4 focus:outline-none"
              />
              <Button variant="primary" type="submit" disabled={busy || !draft.trim()}>
                <SendIcon size={13} />
                Send
              </Button>
            </form>
            <p className="mt-1.5 mb-1 text-center text-2xs text-ink-4">
              Enter to send · Shift + Enter for a new line
            </p>
          </div>
        )}
      </Screen>
    </>
  );
}

interface Named {
  profileId: string;
  profileName: string;
  kind: string;
}

function Message({
  entry,
  modelName,
  services,
  catalog,
  onOpen,
}: {
  entry: ChatEntry;
  modelName?: string;
  services: Named[];
  catalog: Catalog;
  onOpen: (profileId: string) => void;
}) {
  if (entry.role === "user") {
    return (
      <div className="flex justify-end">
        <p className="max-w-[80%] rounded-md rounded-br-sm bg-raised px-3 py-1.5 text-base text-ink">
          {entry.text}
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2 text-2xs text-ink-4">
        <span className="grid h-5 w-5 place-items-center rounded-md border border-line-strong bg-raised text-ink">
          <SparkleIcon size={11} />
        </span>
        <span className="font-medium text-ink-2">root</span>
        {modelName && <span className="num">· {modelName}</span>}
      </div>
      {entry.steps && entry.steps.length > 0 && <Steps steps={entry.steps} />}
      {entry.pending ? (
        <p className="flex items-center gap-2 text-xs text-ink-4">
          <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
          Investigating…
        </p>
      ) : (
        <p
          className={cx(
            "text-base leading-relaxed whitespace-pre-wrap",
            entry.error ? "text-crit" : "text-ink-2",
          )}
        >
          {entry.error ? entry.text : linked(entry.text, services, catalog, onOpen)}
        </p>
      )}
      {entry.usage && (
        <p className="num text-2xs text-ink-4">
          {entry.usage.inputTokens} in · {entry.usage.outputTokens} out
        </p>
      )}
    </div>
  );
}

/**
 * The answer with every connection it names turned into that connection:
 * its mark, and a click to open it. The model writes "orders-api"; the
 * screen knows what orders-api is, and should not make the reader go and
 * find it.
 */
function linked(text: string, services: Named[], catalog: Catalog, onOpen: (id: string) => void) {
  const names = services
    .map((s) => s.profileName)
    .filter(Boolean)
    .sort((a, b) => b.length - a.length)
    .map((n) => n.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  if (names.length === 0) return text;
  // Not inside a longer name: "orders-api" is not a link inside "orders-api-release".
  const pattern = new RegExp(`(?<![\\w-])(${names.join("|")})(?![\\w-])`, "g");
  const byName = new Map(services.map((s) => [s.profileName, s]));
  const out: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(pattern)) {
    const service = byName.get(match[0]);
    if (!service || match.index === undefined) continue;
    if (match.index > last) out.push(text.slice(last, match.index));
    out.push(
      <button
        key={`${match.index}-${service.profileId}`}
        type="button"
        onClick={() => onOpen(service.profileId)}
        title={`Open ${service.profileName}`}
        className="inline-flex items-center gap-1 rounded-sm bg-wash px-[3px] align-[-2px] text-[0.95em] text-ink transition-colors hover:bg-wash-strong"
      >
        <ServiceMark descriptor={catalog.get(service.kind as never)} size={13} />
        {service.profileName}
      </button>,
    );
    last = match.index + match[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

/**
 * The tool trail: which tools it ran, in order, each with whether it worked.
 *
 * Visible rather than folded into "4 tool calls" — what the answer was read
 * from is the reason to trust it, and a count says nothing about that. The
 * results themselves stay behind a click: they are evidence, not the answer.
 */
function Steps({ steps }: { steps: AgentStep[] }) {
  const [open, setOpen] = useState(false);
  const failed = steps.filter((s) => !s.ok).length;

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="label mr-0.5">Looked at</span>
        {steps.map((step, i) => (
          <span
            key={`${step.tool}-${i}`}
            className="num inline-flex h-5 items-center gap-1 rounded-sm border border-line bg-panel px-1.5 text-2xs text-ink-3"
            title={step.result}
          >
            <span
              className="h-1.5 w-1.5 shrink-0 rounded-full"
              style={{ background: step.ok ? "var(--color-ok)" : "var(--color-crit)" }}
              aria-hidden
            />
            {step.tool}
          </span>
        ))}
        {failed > 0 && <Tag color="var(--color-crit)">{failed} failed</Tag>}
        {steps.some((s) => s.mutating) && <Tag color="var(--color-warn)">mutating</Tag>}
        <button
          onClick={() => setOpen((v) => !v)}
          className="flex items-center gap-0.5 text-2xs text-ink-4 transition-colors hover:text-ink-2"
        >
          {open ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
          {open ? "Hide results" : "Results"}
        </button>
      </div>

      {open && (
        <ul className="divide-y divide-line rounded-md border border-line bg-panel">
          {steps.map((step, i) => (
            <li key={`${step.tool}-${i}`} className="px-2.5 py-1.5">
              <div className="flex items-center gap-1.5 text-2xs">
                <span
                  className="h-1.5 w-1.5 shrink-0 rounded-full"
                  style={{ background: step.ok ? "var(--color-ok)" : "var(--color-crit)" }}
                  aria-hidden
                />
                <span className="num text-ink-2">{step.tool}</span>
                {step.mutating && (
                  <Tag color={step.approved ? "var(--color-warn)" : "var(--color-ink-3)"}>
                    {step.approved === null ? "not run" : step.approved ? "approved" : "declined"}
                  </Tag>
                )}
                <span className="num ml-auto text-ink-4">{formatDuration(step.durationMs)}</span>
              </div>
              <p className="num mt-0.5 line-clamp-3 text-2xs text-ink-4" title={step.result}>
                {step.result}
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ welcome

const tint = (color: string, pct: number) => `color-mix(in srgb, ${color} ${pct}%, transparent)`;

/**
 * Each kind of question in the colour and shape it already has in root: a
 * change is the accent diamond the charts draw, a failure is its severity,
 * anything read off a metric is data ink. The tile says what the question
 * is about before its words are read.
 */
function StarterMark({ kind, severity }: { kind: Starter["kind"]; severity: string }) {
  const color = {
    why: severity,
    change: "var(--color-accent)",
    dependency: "var(--color-data)",
    evidence: "var(--color-warn)",
    capacity: "var(--color-warn)",
    flap: "var(--color-serious)",
    trend: "var(--color-data)",
  }[kind];
  const glyph = {
    why: <SearchIcon size={13} />,
    change: <span className="block h-[8px] w-[8px] rotate-45 rounded-[1px]" style={{ background: color }} />,
    dependency: (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden>
        <circle cx="6" cy="6" r="2.5" />
        <circle cx="18" cy="18" r="2.5" />
        <path d="M8 8l8 8" />
      </svg>
    ),
    evidence: <CheckIcon size={13} />,
    capacity: <MetricsIcon size={13} />,
    flap: <RefreshIcon size={13} />,
    trend: <MetricsIcon size={13} />,
  }[kind];
  return (
    <span
      className="grid h-8 w-8 shrink-0 place-items-center rounded-md border"
      style={{ color, background: tint(color, 12), borderColor: tint(color, 30) }}
    >
      {glyph}
    </span>
  );
}

/**
 * What a first look at the assistant should say: what it can see, what is
 * happening, and the questions worth asking about it — each a card that says
 * what answering it will read.
 *
 * It replaces a line of grey text and four grey buttons on an empty page. The
 * app is named for something that sees everything; this is where it says so.
 */
function Welcome({
  config,
  readOnly,
  starters,
  incidents,
  model,
  catalog,
  onAsk,
}: {
  config: AppConfig | null;
  readOnly: boolean;
  starters: Starter[];
  incidents: Incident[];
  model: EstateModel;
  catalog: Catalog;
  onAsk: (question: string) => void;
}) {
  const lead = incidents.find((i) => !i.acknowledged);
  const severity =
    lead?.severity === "critical"
      ? "var(--color-crit)"
      : lead
        ? "var(--color-warn)"
        : "var(--color-ink-3)";
  const metrics = model.services.reduce((n, s) => n + s.snapshot.metrics.length, 0);
  const kindOf = new Map(model.services.map((s) => [s.snapshot.profileId, s.snapshot.kind]));

  const sees: [number, string][] = [
    [model.counts.connections, model.counts.connections === 1 ? "connection" : "connections"],
    [model.counts.entities, "components"],
    [metrics, "live metrics"],
    [model.counts.alerts, model.counts.alerts === 1 ? "alert" : "alerts"],
  ];

  return (
    <div className="flex min-h-full flex-col items-center justify-center gap-6 py-6">
      <div className="flex flex-col items-center gap-3 text-center">
        <span
          className="grid h-12 w-12 place-items-center rounded-xl border border-line-strong bg-raised text-ink shadow-[0_0_40px_-6px_color-mix(in_oklab,var(--color-ink)_25%,transparent)]"
          aria-hidden
        >
          <SparkleIcon size={20} />
        </span>
        <div>
          <h2 className="text-2xl font-semibold tracking-[-.02em] text-ink">Ask root</h2>
          <p className="mt-1 max-w-md text-xs leading-relaxed text-ink-3">
            It reads what the collectors see — snapshots, metrics, alerts, logs and changes —
            through a fixed set of tools, and cites what it found.
          </p>
        </div>
        {config && (
          <div className="flex items-center gap-1.5">
            <Tag>{config.llm.model || config.llm.provider}</Tag>
            <Tag color={readOnly ? "var(--color-ok)" : "var(--color-warn)"}>
              <span className="flex items-center gap-1">
                <ShieldIcon size={10} />
                {readOnly ? "Read-only" : "Actions need your approval"}
              </span>
            </Tag>
          </div>
        )}
      </div>

      {/* What it can see: the same estate the other screens draw, counted. */}
      <div className="flex w-full flex-wrap items-baseline justify-center gap-x-6 gap-y-2 border-y border-line-soft py-3">
        <span className="label">What root can see</span>
        {sees.map(([n, label]) => (
          <span key={label} className="flex items-baseline gap-1.5">
            <span className="num text-lg leading-none font-semibold text-ink">{n}</span>
            <span className="text-2xs text-ink-4">{label}</span>
          </span>
        ))}
      </div>

      <div className="flex w-full flex-col gap-2.5">
        {lead ? (
          <div
            className="flex items-start gap-3 rounded-lg border border-l-2 border-line bg-panel px-3 py-2.5"
            style={{ borderLeftColor: severity }}
          >
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="label" style={{ color: severity }}>
                  Right now
                </span>
                <span className="text-2xs text-ink-4">
                  started {formatRelative(lead.started)} · {lead.alertIds.length} alert
                  {lead.alertIds.length === 1 ? "" : "s"} · {lead.confidence} confidence
                </span>
              </div>
              <p className="mt-1 truncate text-sm font-medium text-ink" title={lead.title}>
                {lead.title}
              </p>
            </div>
            <div className="flex shrink-0 items-center -space-x-1 pt-1">
              {lead.profileIds.slice(0, 5).map((id) => {
                const kind = kindOf.get(id);
                return kind ? (
                  <span key={id} title={id} className="rounded-sm ring-2 ring-panel">
                    <ServiceMark descriptor={catalog.get(kind)} size={18} />
                  </span>
                ) : null;
              })}
            </div>
          </div>
        ) : (
          <span className="label">Worth asking</span>
        )}

        <div className="grid gap-2 @2xl:grid-cols-2">
          {starters.map((starter) => (
            <button
              key={starter.text}
              type="button"
              onClick={() => onAsk(starter.text)}
              className="group flex items-start gap-3 rounded-lg border border-line bg-panel p-3 text-left transition-colors hover:border-line-strong hover:bg-raised"
            >
              <StarterMark kind={starter.kind} severity={severity} />
              <span className="min-w-0 flex-1">
                <span className="block text-xs leading-snug font-medium text-ink-2 group-hover:text-ink">
                  {starter.text}
                </span>
                <span className="mt-1 block text-2xs text-ink-4">reads {starter.reads}</span>
              </span>
              <ChevronRight
                size={12}
                className="mt-0.5 text-ink-4 opacity-0 transition-opacity group-hover:opacity-100"
              />
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
