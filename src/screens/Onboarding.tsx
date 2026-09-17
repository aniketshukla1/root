import { useMemo, useState } from "react";

import type { Catalog } from "../catalog";
import type { AppConfig, ServiceDescriptor } from "../types";
import { blankProfile, ProfileForm } from "./Settings";
import { Button, cx, Empty, Input, PanelHeader, RootMark, ServiceMark } from "../ui/primitives";
import { CheckIcon, SearchIcon } from "../ui/icons";

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
        <span className="font-mono text-xs tracking-[.18em] text-ink uppercase">root</span>

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
