import { useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import { formatDateTime } from "../format";
import type { AuditEntry } from "../types";
import { Empty, Panel, Tag } from "../ui/primitives";

const OUTCOME: Record<AuditEntry["outcome"], { label: string; color: string }> = {
  done: { label: "Done", color: "var(--color-ok)" },
  refused: { label: "Refused", color: "var(--color-warn)" },
  failed: { label: "Failed", color: "var(--color-crit)" },
};

/**
 * Who changed what on this server. The server keeps names only — the command,
 * the id it named, who asked — because arguments can carry secrets, and only
 * an administrator may read it; the server enforces that, not this screen.
 */
export function AuditLog() {
  const [entries, setEntries] = useState<AuditEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.getAuditLog(500).then(setEntries, (e) => setError(errorMessage(e)));
  }, []);

  if (error) return <Empty title="The audit log could not be read" hint={error} />;
  if (!entries) return null;

  return (
    <>
      <p className="mb-3 max-w-2xl text-xs text-ink-3">
        Every change made through this server, and every attempt it refused. Names only: arguments can
        carry secrets, so none are kept.
      </p>
      <Panel flush>
        {entries.length === 0 ? (
          <Empty title="Nothing has changed yet" hint="A saved rule, a restart, a silence: each is entered here as it happens." />
        ) : (
          <ol className="divide-y divide-line">
            {entries.map((e, i) => (
              <li key={`${e.at}-${i}`} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-xs">
                <time className="num w-32 shrink-0 text-ink-4" dateTime={e.at}>
                  {formatDateTime(e.at)}
                </time>
                <span className="min-w-0 text-ink-2" title={e.email}>
                  {e.actor} <span className="text-ink-4">· {e.role}</span>
                </span>
                <code className="text-ink">{e.command}</code>
                {e.target && <span className="num min-w-0 truncate text-ink-3">{e.target}</span>}
                <Tag color={OUTCOME[e.outcome].color} className="ml-auto">
                  {OUTCOME[e.outcome].label}
                </Tag>
                {e.detail && <p className="basis-full text-ink-4">{e.detail}</p>}
              </li>
            ))}
          </ol>
        )}
      </Panel>
    </>
  );
}
