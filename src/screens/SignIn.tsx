import { useState } from "react";

import { api, errorMessage } from "../api";
import type { EstateModel } from "../model";
import type { TeamState } from "../types";
import { LaneStrip } from "../ui/plots";
import { Button, Input, RootMark } from "../ui/primitives";

/**
 * Signing in to a workspace on a server you run.
 *
 * This screen only exists once someone has pointed root at a server. In local
 * mode — the default, and what the packaged app does out of the box — there is
 * nothing to sign in to and this never appears. That is deliberate: a
 * monitoring tool that demands an account before it will look at your own
 * Docker socket has taken something away rather than added anything.
 *
 * The left half shows the estate this machine already reaches. It is not
 * decoration: it is the honest answer to "what am I signing in to", and on a
 * fresh install it is empty and says so rather than showing invented services.
 */
export function SignIn({
  serverUrl,
  model,
  minutes,
  healthRuns,
  onSignedIn,
  onUseLocal,
}: {
  serverUrl: string;
  model: EstateModel;
  minutes: number;
  healthRuns: Record<string, { health: string; from: string; to: string }[]>;
  onSignedIn: (team: TeamState) => void;
  onUseLocal: () => void;
}) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!email.trim() || !password) return;
    setBusy(true);
    setError(null);
    try {
      onSignedIn(await api.signIn(email.trim(), password));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      // The password is dropped either way — it was only ever needed to get a
      // token, and keeping it in a field after a failure invites a retry that
      // sends the same wrong thing again.
      setPassword("");
      setBusy(false);
    }
  };

  const windowMs = minutes * 60_000;
  const start = Date.now() - windowMs;
  const lanes = model.services.slice(0, 4).map((service) => ({
    name: service.snapshot.profileName,
    runs: (healthRuns[service.snapshot.profileId] ?? [])
      .map((run) => ({
        health: run.health as never,
        from: Math.max(0, Math.min(1, (Date.parse(run.from) - start) / windowMs)),
        to: Math.max(0, Math.min(1, (Date.parse(run.to) - start) / windowMs)),
      }))
      .filter((run) => run.to > run.from),
  }));

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-page">
      <div className="relative hidden w-[620px] shrink-0 flex-col justify-between gap-10 overflow-y-auto border-r border-line p-12 lg:flex">
        <div
          className="pointer-events-none absolute inset-x-0 top-0 h-[420px]"
          style={{
            background:
              "radial-gradient(120% 90% at 20% -10%, color-mix(in srgb, var(--color-accent) 10%, transparent), transparent 66%)",
          }}
          aria-hidden
        />

        <div className="relative z-10 flex items-center gap-3">
          <span className="grid h-8 w-8 place-items-center rounded-md bg-accent text-on-accent">
            <RootMark size={20} />
          </span>
          <span className="font-mono text-xs tracking-[.18em] text-ink uppercase">root</span>
        </div>

        <div className="relative z-10">
          <h1 className="max-w-[15ch] text-6xl leading-[1.04] font-medium tracking-[-.03em] text-ink">
            One window on the whole estate.
          </h1>
          <p className="mt-5 max-w-[46ch] text-lg leading-relaxed text-ink-3">
            Live health, metric history, a unified log stream, and an assistant that investigates
            rather than guesses — across every service you run.
          </p>

          <div className="mt-8 flex max-w-[420px] flex-col gap-1.5">
            {lanes.length === 0 ? (
              <p className="num text-2xs leading-relaxed text-ink-4">
                Nothing is connected on this machine yet. Signing in joins a workspace; it does not
                add connections, and root can watch an estate without either.
              </p>
            ) : (
              lanes.map((lane) => (
                <div key={lane.name} className="flex items-center gap-3">
                  <span className="num w-[100px] shrink-0 truncate text-2xs text-ink-4">
                    {lane.name}
                  </span>
                  <span className="min-w-0 flex-1">
                    <LaneStrip runs={lane.runs} height={11} />
                  </span>
                </div>
              ))
            )}
          </div>
        </div>

        {/* Whatever this server does about encryption and audit is that
            server's business, and root does not know. Claiming a compliance
            posture on its behalf would be the one lie this screen could tell
            that actually costs someone something. */}
        <p className="relative z-10 num text-2xs text-ink-4">
          Credentials go to this machine's keychain. Everything else is between you and the server
          you run.
        </p>
      </div>

      {/* Centred with an auto margin rather than `items-center`, because a
          centred flex item that overflows its scroll container pushes past the
          top edge and cannot be scrolled back to. On a short window — a
          laptop, or a Tauri window dragged down — that put the heading
          somewhere unreachable. Auto margins centre and still yield. */}
      <div className="flex min-w-0 flex-1 justify-center overflow-y-auto p-12">
        <form onSubmit={submit} className="my-auto w-[380px]">
          <h2 className="text-4xl font-medium tracking-[-.02em] text-ink">Sign in</h2>
          <p className="mt-2 text-base text-ink-3">
            to <span className="num text-ink-2">{serverUrl}</span>
          </p>

          <div className="mt-7 flex flex-col gap-3">
            <label className="block">
              <span className="label mb-1.5 block">Email</span>
              <Input
                type="email"
                autoComplete="username"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="you@example.com"
                className="h-[38px]"
                autoFocus
              />
            </label>

            <label className="block">
              <span className="label mb-1.5 block">Password</span>
              <Input
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="h-[38px]"
              />
            </label>

            {error && (
              <p
                className="rounded-md border px-3 py-2 text-2xs leading-relaxed"
                style={{
                  color: "var(--color-crit)",
                  borderColor: "color-mix(in srgb, var(--color-crit) 35%, transparent)",
                  background: "color-mix(in srgb, var(--color-crit) 9%, transparent)",
                }}
                role="alert"
              >
                {error}
              </p>
            )}

            <Button
              type="submit"
              variant="primary"
              className="mt-1 h-[38px]"
              disabled={busy || !email.trim() || !password}
            >
              {busy ? "Signing in…" : "Sign in"}
            </Button>
          </div>

          {/* The way out. root does not need this server to be useful, and a
              sign-in screen with no exit would be claiming otherwise. */}
          <div className="mt-7 border-t border-line pt-5">
            <p className="text-xs leading-relaxed text-ink-4">
              root works without a workspace. Local mode reads the config on this machine and keeps
              every credential in its keychain — nothing leaves.
            </p>
            <Button className="mt-2.5" onClick={onUseLocal}>
              Use root locally instead
            </Button>
          </div>
        </form>
      </div>
    </div>
  );
}
