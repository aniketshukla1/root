import { useEffect, useMemo, useState } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import type { AppConfig, LlmProviderKind, LlmSettings, ServiceDescriptor } from "../types";
import { blankProfile, ProfileForm } from "./Settings";
import {
  Button,
  cx,
  Empty,
  Input,
  PanelHeader,
  RootMark,
  Segmented,
  Select,
  ServiceMark,
} from "../ui/primitives";
import { CheckIcon, SearchIcon } from "../ui/icons";
import { DemoLauncher } from "../ui/demo";

/**
 * First run: connect something.
 *
 * root has no opinion about what you run. It asks the backend which collectors
 * exist and renders whatever it is handed, so this screen is a view of the
 * catalog rather than a list somebody typed — a collector added in Rust shows
 * up here without a line of TypeScript changing.
 *
 * The designed flow had three steps, the third being "invite your team". That
 * step is not here: there is no team backend, and a form that collects invites
 * and drops them on the floor is worse than no form. What is here is real —
 * the catalog, the collector's own fields, a live connection test, and a saved
 * profile that the estate starts polling immediately.
 */
export function Onboarding({
  catalog,
  onConfigChange,
  onSkip,
}: {
  catalog: Catalog;
  onConfigChange: (config: AppConfig) => void;
  onSkip: () => void;
}) {
  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState<ServiceDescriptor | null>(null);
  const [watching, setWatching] = useState(false);
  const [watchError, setWatchError] = useState<string>();
  const thisMachine = catalog.has("this_machine") ? catalog.get("this_machine") : null;

  // One click to real data: a connection to the machine root is running on,
  // saved like any other, so the estate starts polling it at once.
  const watchThisComputer = async () => {
    if (!thisMachine) return;
    setWatching(true);
    setWatchError(undefined);
    try {
      onConfigChange(await api.saveProfile({ ...blankProfile(thisMachine), name: "This computer" }));
    } catch (e) {
      setWatchError(errorMessage(e));
      setWatching(false);
    }
  };

  const groups = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return catalog
      .byCategory()
      .map(({ category, services }) => ({
        category,
        services: needle
          ? services.filter(
              (d) =>
                d.label.toLowerCase().includes(needle) ||
                d.kind.toLowerCase().includes(needle) ||
                d.blurb.toLowerCase().includes(needle),
            )
          : services,
      }))
      .filter((g) => g.services.length > 0);
  }, [catalog, query]);

  const total = groups.reduce((sum, g) => sum + g.services.length, 0);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-page">
      {/* One of three notches lit. The ribbon is the same device the estate
          uses for proportion, so it means the same thing here. */}
      <div className="flex h-[3px] shrink-0 gap-px" aria-hidden>
        <span className="grow bg-accent" />
        <span className="grow-[11] bg-sunk" />
      </div>

      <header className="flex h-[58px] shrink-0 items-center gap-3 border-b border-line px-6">
        <span className="grid h-[26px] w-[26px] place-items-center rounded-sm bg-accent text-on-accent">
          <RootMark size={16} />
        </span>
        <span className="text-[14px] font-semibold tracking-[-.01em] text-ink">root</span>

        <div className="ml-auto flex items-center gap-5">
          <Step done label="Workspace" />
          <Step current index={2} label="Connect a service" />
          <Button variant="ghost" onClick={onSkip}>
            Skip for now
          </Button>
        </div>
      </header>

      <div className="flex min-h-0 flex-1 justify-center overflow-y-auto p-6">
        <div className="flex w-full max-w-[960px] flex-col">
          {/* The fastest way to real data needs nothing typed: the machine
              root is running on. Ahead of the demo, because real beats
              simulated whenever it is this cheap. */}
          {thisMachine && (
            <section
              className="panel hue mb-4 flex flex-col gap-3 px-4 py-3.5 sm:flex-row sm:items-center"
              style={{ "--hue": "var(--color-ok)" } as React.CSSProperties}
            >
              <ServiceMark descriptor={thisMachine} size={28} />
              <div className="min-w-0 flex-1">
                <div className="text-base text-ink">Watch this computer</div>
                <p className="mt-0.5 text-2xs leading-snug text-ink-4">
                  CPU, memory, every disk, network and the busiest processes, read directly from
                  the machine root runs on. Nothing to install and nothing to type.
                </p>
                {watchError && <p className="mt-1 text-2xs text-crit">{watchError}</p>}
              </div>
              <Button variant="primary" loading={watching} onClick={() => void watchThisComputer()}>
                Start watching
              </Button>
            </section>
          )}

          <AssistantSetup onConfigChange={onConfigChange} />

          {/* Nothing to connect yet is the commonest first run, and the
              estate is where every screen earns its keep — so the demo comes
              ahead of the catalogue rather than after it. */}
          <div className="mb-8">
            <DemoLauncher config={null} onConfigChange={onConfigChange} />
          </div>

          <p className="label mb-3">Step 2 of 2</p>
          <h1 className="text-5xl leading-[1.06] font-medium tracking-[-.03em] text-ink">
            Connect something you actually run.
          </h1>
          <p className="mt-3 max-w-[66ch] text-sm leading-relaxed text-ink-3">
            root has no opinion about what you run — it asks the backend what collectors exist and
            renders whatever it is handed. Pick one to start with; the rest can wait, and nothing
            here is irreversible.
          </p>

          {/* shrink-0, because this is a flex item in a column with no
              definite height: without it the panel is squeezed to its header
              and `overflow-hidden` clips the entire catalogue away. The same
              trap Screen guards against for every other screen — this one
              builds its own frame, so it has to say so itself. */}
          <section className="panel mt-7 flex shrink-0 flex-col overflow-hidden">
            <PanelHeader
              title="Service types"
              subtitle={`${total} available`}
              actions={
                <div className="flex w-[190px] items-center gap-1.5">
                  <SearchIcon size={12} style={{ color: "var(--color-ink-4)" }} />
                  <Input
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    placeholder="Search"
                    size="sm"
                  />
                </div>
              }
            />

            <div className="p-3.5">
              {catalog.isEmpty ? (
                <Empty
                  title="Catalog unavailable"
                  hint="The backend has not published a service catalog, so there is nothing to pick from yet."
                />
              ) : total === 0 ? (
                <Empty
                  title={`Nothing matches "${query}"`}
                  hint="Every collector the backend publishes is searchable by name, kind, or what it does."
                />
              ) : (
                groups.map(({ category, services }) => (
                  <div key={category} className="mb-4 last:mb-0">
                    <div className="label mb-2">{category}</div>
                    <div className="grid gap-2 [grid-template-columns:repeat(auto-fill,minmax(230px,1fr))]">
                      {services.map((descriptor) => (
                        <button
                          key={descriptor.kind}
                          type="button"
                          onClick={() => setPicked(descriptor)}
                          className={cx(
                            "flex items-start gap-2.5 rounded-md border p-2.5 text-left transition-colors",
                            "border-line hover:border-accent hover:bg-accent-soft",
                          )}
                        >
                          <ServiceMark descriptor={descriptor} size={22} />
                          <span className="min-w-0 flex-1">
                            <span className="block text-base text-ink">{descriptor.label}</span>
                            <span className="mt-0.5 block text-2xs leading-snug text-ink-4">
                              {descriptor.blurb}
                            </span>
                          </span>
                        </button>
                      ))}
                    </div>
                  </div>
                ))
              )}
            </div>
          </section>

          <p className="mt-4 text-2xs leading-relaxed text-ink-4">
            Credentials go to the operating system keychain, never to a file root writes. A
            connection can be tested before it is saved, and removed afterwards without trace.
          </p>
        </div>
      </div>

      {/* The same form Settings uses, not a second one that could drift from
          it — including its live connection test and its dependency field. */}
      {picked && (
        <ProfileForm
          profile={blankProfile(picked)}
          descriptor={picked}
          siblings={[]}
          onClose={() => setPicked(null)}
          onSaved={(config) => {
            setPicked(null);
            onConfigChange(config);
          }}
        />
      )}
    </div>
  );
}

/**
 * The assistant, set up before anyone has to find Settings: an Ollama already
 * running here is offered in one click, and otherwise a key can be pasted.
 * Optional — every screen works without a model.
 */
function AssistantSetup({ onConfigChange }: { onConfigChange: (config: AppConfig) => void }) {
  // undefined while asking; null when no Ollama answered.
  const [ollama, setOllama] = useState<string[] | null | undefined>(undefined);
  const [model, setModel] = useState("");
  const [provider, setProvider] = useState<LlmProviderKind>("anthropic");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [ready, setReady] = useState<string | null>(null);
  const [error, setError] = useState<string>();

  useEffect(() => {
    let live = true;
    api
      .detectOllama()
      .then((models) => {
        if (!live) return;
        setOllama(models);
        if (models && models.length > 0) setModel(models[0]);
      })
      .catch(() => live && setOllama(null));
    return () => {
      live = false;
    };
  }, []);

  // An empty model asks the backend for the provider's own default, so this
  // screen keeps no second copy of the defaults.
  const save = async (choice: Pick<LlmSettings, "provider" | "model">, apiKey?: string) => {
    setBusy(true);
    setError(undefined);
    try {
      const current = (await api.getConfig()).llm;
      const config = await api.saveLlmSettings({ ...current, ...choice, baseUrl: null }, apiKey);
      onConfigChange(config);
      setReady(`${config.llm.provider === "ollama" ? "Ollama" : "your key"} · ${config.llm.model}`);
      setKey("");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="panel mb-8 px-4 py-3.5">
      <div className="flex flex-wrap items-baseline gap-x-2">
        <span className="text-base text-ink">Set up the assistant</span>
        <span className="text-2xs text-ink-4">optional — every screen works without one</span>
      </div>
      {ready ? (
        <p className="mt-2 text-xs text-ink-2">
          <span style={{ color: "var(--color-ok)" }}>Ready</span> — {ready}. Change it any time in
          Settings.
        </p>
      ) : (
        <div className="mt-3 flex flex-col gap-2.5">
          {ollama === undefined ? (
            <p className="text-2xs text-ink-4">Looking for Ollama on this machine…</p>
          ) : ollama && ollama.length > 0 ? (
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-xs text-ink-2">Ollama is running here.</span>
              <Select
                value={model}
                onChange={(e) => setModel(e.target.value)}
                size="sm"
                aria-label="Ollama model"
              >
                {ollama.map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))}
              </Select>
              <Button
                variant="primary"
                loading={busy}
                onClick={() => void save({ provider: "ollama", model })}
              >
                Use Ollama
              </Button>
              <span className="text-2xs text-ink-4">nothing leaves this machine</span>
            </div>
          ) : (
            <p className="text-2xs text-ink-4">No Ollama answered on localhost:11434.</p>
          )}
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs text-ink-2">Or a key:</span>
            <Segmented
              label="Provider"
              value={provider}
              onChange={setProvider}
              options={[
                { value: "anthropic", label: "Anthropic" },
                { value: "open_ai_compatible", label: "OpenAI-compatible" },
              ]}
            />
            <Input
              type="password"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder="Paste an API key"
              size="sm"
              className="w-56"
              aria-label="API key"
              autoComplete="off"
            />
            <Button
              disabled={!key.trim()}
              loading={busy}
              onClick={() => void save({ provider, model: "" }, key.trim())}
            >
              Save key
            </Button>
          </div>
          {error && <p className="text-2xs text-crit">{error}</p>}
        </div>
      )}
    </section>
  );
}

function Step({
  done,
  current,
  index,
  label,
}: {
  done?: boolean;
  current?: boolean;
  index?: number;
  label: string;
}) {
  return (
    <span
      className="flex items-center gap-2 font-mono text-2xs"
      style={{ color: current ? "var(--color-accent)" : "var(--color-ink-3)" }}
    >
      {done ? (
        <CheckIcon size={13} style={{ color: "var(--color-ok)" }} />
      ) : (
        <span
          className="grid h-[15px] w-[15px] place-items-center rounded-full bg-accent text-2xs font-semibold text-on-accent"
          aria-hidden
        >
          {index}
        </span>
      )}
      {label}
    </span>
  );
}
