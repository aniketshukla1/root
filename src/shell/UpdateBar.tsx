import { useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import { serverUrl } from "../remote";
import type { UpdateInfo } from "../types";
import { Button } from "../ui/primitives";

/**
 * One quiet line when a newer signed release is out. Checked once a launch.
 * A failed check — offline, no feed, a build without a release key, a browser
 * talking to a server — shows nothing: none of those is the reader's to fix.
 */
export function UpdateBar() {
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    // A browser talking to a server has no app here to update.
    if (serverUrl()) return;
    api.checkForUpdate().then(setUpdate, () => {});
  }, []);

  if (!update) return null;

  const install = async () => {
    setBusy(true);
    setError(null);
    try {
      // Restarts into the new version; it returns only when it did not.
      await api.installUpdate();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      role="status"
      className="flex min-h-[34px] shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-line px-4 py-1.5 text-xs"
    >
      <span className="font-medium text-ink">root {update.version} is ready</span>
      <span className="num text-ink-4">you have {update.currentVersion}</span>
      {update.notes && (
        <span className="min-w-0 flex-1 truncate text-ink-3" title={update.notes}>
          {update.notes}
        </span>
      )}
      {error && (
        <span role="alert" className="truncate text-crit" title={error}>
          {error}
        </span>
      )}
      <span className="ml-auto flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="sm" onClick={() => setUpdate(null)}>
          Later
        </Button>
        <Button variant="primary" size="sm" loading={busy} onClick={install}>
          Restart to update
        </Button>
      </span>
    </div>
  );
}
