import { useCallback, useEffect, useRef, useState } from "react";

import { api, errorMessage } from "../api";
import { conversationFor, promptFor, targetSubject, type ExplainTarget } from "../explain";
import type { AgentStep } from "../types";
import { AssistantIcon, ChevronRight, CloseIcon, ExternalIcon } from "../ui/icons";
import { Button, cx, Empty, Input, Kbd } from "../ui/primitives";

/**
 * The assistant, docked beside whatever you were already looking at.
 *
 * The whole point is not losing your place: an operator who cannot read an
 * error should not have to leave the screen the error is on, restate it in a
 * chat box, and then navigate back. So the target arrives with the panel, the
 * question is built from it, and the answer lands next to the original line.
 */
export function AssistantDock({
  target,
  configured,
  onClose,
  onExpand,
}: {
  target: ExplainTarget;
  configured: boolean;
  onClose: () => void;
  onExpand: () => void;
}) {
  const [answer, setAnswer] = useState<string | null>(null);
  const [steps, setSteps] = useState<AgentStep[]>([]);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const bodyRef = useRef<HTMLDivElement>(null);

  const conversation = conversationFor(target);
  const subject = targetSubject(target);

  const send = useCallback(
    async (message: string) => {
      if (!message.trim()) return;
      setBusy(true);
      setFailure(null);
      try {
        const turn = await api.sendMessage(conversation, message);
        setAnswer(turn.text);
        setSteps(turn.steps);
      } catch (error) {
        // A dock that silently shows nothing is worse than one that says why.
        setFailure(errorMessage(error));
      } finally {
        setBusy(false);
      }
    },
    [conversation],
  );

  // Asking is the point of opening it, so opening asks — except for a bare
  // "Ask", which has no subject yet and waits for one.
  useEffect(() => {
    setAnswer(null);
    setSteps([]);
    setFailure(null);
    if (target.kind !== "ask" && configured) void send(promptFor(target));
  }, [conversation, configured, send, target]);

  useEffect(() => {
    bodyRef.current?.scrollTo({ top: 0 });
  }, [conversation]);

  return (
    <aside className="flex w-[392px] shrink-0 flex-col border-l border-line bg-panel">
      <header className="flex h-[52px] shrink-0 items-center gap-2.5 border-b border-line px-4">
        <span className="flex text-accent">
          <AssistantIcon size={15} />
        </span>
        <span className="label">Assistant</span>
        <span className="num rounded-sm border border-ok/30 bg-ok/10 px-1.5 text-2xs text-ok">
          read-only
        </span>
        <div className="ml-auto flex items-center gap-1">
          <Button variant="ghost" className="px-1.5" onClick={onExpand} title="Open full conversation">
            <ExternalIcon size={13} />
          </Button>
          <Button variant="ghost" className="px-1.5" onClick={onClose} title="Close  ⌘J">
            <CloseIcon size={14} />
          </Button>
        </div>
      </header>

      {subject && (
        // What was clicked, restated — so the panel is visibly about this line
        // and not about the last thing asked.
        <div className="shrink-0 border-b border-line bg-sunk px-4 py-2.5">
          <div className="label mb-1.5">Explaining</div>
          <div className="num text-2xs leading-relaxed break-words text-ink-2">
            <span className="text-ink-4">{subject.meta}</span>
            <br />
            {subject.body}
          </div>
        </div>
      )}

      <div ref={bodyRef} className="min-h-0 flex-1 overflow-y-auto px-4 py-3.5">
        {!configured ? (
          <Empty
            title="No model configured"
            hint="Add a provider in Settings to have the assistant explain what it sees."
          />
        ) : failure ? (
          <div className="rounded-md border border-crit/40 bg-crit/5 p-3">
            <div className="label mb-1.5 text-crit">Could not answer</div>
            <p className="num text-2xs leading-relaxed text-ink-2">{failure}</p>
            {target.kind !== "ask" && (
              <Button className="mt-3" onClick={() => void send(promptFor(target))}>
                Try again
              </Button>
            )}
          </div>
        ) : busy && !answer ? (
          <Investigating />
        ) : answer ? (
          <>
            {steps.length > 0 && <Steps steps={steps} />}
            <div className="text-[12.5px] leading-relaxed whitespace-pre-wrap text-ink-2">
              {answer}
            </div>
          </>
        ) : (
          <Empty
            title="Ask about this estate"
            hint="Or click Explain on any log line or alert to start from it."
          />
        )}
      </div>

      <div className="shrink-0 border-t border-line px-4 py-3">
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const message = draft;
            setDraft("");
            void send(message);
          }}
          className="flex items-center gap-2"
        >
          <Input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder={target.kind === "ask" ? "Ask about your infrastructure…" : "Ask a follow-up…"}
            className="h-8 flex-1"
            disabled={!configured}
          />
          <Kbd>⌘J</Kbd>
        </form>
        <p className="num mt-2 text-[9.5px] leading-relaxed text-ink-4">
          Grounded in this workspace&rsquo;s snapshots, metrics and logs. It cannot change anything.
        </p>
      </div>
    </aside>
  );
}

/** The investigation is shown, not summarised away. */
function Steps({ steps }: { steps: AgentStep[] }) {
  const [open, setOpen] = useState(false);
  const failed = steps.filter((s) => !s.ok).length;

  return (
    <div className="panel mb-3.5 overflow-hidden">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2.5 px-3 py-2 text-left"
      >
        <span className="label">Investigation</span>
        <span className="num text-2xs text-ink-4">
          {steps.length} tool {steps.length === 1 ? "call" : "calls"}
          {failed > 0 && <span className="text-crit"> · {failed} failed</span>}
        </span>
        <ChevronRight
          size={11}
          className={cx("ml-auto text-ink-4 transition-transform", open && "rotate-90")}
        />
      </button>
      {open &&
        steps.map((step, i) => (
          <div
            key={i}
            className="flex items-start gap-2.5 border-t border-line-soft px-3 py-1.5"
          >
            <span
              className={cx("num shrink-0 text-2xs", step.ok ? "text-accent" : "text-crit")}
              style={{ width: 118 }}
            >
              {step.tool}
            </span>
            <span className="num min-w-0 flex-1 truncate text-2xs text-ink-4" title={step.result}>
              {step.result}
            </span>
          </div>
        ))}
    </div>
  );
}

function Investigating() {
  return (
    <div className="flex flex-col gap-2">
      <div className="label">Investigating</div>
      <div className="flex flex-col gap-1.5">
        {[0, 1, 2].map((i) => (
          <div
            key={i}
            className="h-3 animate-pulse rounded-sm bg-sunk"
            style={{ width: `${88 - i * 17}%`, animationDelay: `${i * 140}ms` }}
          />
        ))}
      </div>
    </div>
  );
}
