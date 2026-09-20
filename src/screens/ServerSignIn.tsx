// The door, when root is a server and this is a browser.
//
// Two states, and which one shows is the server's answer rather than a guess:
//
//   - **Claim.** A fresh server has no accounts, and the first account made on
//     it is an administrator. It is offered here because the alternative — a
//     password printed into the boot log — is a credential sitting in whatever
//     collects logs.
//   - **Sign in.** Every time after that.
//
// The desktop app never sees this. It talks to an engine in its own process,
// where there is nobody to authenticate to, and a monitoring tool that
// demanded an account before it would read your own Docker socket would have
// taken something away rather than added anything.

import { useEffect, useState } from "react";

import * as remote from "../remote";
import { RootMark } from "../ui/primitives";
import { Button, Input } from "../ui/primitives";

type Mode = "checking" | "claim" | "signIn";

export function ServerSignIn({
  server,
  onSignedIn,
}: {
  server: string;
  onSignedIn: () => void;
}) {
  const [mode, setMode] = useState<Mode>("checking");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState<string>();
  const [sso, setSso] = useState(false);
  const [busy, setBusy] = useState(false);

  // Ask the server which door this is rather than inferring it. An anonymous
  // caller is refused the account list with 401 whether or not the list is
  // empty, so status codes cannot tell these apart — the server says.
  useEffect(() => {
    let cancelled = false;
    fetch(`${server}/healthz`)
      .then((r) => r.json())
      .then((health) => {
        if (cancelled) return;
        setMode(health.accounts && !health.claimed ? "claim" : "signIn");
        setSso(Boolean(health.sso));
      })
      .catch(() => !cancelled && setMode("signIn"));
    return () => {
      cancelled = true;
    };
  }, [server]);

  // A failed round trip comes back here rather than to a dead end, carrying
  // the provider's own message — the only part anyone can act on.
  useEffect(() => {
    const failed = new URLSearchParams(window.location.search).get("sso_error");
    if (failed) setError(failed);
  }, []);

  const submit = async () => {
    setBusy(true);
    setError(undefined);
    try {
      if (mode === "claim") {
        const created = await fetch(`${server}/auth/users`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ email, password, displayName: name || email }),
        });
        if (!created.ok) {
          throw new Error((await created.json()).error ?? "that did not work");
        }
      }

      const response = await fetch(`${server}/auth/login`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ email, password }),
      });
      const body = await response.json();
      if (!response.ok) throw new Error(body.error ?? "that did not work");

      remote.setToken(body.token);
      onSignedIn();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  if (mode === "checking") return null;

  const claiming = mode === "claim";

  return (
    <div className="flex min-h-dvh items-center justify-center bg-page px-6">
      <div className="panel flex w-full max-w-[420px] flex-col gap-4 p-6">
        <div className="flex items-center gap-2.5">
          <RootMark size={22} />
          <span className="num text-2xs tracking-[.18em] text-ink-3 uppercase">root</span>
        </div>

        <div>
          <h1 className="text-2xl leading-tight font-medium tracking-[-.02em] text-ink">
            {claiming ? "Claim this server" : "Sign in"}
          </h1>
          <p className="mt-2 text-sm leading-relaxed text-ink-2">
            {claiming
              ? "Nobody has an account here yet. The first one is an administrator, and every account after it has to be created by one."
              : "This root server keeps accounts. Your session lasts a week of inactivity."}
          </p>
          <p className="num mt-2 text-2xs text-ink-4">{server}</p>
        </div>

        <form
          className="flex flex-col gap-2.5"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          {claiming && (
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Your name"
              autoComplete="name"
            />
          )}
          <Input
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            placeholder="you@example.com"
            type="email"
            autoComplete="username"
            required
          />
          <Input
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder={claiming ? "At least 12 characters" : "Password"}
            type="password"
            autoComplete={claiming ? "new-password" : "current-password"}
            required
          />

          {error && (
            <p className="num text-2xs leading-relaxed text-crit" role="alert">
              {error}
            </p>
          )}

          <Button variant="primary" type="submit" disabled={busy}>
            {busy ? "…" : claiming ? "Create the first account" : "Sign in"}
          </Button>
        </form>

        {sso && (
          <>
            <div className="flex items-center gap-3">
              <span className="h-px flex-1 bg-line" />
              <span className="num text-2xs text-ink-4">or</span>
              <span className="h-px flex-1 bg-line" />
            </div>
            <Button onClick={() => window.location.assign(`${server}/auth/oidc/start`)}>
              Continue with single sign-on
            </Button>
          </>
        )}

        {/* A server that has a shared token configured can also be reached
            with it. Said plainly rather than hidden, because somebody
            automating against this needs to know it is the supported path. */}
        <p className="num text-2xs leading-relaxed text-ink-4">
          Scripts and scrapes use the server's shared token instead, as a bearer
          header. It has no account behind it and is treated as an administrator.
        </p>
      </div>
    </div>
  );
}
