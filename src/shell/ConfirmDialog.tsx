import { useState } from "react";

import { api, errorMessage } from "../api";
import { formatRelative } from "../format";
import type { PendingConfirmation } from "../types";
import { Button } from "../ui/primitives";
import { ShieldIcon } from "../ui/icons";

/**
 * The approval gate for an assistant-initiated mutating call.
 *
 * One request at a time, oldest first — showing them batched would invite
 * rubber-stamping, which is the entire thing this dialog exists to prevent.
 * Declining is fed back to the model as a result, so it adapts rather than
 * retrying.
 */
export function ConfirmDialog({
  request,
  onResolved,
}: {
  request: PendingConfirmation;
  onResolved: (callId: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const resolve = async (approved: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await api.resolveConfirmation(request.callId, approved);
      onResolved(request.callId);
    } catch (e) {
      setError(errorMessage(e));
      setBusy(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/70 p-4 backdrop-blur-sm"
      role="dialog"
      aria-modal
    >
      <div className="panel w-full max-w-lg bg-overlay overlay-plate">
        <header className="flex h-10 items-center gap-2 border-b border-line px-4">
          <ShieldIcon size={15} />
          <h2 className="text-sm font-semibold text-ink">Approval required</h2>
          <span className="num ml-auto text-2xs text-ink-4">
            {formatRelative(request.requestedAt)}
          </span>
        </header>

        <div className="p-4">
          <p className="text-base text-ink-2">{request.description}</p>

          <div className="mt-3">
            <span className="label">Equivalent command</span>
            <pre className="mt-1 overflow-x-auto rounded-md border border-line bg-page p-2.5 text-2xs text-ink-2">
              {request.equivalentCommand}
            </pre>
          </div>

          <dl className="mt-3 rounded-md border border-line bg-page p-2.5 text-2xs">
            <div className="flex gap-2">
              <dt className="w-20 shrink-0 text-ink-4">Target</dt>
              <dd className="font-medium text-ink">{request.profileName}</dd>
            </div>
            <div className="mt-1 flex gap-2">
              <dt className="w-20 shrink-0 text-ink-4">Tool</dt>
              <dd className="num text-ink-2">{request.tool}</dd>
            </div>
            {Object.entries(request.arguments).map(([key, value]) => (
              <div key={key} className="mt-1 flex gap-2">
                <dt className="w-20 shrink-0 text-ink-4">{key}</dt>
                <dd className="num min-w-0 break-all text-ink-2">{String(value)}</dd>
              </div>
            ))}
          </dl>

          <p className="mt-3 text-2xs leading-snug text-ink-4">
            This changes the state of running infrastructure. There is no “always allow” — every
            call is approved individually.
          </p>

          {error && <p className="mt-2 text-xs text-crit">{error}</p>}
        </div>

        <footer className="flex justify-end gap-2 border-t border-line px-4 py-2.5">
          <Button onClick={() => resolve(false)} disabled={busy}>
            Decline
          </Button>
          <Button variant="danger" onClick={() => resolve(true)} disabled={busy}>
            {busy ? "Running…" : "Approve and run"}
          </Button>
        </footer>
      </div>
    </div>
  );
}
