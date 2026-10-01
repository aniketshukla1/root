import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "../events";

import { api, errorMessage } from "../api";
import { formatDuration } from "../format";
import { suggestions } from "../incident";
import type { AgentEvent, AgentStep, AppConfig, ChatEntry, Incident } from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { Button, cx, Empty, Panel, Tag } from "../ui/primitives";
import { ChevronDown, ChevronRight, SendIcon, ShieldIcon } from "../ui/icons";

export function Assistant({
  config,
  conversationId,
  incidents,
  onOpenSettings,
}: {
  config: AppConfig | null;
  conversationId: string;
  incidents: Incident[];
  onOpenSettings: () => void;
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
                <div className="flex h-full flex-col items-center justify-center gap-4">
                  <div className="text-center">
                    <p className="text-base text-ink-2">
                      Ask about anything the collectors can see.
                    </p>
                    <p className="mt-1 text-xs text-ink-4">
                      The assistant reads snapshots, metrics, alerts, and logs through a fixed set
                      of tools, and cites what it found.
                    </p>
                  </div>
                  <div className="flex flex-col gap-1.5">
                    {SUGGESTIONS.map((suggestion) => (
                      <button
                        key={suggestion}
                        onClick={() => send(suggestion)}
                        className="rounded-md border border-line bg-panel px-3 py-1.5 text-left text-xs text-ink-2 transition-colors hover:border-line-strong hover:text-ink"
                      >
                        {suggestion}
                      </button>
                    ))}
                  </div>
                </div>
              ) : (
                <div className="flex flex-col gap-4 py-1">
                  {entries.map((entry) => (
                    <Message key={entry.id} entry={entry} />
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
          </div>
        )}
      </Screen>
    </>
  );
}

function Message({ entry }: { entry: ChatEntry }) {
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
          {entry.text}
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

/** The tool trail. Collapsed by default — it is evidence, not the answer. */
function Steps({ steps }: { steps: AgentStep[] }) {
  const [open, setOpen] = useState(false);
  const failed = steps.filter((s) => !s.ok).length;

  return (
    <div className="rounded-md border border-line bg-panel">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-2.5 py-1.5 text-2xs text-ink-4 transition-colors hover:text-ink-2"
      >
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        <span className="num">{steps.length}</span>
        <span>tool call{steps.length === 1 ? "" : "s"}</span>
        {failed > 0 && <Tag color="var(--color-crit)">{failed} failed</Tag>}
        {steps.some((s) => s.mutating) && <Tag color="var(--color-warn)">mutating</Tag>}
      </button>

      {open && (
        <ul className="divide-y divide-line border-t border-line">
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
