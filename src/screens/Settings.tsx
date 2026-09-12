import { useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import type { Catalog } from "../catalog";
import { applyDensity, readDensity, type Density } from "../density";
import { applyTheme, readTheme, watchSystemTheme, type Theme } from "../theme";
import type {
  ConfigSource,
  Runtime,
  HistorySource,
  AgentMode,
  AppConfig,
  ConnectionProfile,
  Criticality,
  Environment,
  FieldSpec,
  HealthSnapshot,
  IngestStatus,
  LlmProviderKind,
  LlmSettings,
  OperationalMetadata,
  ServiceDescriptor,
} from "../types";
import { Screen, ScreenHeader } from "../shell/ScreenHeader";
import { TeamSettings } from "./TeamSettings";
import {
  Button,
  cx,
  Empty,
  HealthBadge,
  Input,
  Panel,
  Segmented,
  Select,
  ServiceMark,
  Tabs,
  Tag,
} from "../ui/primitives";
import { CloseIcon, PlusIcon, TrashIcon } from "../ui/icons";

type Tab = "connections" | "team" | "assistant" | "ingest" | "display";

export function Settings({
  config,
  catalog,
  openProfileId,
  onConfigChange,
  onAsk,
}: {
  config: AppConfig | null;
  catalog: Catalog;
  /** Opens this connection's editor on arrival. */
  openProfileId?: string;
  onConfigChange: (config: AppConfig) => void;
  onAsk: () => void;
}) {
  const [tab, setTab] = useState<Tab>("connections");

  return (
    <>
      <ScreenHeader title="Settings" onAsk={onAsk} />
      <Screen padded={false} className="flex flex-col">
        <Tabs
          value={tab}
          onChange={setTab}
          options={[
            { value: "connections", label: "Connections", count: config?.profiles.length },
            { value: "team", label: "Workspace" },
            { value: "assistant", label: "Assistant" },
            { value: "ingest", label: "Deploy ingest" },
            { value: "display", label: "Display" },
          ]}
        />
        <div className="p-4">
          {tab === "connections" && (
            <Connections
              config={config}
              catalog={catalog}
              openProfileId={openProfileId}
              onConfigChange={onConfigChange}
            />
          )}
          {tab === "team" && <TeamSettings />}
          {tab === "assistant" && (
            <AssistantSettings config={config} onConfigChange={onConfigChange} />
          )}
          {tab === "ingest" && <IngestSettings />}
          {tab === "display" && <DisplaySettings />}
        </div>
      </Screen>
    </>
  );
}

// --------------------------------------------------------------- connections

function Connections({
  config,
  catalog,
  openProfileId,
  onConfigChange,
}: {
  config: AppConfig | null;
  catalog: Catalog;
  openProfileId?: string;
  onConfigChange: (config: AppConfig) => void;
}) {
  const [editing, setEditing] = useState<ConnectionProfile | null>(null);

  const [picking, setPicking] = useState(false);
  const [keychain, setKeychain] = useState<boolean | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    api.secretStoreAvailable().then(setKeychain).catch(() => setKeychain(false));
  }, []);

  const profiles = config?.profiles ?? [];

  /**
   * Arriving with a connection named opens its editor directly.
   *
   * The incident screen's "could not see" disclosure links here to have
   * dependencies declared. Landing on a list of twelve connections and asking
   * the operator to find the right one again is most of the friction the
   * disclosure was meant to remove.
   */
  useEffect(() => {
    if (!openProfileId) return;
    const target = profiles.find((p) => p.id === openProfileId);
    if (target) setEditing(target);
  }, [openProfileId, config]);

  const toggle = async (profile: ConnectionProfile) => {
    setBusy(profile.id);
    try {
      onConfigChange(await api.saveProfile({ ...profile, enabled: !profile.enabled }));
    } catch (error) {
      console.error("toggle failed:", errorMessage(error));
    } finally {
      setBusy(null);
    }
  };

  const remove = async (profile: ConnectionProfile) => {
    if (!confirm(`Delete “${profile.name}”? Its stored credential is removed from the keychain too.`))
      return;
    setBusy(profile.id);
    try {
      onConfigChange(await api.deleteProfile(profile.id));
    } catch (error) {
      console.error("delete failed:", errorMessage(error));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex flex-col gap-3">
      {keychain === false && (
        <div
          className="rounded-lg border px-3 py-2 text-xs"
          style={{
            borderColor: "color-mix(in oklab, var(--color-warn) 35%, transparent)",
            background: "color-mix(in oklab, var(--color-warn) 8%, transparent)",
          }}
        >
          <span className="font-medium text-warn">Keychain unavailable.</span>{" "}
          <span className="text-ink-2">
            Credentials cannot be stored on this system. Connections that need one will fail to
            authenticate.
          </span>
        </div>
      )}

      <Panel
        title="Connections"
        subtitle={`${profiles.filter((p) => p.enabled).length} of ${profiles.length} enabled`}
        actions={
          <Button variant="primary" onClick={() => setPicking(true)}>
            <PlusIcon size={13} />
            Add connection
          </Button>
        }
        flush
      >
        {profiles.length === 0 ? (
          <Empty
            title="No connections"
            hint="Add one to start collecting health, metrics, and logs from your infrastructure."
          />
        ) : (
          <table className="w-full border-collapse">
            <thead>
              <tr className="border-b border-line">
                <th className="label px-3 py-1.5 text-left font-semibold">Name</th>
                <th className="label px-3 py-1.5 text-left font-semibold">Type</th>
                <th className="label px-3 py-1.5 text-left font-semibold">Endpoint</th>
                <th className="label hidden px-3 py-1.5 text-left font-semibold lg:table-cell">
                  Team
                </th>
                <th className="label px-3 py-1.5 text-right font-semibold">Poll</th>
                <th className="label px-3 py-1.5 text-left font-semibold">Credential</th>
                <th className="label px-3 py-1.5 text-left font-semibold">Enabled</th>
                <th className="w-px" />
              </tr>
            </thead>
            <tbody>
              {profiles.map((profile) => {
                const descriptor = catalog.get(profile.kind);
                const ops = profile.metadata?.operations as OperationalMetadata | undefined;
                return (
                  <tr
                    key={profile.id}
                    className="group border-b border-line/50 last:border-0 hover:bg-white/[0.035]"
                    style={{ height: "var(--row-h)" }}
                  >
                    <td className="max-w-52 px-3">
                      <button
                        onClick={() => setEditing(profile)}
                        className="flex min-w-0 items-center gap-2 text-xs font-medium text-ink transition-colors hover:text-accent"
                      >
                        <ServiceMark descriptor={descriptor} size={16} />
                        <span className="truncate">{profile.name}</span>
                      </button>
                    </td>
                    <td className="px-3 text-xs text-ink-3">{descriptor.label}</td>
                    <td className="num max-w-64 truncate px-3 text-xs text-ink-3" title={profile.endpoint}>
                      {profile.endpoint || <span className="text-ink-4">local socket</span>}
                    </td>
                    <td className="hidden max-w-40 truncate px-3 text-xs text-ink-3 lg:table-cell">
                      {ops?.team || <span className="text-ink-4 italic">Unassigned</span>}
                    </td>
                    <td className="num px-3 text-right text-xs text-ink-3">
                      {profile.pollIntervalSecs}s
                    </td>
                    <td className="px-3 text-xs">
                      {profile.hasSecret ? (
                        <Tag color="var(--color-ok)">Stored</Tag>
                      ) : (
                        <span className="text-ink-4">—</span>
                      )}
                    </td>
                    <td className="px-3">
                      <label className="flex cursor-pointer items-center gap-1.5 text-xs text-ink-3">
                        <input
                          type="checkbox"
                          checked={profile.enabled}
                          disabled={busy === profile.id}
                          onChange={() => toggle(profile)}
                          className="accent-accent"
                        />
                        {profile.enabled ? "On" : "Off"}
                      </label>
                    </td>
                    <td className="px-3">
                      <span className="flex justify-end gap-1 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
                        <Button variant="ghost" onClick={() => setEditing(profile)}>
                          Edit
                        </Button>
                        <Button
                          variant="danger"
                          onClick={() => remove(profile)}
                          disabled={busy === profile.id}
                          title={`Delete ${profile.name}`}
                        >
                          <TrashIcon size={12} />
                        </Button>
                      </span>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </Panel>

      {picking && (
        <KindPicker
          catalog={catalog}
          onCancel={() => setPicking(false)}
          onPick={(descriptor) => {
            setPicking(false);
            setEditing(blankProfile(descriptor));
          }}
        />
      )}

      {editing && (
        <ProfileForm
          profile={editing}
          descriptor={catalog.get(editing.kind)}
          siblings={profiles.filter((p) => p.id !== editing.id)}
          onClose={() => setEditing(null)}
          onSaved={(next) => {
            onConfigChange(next);
            setEditing(null);
          }}
        />
      )}
    </div>
  );
}

export function blankProfile(descriptor: ServiceDescriptor): ConnectionProfile {
  const options: Record<string, unknown> = {};
  for (const field of descriptor.fields) {
    if (field.default !== undefined) options[field.key] = field.default;
  }
  return {
    id: crypto.randomUUID(),
    name: "",
    kind: descriptor.kind,
    endpoint: "",
    username: null,
    hasSecret: false,
    enabled: true,
    pollIntervalSecs: descriptor.defaultPollSecs,
    dependsOn: [],
    configSources: [],
    runtime: { kind: "undeclared" },
    options,
    metadata: {},
  };
}

function KindPicker({
  catalog,
  onCancel,
  onPick,
}: {
  catalog: Catalog;
  onCancel: () => void;
  onPick: (descriptor: ServiceDescriptor) => void;
}) {
  return (
    <Modal title="Add a connection" onClose={onCancel} wide>
      {catalog.isEmpty ? (
        <Empty title="Catalog unavailable" hint="The backend has not published a service catalog." />
      ) : (
        <div className="flex flex-col gap-4">
          {catalog.byCategory().map(({ category, services }) => (
            <div key={category}>
              <h3 className="label mb-1.5">{category}</h3>
              <div className="grid gap-1.5 [grid-template-columns:repeat(auto-fill,minmax(230px,1fr))]">
                {services.map((descriptor) => (
                  <button
                    key={descriptor.kind}
                    onClick={() => onPick(descriptor)}
                    className="flex items-start gap-2 rounded-md border border-line bg-page p-2 text-left transition-colors hover:border-line-strong hover:bg-raised"
                  >
                    <ServiceMark descriptor={descriptor} size={20} />
                    <span className="min-w-0">
                      <span className="block text-xs font-medium text-ink">{descriptor.label}</span>
                      <span className="block text-2xs leading-snug text-ink-4">
                        {descriptor.blurb}
                      </span>
                    </span>
                  </button>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}

const ENVIRONMENTS: Environment[] = ["production", "staging", "development", "shared"];
const TIERS: Criticality[] = ["tier_0", "tier_1", "tier_2", "tier_3"];

/** Exported so first-run onboarding uses the same form, not a second one. */
export function ProfileForm({
  profile,
  descriptor,
  siblings,
  onClose,
  onSaved,
}: {
  profile: ConnectionProfile;
  descriptor: ServiceDescriptor;
  /** Everything else configured — the only things this can depend on. */
  siblings: ConnectionProfile[];
  onClose: () => void;
  onSaved: (config: AppConfig) => void;
}) {
  const [draft, setDraft] = useState<ConnectionProfile>(profile);
  const [secret, setSecret] = useState("");

  /**
   * Metric keys this connection actually reports.
   *
   * Read from the last poll rather than offered as free text: a mapping typed
   * against a key that does not exist produces no series and no error, which
   * is the worst of both. Empty for a connection that has never been polled,
   * and the editor falls back to accepting anything.
   */
  const [metricKeys, setMetricKeys] = useState<string[]>([]);
  useEffect(() => {
    let live = true;
    api
      .getDashboard()
      .then((dashboard) => {
        if (!live) return;
        const snapshot = dashboard.snapshots.find((s) => s.profileId === profile.id);
        setMetricKeys(snapshot?.metrics.map((m) => m.key) ?? []);
      })
      .catch(() => {
        /* A form that cannot reach the backend still has to open. */
      });
    return () => {
      live = false;
    };
  }, [profile.id]);
  const [advanced, setAdvanced] = useState(false);
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<{ ok: boolean; snapshot?: HealthSnapshot; error?: string } | null>(
    null,
  );
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const ops = (draft.metadata?.operations ?? {}) as OperationalMetadata;
  const isNew = !profile.name;

  const setOption = (key: string, value: unknown) =>
    setDraft((d) => ({ ...d, options: { ...d.options, [key]: value } }));

  const setOps = (patch: Partial<OperationalMetadata>) =>
    setDraft((d) => ({
      ...d,
      metadata: { ...d.metadata, operations: { ...ops, ...patch } },
    }));

  // `undefined` keeps whatever is already in the keychain; an empty string on a
  // new profile means there is nothing to store.
  const secretArg = secret ? secret : undefined;

  const runTest = async () => {
    setTesting(true);
    setTest(null);
    try {
      setTest({ ok: true, snapshot: await api.testProfile(draft, secretArg) });
    } catch (error) {
      setTest({ ok: false, error: errorMessage(error) });
    } finally {
      setTesting(false);
    }
  };

  const save = async () => {
    setSaving(true);
    setSaveError(null);
    try {
      onSaved(await api.saveProfile(draft, secretArg));
    } catch (error) {
      setSaveError(errorMessage(error));
    } finally {
      setSaving(false);
    }
  };

  const basic = descriptor.fields.filter((f) => !f.advanced);
  const extra = descriptor.fields.filter((f) => f.advanced);

  return (
    <Modal
      title={
        <span className="flex items-center gap-2">
          <ServiceMark descriptor={descriptor} size={18} />
          {isNew ? `New ${descriptor.label} connection` : draft.name}
        </span>
      }
      onClose={onClose}
      wide
      footer={
        <>
          {test && (
            <span className="mr-auto flex min-w-0 items-center gap-2 text-xs">
              {test.ok && test.snapshot ? (
                <>
                  <HealthBadge health={test.snapshot.health} />
                  <span className="truncate text-ink-3">{test.snapshot.summary}</span>
                </>
              ) : (
                <span className="num truncate text-crit" title={test.error}>
                  {test.error}
                </span>
              )}
            </span>
          )}
          {saveError && (
            <span className="mr-auto truncate text-xs text-crit" title={saveError}>
              {saveError}
            </span>
          )}
          <Button onClick={runTest} disabled={testing || saving}>
            {testing ? "Testing…" : "Test connection"}
          </Button>
          <Button variant="primary" onClick={save} disabled={saving || !draft.name.trim()}>
            {saving ? "Saving…" : "Save"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <Fieldset title="Connection">
          <Field label="Name" required help="How this connection appears throughout the app.">
            <Input
              className="w-full"
              value={draft.name}
              autoFocus
              placeholder={`${descriptor.label} — production`}
              onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))}
            />
          </Field>

          <SpecField
            spec={descriptor.endpoint}
            value={draft.endpoint}
            onChange={(value) => setDraft((d) => ({ ...d, endpoint: String(value ?? "") }))}
          />

          {descriptor.username && (
            <SpecField
              spec={descriptor.username}
              value={draft.username ?? ""}
              onChange={(value) =>
                setDraft((d) => ({ ...d, username: value ? String(value) : null }))
              }
            />
          )}

          {descriptor.credential && (
            <Field
              label={descriptor.credential.label}
              required={descriptor.credential.required && !draft.hasSecret}
              help={
                draft.hasSecret
                  ? "A credential is stored in the OS keychain. Leave blank to keep it."
                  : descriptor.credential.help
              }
            >
              <Input
                type="password"
                className="w-full"
                value={secret}
                placeholder={draft.hasSecret ? "••••••••  (unchanged)" : descriptor.credential.placeholder}
                onChange={(e) => setSecret(e.target.value)}
              />
            </Field>
          )}

          <Field label="Poll interval" help="How often this connection is collected.">
            <div className="flex items-center gap-2">
              <Input
                type="number"
                min={1}
                className="w-24"
                value={draft.pollIntervalSecs}
                onChange={(e) =>
                  setDraft((d) => ({
                    ...d,
                    pollIntervalSecs: Math.max(1, Number(e.target.value) || descriptor.defaultPollSecs),
                  }))
                }
              />
              <span className="text-xs text-ink-4">seconds</span>
            </div>
          </Field>

          <Field
            label="Depends on"
            help="What this connection needs in order to work. Declared, not discovered — root watches connections, not the network between them. Used to tell which failure explains the others; what depends on this is derived, so only declare one direction."
          >
            {siblings.length === 0 ? (
              <p className="text-xs text-ink-4">
                Nothing else is configured yet. Dependencies can be added once there is more than
                one connection.
              </p>
            ) : (
              <div className="flex flex-wrap gap-1.5">
                {siblings.map((sibling) => {
                  const on = draft.dependsOn.includes(sibling.id);
                  return (
                    <button
                      key={sibling.id}
                      type="button"
                      onClick={() =>
                        setDraft((d) => ({
                          ...d,
                          dependsOn: on
                            ? d.dependsOn.filter((id) => id !== sibling.id)
                            : [...d.dependsOn, sibling.id],
                        }))
                      }
                      className={cx(
                        "inline-flex h-[22px] items-center rounded-sm border px-2 font-mono text-2xs tracking-[.04em] transition-colors",
                        on
                          ? "border-accent bg-accent text-on-accent"
                          : "border-line bg-panel text-ink-3 hover:border-accent hover:text-accent",
                      )}
                    >
                      {sibling.name || sibling.id}
                    </button>
                  );
                })}
              </div>
            )}
          </Field>

          <Field
            label="Configuration files"
            help="Absolute paths to this connection's config. Declared, not discovered — root does not go looking for files that might be config, because a tool that goes looking eventually opens something it should not have. Credentials are stripped before anything leaves this machine, and files that are entirely secret (.env, private keys) are named but never opened."
          >
            <ConfigSources
              sources={draft.configSources}
              onChange={(configSources) => setDraft((d) => ({ ...d, configSources }))}
            />
          </Field>

          <Field
            label="Where this runs"
            help="Optional, and root works the same without it. root already reads a container's image and published port to work out what it is — but nothing checks that, so a wrong answer is wrong quietly. Saying where this actually runs gives the inference something to be wrong against, and the disagreements show up on the Scorecard."
          >
            <RuntimeField
              runtime={draft.runtime}
              hosts={siblings.filter((s) =>
                ["docker", "podman", "kubernetes"].includes(s.kind),
              )}
              onChange={(runtime) => setDraft((d) => ({ ...d, runtime }))}
            />
          </Field>

          <Field
            label="Long-range history"
            help="root keeps a few hours in memory, which is enough for a chart and not enough for a forecast — a volume that fills in three weeks is invisible to it. If you already run Prometheus, point this at it and say which PromQL produces each metric. Read-only; nothing is written there."
          >
            <PrometheusHistory
              source={draft.historySource}
              metricKeys={metricKeys}
              candidates={siblings.filter((s) => s.kind === "prometheus")}
              onChange={(historySource) => setDraft((d) => ({ ...d, historySource }))}
            />
          </Field>
        </Fieldset>

        {basic.length > 0 && (
          <Fieldset title={`${descriptor.label} options`}>
            {basic.map((spec) => (
              <SpecField
                key={spec.key}
                spec={spec}
                value={draft.options[spec.key]}
                onChange={(value) => setOption(spec.key, value)}
              />
            ))}
          </Fieldset>
        )}

        <Fieldset
          title="Ownership"
          hint="Never guessed. A connection with no team shows as unassigned and lowers coverage."
        >
          <Field label="Team">
            <Input
              className="w-full"
              value={ops.team ?? ""}
              placeholder="Platform Runtime"
              onChange={(e) => setOps({ team: e.target.value })}
            />
          </Field>
          <Field label="Responder">
            <Input
              className="w-full"
              value={ops.owner ?? ""}
              placeholder="On-call rotation or a name"
              onChange={(e) => setOps({ owner: e.target.value })}
            />
          </Field>
          <Field label="Contact">
            <Input
              className="w-full"
              value={ops.contact ?? ""}
              placeholder="#platform-oncall or an email"
              onChange={(e) => setOps({ contact: e.target.value })}
            />
          </Field>
          <Field label="Environment">
            <Select
              className="w-full"
              value={ops.environment ?? "shared"}
              onChange={(e) => setOps({ environment: e.target.value as Environment })}
            >
              {ENVIRONMENTS.map((env) => (
                <option key={env} value={env}>
                  {env.charAt(0).toUpperCase() + env.slice(1)}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="Criticality">
            <Select
              className="w-full"
              value={ops.criticality ?? "tier_2"}
              onChange={(e) => setOps({ criticality: e.target.value as Criticality })}
            >
              {TIERS.map((tier) => (
                <option key={tier} value={tier}>
                  Tier {tier.replace("tier_", "")}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="Runbook URL">
            <Input
              className="w-full"
              value={ops.runbookUrl ?? ""}
              placeholder="https://…"
              onChange={(e) => setOps({ runbookUrl: e.target.value })}
            />
          </Field>
        </Fieldset>

        {extra.length > 0 && (
          <div>
            <button
              onClick={() => setAdvanced((v) => !v)}
              className="text-xs text-ink-4 transition-colors hover:text-ink-2"
            >
              {advanced ? "Hide" : "Show"} advanced options ({extra.length})
            </button>
            {advanced && (
              <div className="mt-2">
                <Fieldset title="Advanced">
                  {extra.map((spec) => (
                    <SpecField
                      key={spec.key}
                      spec={spec}
                      value={draft.options[spec.key]}
                      onChange={(value) => setOption(spec.key, value)}
                    />
                  ))}
                </Fieldset>
              </div>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}

/** One form control, rendered entirely from the backend's FieldSpec. */
function SpecField({
  spec,
  value,
  onChange,
}: {
  spec: FieldSpec;
  value: unknown;
  onChange: (value: unknown) => void;
}) {
  if (spec.kind === "boolean") {
    return (
      <Field label={spec.label} help={spec.help}>
        <label className="flex cursor-pointer items-center gap-2 text-xs text-ink-2">
          <input
            type="checkbox"
            checked={Boolean(value)}
            onChange={(e) => onChange(e.target.checked)}
            className="accent-accent"
          />
          Enabled
        </label>
      </Field>
    );
  }

  if (spec.kind === "select") {
    return (
      <Field label={spec.label} required={spec.required} help={spec.help}>
        <Select
          className="w-full"
          value={String(value ?? spec.default ?? "")}
          onChange={(e) => onChange(e.target.value)}
        >
          {spec.options?.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </Select>
      </Field>
    );
  }

  return (
    <Field label={spec.label} required={spec.required} help={spec.help}>
      <Input
        className="w-full"
        type={spec.kind === "password" ? "password" : spec.kind === "number" ? "number" : "text"}
        min={spec.min}
        max={spec.max}
        value={value === null || value === undefined ? "" : String(value)}
        placeholder={spec.placeholder}
        onChange={(e) =>
          onChange(spec.kind === "number" ? Number(e.target.value) || undefined : e.target.value)
        }
      />
    </Field>
  );
}

function Fieldset({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <section>
      <h3 className="label mb-0.5">{title}</h3>
      {hint && <p className="mb-2 text-2xs text-ink-4">{hint}</p>}
      <div className="grid gap-2.5 [grid-template-columns:repeat(auto-fill,minmax(240px,1fr))]">
        {children}
      </div>
    </section>
  );
}

function Field({
  label,
  required,
  help,
  children,
}: {
  label: string;
  required?: boolean;
  help?: string;
  children: React.ReactNode;
}) {
  return (
    <label className="flex min-w-0 flex-col gap-1">
      <span className="text-xs text-ink-3">
        {label}
        {required && <span className="ml-0.5 text-crit">*</span>}
      </span>
      {children}
      {help && <span className="text-2xs leading-snug text-ink-4">{help}</span>}
    </label>
  );
}

// ----------------------------------------------------------------- assistant

const PROVIDERS: { value: LlmProviderKind; label: string; hint: string }[] = [
  { value: "anthropic", label: "Anthropic", hint: "The Messages API." },
  {
    value: "open_ai_compatible",
    label: "OpenAI-compatible",
    hint: "Any /chat/completions endpoint — OpenAI, Groq, OpenRouter, Together, vLLM, LM Studio.",
  },
  { value: "ollama", label: "Ollama", hint: "A local server. No key, nothing leaves the machine." },
];

function AssistantSettings({
  config,
  onConfigChange,
}: {
  config: AppConfig | null;
  onConfigChange: (config: AppConfig) => void;
}) {
  const [draft, setDraft] = useState<LlmSettings | null>(config?.llm ?? null);
  const [apiKey, setApiKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [loadingModels, setLoadingModels] = useState(false);

  useEffect(() => {
    if (config?.llm) setDraft(config.llm);
  }, [config?.llm]);

  if (!draft) {
    return <Empty title="Loading settings…" />;
  }

  const provider = PROVIDERS.find((p) => p.value === draft.provider);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      onConfigChange(await api.saveLlmSettings(draft, apiKey || undefined));
      setApiKey("");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSaving(false);
    }
  };

  const loadModels = async () => {
    setLoadingModels(true);
    try {
      // Saving first means the backend queries with the key just entered.
      onConfigChange(await api.saveLlmSettings(draft, apiKey || undefined));
      setApiKey("");
      setModels(await api.listModels());
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoadingModels(false);
    }
  };

  return (
    <div className="grid max-w-4xl gap-3 lg:grid-cols-2">
      <Panel title="Model">
        <div className="flex flex-col gap-3">
          <Field label="Provider" help={provider?.hint}>
            <Select
              className="w-full"
              value={draft.provider}
              onChange={(e) =>
                setDraft({ ...draft, provider: e.target.value as LlmProviderKind })
              }
            >
              {PROVIDERS.map((p) => (
                <option key={p.value} value={p.value}>
                  {p.label}
                </option>
              ))}
            </Select>
          </Field>

          <Field label="Model">
            <div className="flex gap-1.5">
              <Input
                className="flex-1"
                list="model-options"
                value={draft.model}
                placeholder="claude-opus-5"
                onChange={(e) => setDraft({ ...draft, model: e.target.value })}
              />
              <Button onClick={loadModels} disabled={loadingModels}>
                {loadingModels ? "…" : "List"}
              </Button>
            </div>
            <datalist id="model-options">
              {models.map((model) => (
                <option key={model} value={model} />
              ))}
            </datalist>
          </Field>

          {draft.provider !== "anthropic" && (
            <Field
              label="Base URL"
              help={
                draft.provider === "ollama"
                  ? "Default: http://localhost:11434"
                  : "Point this at the service whose key you are using."
              }
            >
              <Input
                className="w-full"
                value={draft.baseUrl ?? ""}
                placeholder={
                  draft.provider === "ollama"
                    ? "http://localhost:11434"
                    : "https://api.openai.com/v1"
                }
                onChange={(e) => setDraft({ ...draft, baseUrl: e.target.value || null })}
              />
            </Field>
          )}

          {draft.provider !== "ollama" && (
            <Field
              label="API key"
              help={
                draft.hasApiKey
                  ? "A key is stored in the OS keychain. Leave blank to keep it."
                  : "Stored in the OS keychain, never in the config file."
              }
            >
              <Input
                type="password"
                className="w-full"
                value={apiKey}
                placeholder={draft.hasApiKey ? "••••••••  (unchanged)" : "sk-…"}
                onChange={(e) => setApiKey(e.target.value)}
              />
            </Field>
          )}

          <div className="grid grid-cols-2 gap-2.5">
            <Field label="Max tokens">
              <Input
                type="number"
                min={256}
                className="w-full"
                value={draft.maxTokens}
                onChange={(e) =>
                  setDraft({ ...draft, maxTokens: Number(e.target.value) || draft.maxTokens })
                }
              />
            </Field>
            <Field label="Temperature">
              <Input
                type="number"
                min={0}
                max={2}
                step={0.1}
                className="w-full"
                value={draft.temperature}
                onChange={(e) => setDraft({ ...draft, temperature: Number(e.target.value) })}
              />
            </Field>
          </div>

          <div className="flex items-center gap-2">
            <Button variant="primary" onClick={save} disabled={saving}>
              {saving ? "Saving…" : "Save"}
            </Button>
            {error && (
              <span className="truncate text-xs text-crit" title={error}>
                {error}
              </span>
            )}
          </div>
        </div>
      </Panel>

      <Panel title="Authority">
        <p className="mb-3 text-xs leading-relaxed text-ink-3">
          What the assistant is allowed to do. There is no “always allow” — under{" "}
          <span className="text-ink-2">Allow actions</span>, every mutating call still stops for
          explicit approval in a dialog naming the exact target.
        </p>

        <div className="flex flex-col gap-2">
          {(
            [
              {
                value: "read_only" as AgentMode,
                label: "Read-only",
                detail:
                  "Inspect everything, change nothing. Remediation is surfaced as an exact command for you to run.",
              },
              {
                value: "confirm_actions" as AgentMode,
                label: "Allow actions",
                detail:
                  "Adds container restart/start/stop, Jenkins build triggers, and query termination — each one approved individually.",
              },
            ]
          ).map((mode) => (
            <label
              key={mode.value}
              className={cx(
                "flex cursor-pointer gap-2.5 rounded-md border p-2.5 transition-colors",
                draft.agentMode === mode.value
                  ? "border-accent/50 bg-accent/8"
                  : "border-line bg-page hover:border-line-strong",
              )}
            >
              <input
                type="radio"
                name="agent-mode"
                checked={draft.agentMode === mode.value}
                onChange={() => setDraft({ ...draft, agentMode: mode.value })}
                className="mt-0.5 accent-accent"
              />
              <span className="min-w-0">
                <span className="block text-xs font-medium text-ink">{mode.label}</span>
                <span className="block text-2xs leading-snug text-ink-4">{mode.detail}</span>
              </span>
            </label>
          ))}
        </div>

        <p className="mt-3 text-2xs leading-snug text-ink-4">
          There is no arbitrary-SQL tool and no shell tool. Database access is a fixed set of named
          diagnostics; anything outside that is routed through a proposed command for a human.
        </p>
      </Panel>
    </div>
  );
}

// -------------------------------------------------------------------- ingest

/**
 * The local deploy-ingest listener.
 *
 * This screen opens a socket in an app that holds infrastructure credentials,
 * so it states the posture plainly rather than burying it: off by default,
 * loopback only, token required. A user should be able to read this panel and
 * know exactly what they just exposed.
 */
function IngestSettings() {
  const [status, setStatus] = useState<IngestStatus | null>(null);
  const [port, setPort] = useState(7717);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Held only until the panel unmounts — the backend cannot return it again. */
  const [freshToken, setFreshToken] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    api
      .getIngestSettings()
      .then((s) => {
        setStatus(s);
        setPort(s.port);
      })
      .catch((e) => setError(errorMessage(e)));
  }, []);

  if (!status) {
    return <Empty title={error ?? "Loading…"} />;
  }

  const apply = async (enabled: boolean, nextPort: number) => {
    setBusy(true);
    setError(null);
    try {
      const next = await api.saveIngestSettings(enabled, nextPort);
      setStatus(next);
      setPort(next.port);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const rotate = async () => {
    setBusy(true);
    setError(null);
    try {
      setFreshToken(await api.rotateIngestToken());
      setStatus(await api.getIngestSettings());
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const copy = async (text: string, what: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(what);
      setTimeout(() => setCopied(null), 1500);
    } catch {
      setError("Could not reach the clipboard.");
    }
  };

  const example = [
    `curl -X POST ${status.url} \\`,
    `  -H "Authorization: Bearer $ROOT_INGEST_TOKEN" \\`,
    `  -H "Content-Type: application/json" \\`,
    `  -d '{"service":"checkout-api","version":"1.9.0","connection":"prod-docker"}'`,
  ].join("\n");

  return (
    <div className="grid max-w-4xl gap-3 lg:grid-cols-2">
      <Panel title="Listener">
        <div className="flex flex-col gap-3">
          <label
            className={cx(
              "flex cursor-pointer gap-2.5 rounded-md border p-2.5 transition-colors",
              status.enabled ? "border-accent/50 bg-accent/8" : "border-line bg-page",
            )}
          >
            <input
              type="checkbox"
              checked={status.enabled}
              disabled={busy}
              onChange={(e) => apply(e.target.checked, port)}
              className="mt-0.5 accent-accent"
            />
            <span className="min-w-0">
              <span className="block text-xs font-medium text-ink">
                Accept deploy events on this machine
              </span>
              <span className="block text-2xs leading-snug text-ink-4">
                Off by default. While on, root listens on loopback for POSTs describing a
                deploy, and each one becomes a marker on that connection's timeline.
              </span>
            </span>
          </label>

          <Field
            label="Port"
            help="Above 1023. Changing it moves the listener immediately."
          >
            <div className="flex gap-1.5">
              <Input
                type="number"
                min={1024}
                max={65535}
                className="w-28"
                value={port}
                onChange={(e) => setPort(Number(e.target.value) || 7717)}
              />
              <Button
                onClick={() => apply(status.enabled, port)}
                disabled={busy || port === status.port}
              >
                Apply
              </Button>
            </div>
          </Field>

          <Field label="Endpoint">
            <div className="flex items-center gap-1.5">
              <code className="min-w-0 flex-1 truncate rounded-md border border-line bg-page px-2 py-1 text-2xs text-ink-2">
                {status.url}
              </code>
              <Button onClick={() => copy(status.url, "url")}>
                {copied === "url" ? "Copied" : "Copy"}
              </Button>
            </div>
          </Field>

          {error && (
            <p className="text-xs text-crit" role="alert">
              {error}
            </p>
          )}

          <div className="rounded-md border border-line bg-page p-2.5">
            <h4 className="label mb-1.5">What this does and does not expose</h4>
            <ul className="flex flex-col gap-1 text-2xs leading-snug text-ink-3">
              <li>
                Binds <span className="num text-ink-2">127.0.0.1</span> only — never
                <span className="num text-ink-2"> 0.0.0.0</span>. Nothing off this machine can
                reach it, whatever your firewall says.
              </li>
              <li>Every request needs the bearer token; it is compared in constant time.</li>
              <li>
                Accepts one shape of record and rejects unknown connections, so a marker always
                belongs to something real.
              </li>
              <li>
                A cloud CI runner cannot reach a laptop. The poster has to be on this machine —
                a deploy script, a git hook, a self-hosted runner — or arrive through a tunnel
                you set up.
              </li>
            </ul>
          </div>
        </div>
      </Panel>

      <Panel title="Token">
        <div className="flex flex-col gap-3">
          {status.keychainAvailable ? (
            <p className="text-xs leading-relaxed text-ink-3">
              Stored in the OS keychain, never in the config file. It is shown once when
              created — root cannot read it back, so rotate if you lose it.
            </p>
          ) : (
            <p className="text-xs text-warn">
              The keychain is unavailable on this system, so no token can be stored and the
              listener will refuse to start.
            </p>
          )}

          <div className="flex items-center gap-2">
            <span className="text-xs text-ink-4">Status</span>
            {status.hasToken ? (
              <Tag color="var(--color-ok)">Token stored</Tag>
            ) : (
              <Tag color="var(--color-warn)">No token</Tag>
            )}
            <Button className="ml-auto" onClick={rotate} disabled={busy}>
              {status.hasToken ? "Rotate" : "Generate"}
            </Button>
          </div>

          {freshToken && (
            <div
              className="rounded-md border p-2.5"
              style={{
                borderColor: "color-mix(in oklab, var(--color-warn) 40%, transparent)",
                background: "color-mix(in oklab, var(--color-warn) 8%, transparent)",
              }}
            >
              <p className="mb-1.5 text-2xs text-warn">
                Copy this now — it will not be shown again. Any pipeline using the previous
                token has already stopped working.
              </p>
              <div className="flex items-center gap-1.5">
                <code className="min-w-0 flex-1 truncate rounded border border-line bg-page px-2 py-1 text-2xs text-ink">
                  {freshToken}
                </code>
                <Button onClick={() => copy(freshToken, "token")}>
                  {copied === "token" ? "Copied" : "Copy"}
                </Button>
              </div>
            </div>
          )}

          <div>
            <div className="mb-1 flex items-center gap-2">
              <span className="label">Example</span>
              <Button
                variant="ghost"
                className="ml-auto"
                onClick={() => copy(example, "curl")}
              >
                {copied === "curl" ? "Copied" : "Copy"}
              </Button>
            </div>
            <pre className="overflow-x-auto rounded-md border border-line bg-page p-2.5 text-2xs leading-relaxed text-ink-2">
              {example}
            </pre>
            <p className="mt-1.5 text-2xs leading-snug text-ink-4">
              <span className="num text-ink-3">connection</span> matches a connection by id or
              name. <span className="num text-ink-3">previousVersion</span>,{" "}
              <span className="num text-ink-3">at</span> and{" "}
              <span className="num text-ink-3">note</span> are optional.
            </p>
          </div>
        </div>
      </Panel>
    </div>
  );
}

// ------------------------------------------------------------------- display

function DisplaySettings() {
  const [density, setDensity] = useState<Density>(readDensity);
  const [theme, setTheme] = useState<Theme>(readTheme);

  const change = (next: Density) => {
    setDensity(next);
    applyDensity(next);
  };

  const changeTheme = (next: Theme) => {
    setTheme(next);
    applyTheme(next);
  };

  // "System" keeps following the OS after it is chosen, rather than resolving
  // once and then drifting when the machine flips at sunset.
  useEffect(() => watchSystemTheme(theme, () => setTheme("system")), [theme]);

  return (
    <Panel title="Display" className="max-w-xl">
      <Field
        label="Theme"
        help="Light is warm paper rather than white: a dark console inverted onto cold white reads as a mistake, and on paper it reads as printed."
      >
        <Segmented
          value={theme}
          onChange={changeTheme}
          options={[
            { value: "dark", label: "Dark" },
            { value: "light", label: "Light" },
            { value: "system", label: "System" },
          ]}
        />
      </Field>

      <Field
        label="Density"
        help="Sets row height and type scale across every table and list in the app."
      >
        <Segmented
          value={density}
          onChange={change}
          options={[
            { value: "compact", label: "Compact" },
            { value: "comfortable", label: "Comfortable" },
          ]}
        />
      </Field>

      <div className="mt-4 overflow-hidden rounded-md border border-line">
        <table className="w-full border-collapse">
          <thead>
            <tr className="border-b border-line">
              <th className="label px-3 py-1.5 text-left font-semibold">Preview</th>
              <th className="label px-3 py-1.5 text-right font-semibold">Value</th>
            </tr>
          </thead>
          <tbody>
            {["prod-docker", "orders-db", "ci.acme.dev"].map((name, i) => (
              <tr
                key={name}
                className="border-b border-line/50 last:border-0"
                style={{ height: "var(--row-h)" }}
              >
                <td className="px-3 text-xs text-ink-2">{name}</td>
                <td className="num px-3 text-right text-ink-3">{(i + 1) * 37}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <p className="mt-3 text-2xs leading-snug text-ink-4">
        Dark is tuned for long sessions in a dim room; light is warm paper for a bright one. Both
        carry the full palette, and the status and identity colours are validated for colour-vision
        deficiency against each ground rather than only against the dark one.
      </p>
    </Panel>
  );
}

// --------------------------------------------------------------------- modal

function Modal({
  title,
  children,
  footer,
  onClose,
  wide,
}: {
  title: React.ReactNode;
  children: React.ReactNode;
  footer?: React.ReactNode;
  onClose: () => void;
  wide?: boolean;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/60 p-4 backdrop-blur-sm"
      onClick={onClose}
      role="dialog"
      aria-modal
    >
      <div
        className={cx(
          "panel flex max-h-[88vh] w-full flex-col bg-overlay shadow-2xl shadow-black/50",
          wide ? "max-w-3xl" : "max-w-md",
        )}
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex h-10 shrink-0 items-center gap-2 border-b border-line px-4">
          <h2 className="text-sm font-semibold text-ink">{title}</h2>
          <button
            onClick={onClose}
            className="ml-auto text-ink-4 transition-colors hover:text-ink"
            title="Close"
          >
            <CloseIcon size={15} />
          </button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto p-4">{children}</div>
        {footer && (
          <footer className="flex shrink-0 items-center gap-2 border-t border-line px-4 py-2.5">
            {footer}
          </footer>
        )}
      </div>
    </div>
  );
}

/**
 * The declared places a connection's config lives.
 *
 * A path and an optional glob, nothing more. There is no directory picker and
 * no browse button on purpose: those invite pointing root at a home directory
 * to see what it finds, and "what it finds" is the failure mode this whole
 * feature is built to avoid. Typing a path is a deliberate act.
 */
function ConfigSources({
  sources,
  onChange,
}: {
  sources: ConfigSource[];
  onChange: (sources: ConfigSource[]) => void;
}) {
  const update = (id: string, patch: Partial<ConfigSource>) =>
    onChange(sources.map((s) => (s.id === id ? { ...s, ...patch } : s)));

  return (
    <div className="flex flex-col gap-2">
      {sources.map((source) => (
        <div key={source.id} className="flex flex-col gap-1.5 rounded-md border border-line p-2">
          <div className="flex gap-1.5">
            <Input
              value={source.path}
              placeholder="/etc/nginx"
              onChange={(e) => update(source.id, { path: e.target.value })}
              className="flex-1"
            />
            <Button
              variant="ghost"
              onClick={() => onChange(sources.filter((s) => s.id !== source.id))}
            >
              Remove
            </Button>
          </div>
          <div className="flex items-center gap-1.5">
            <span className="num w-[52px] shrink-0 text-2xs text-ink-4">include</span>
            <Input
              value={source.include ?? ""}
              placeholder="*.conf — leave blank for a single file"
              onChange={(e) =>
                update(source.id, { include: e.target.value || undefined })
              }
              className="flex-1"
            />
          </div>
          {!source.path.startsWith("/") && source.path.length > 0 && (
            <p className="num text-2xs text-warn">
              The path must be absolute — a relative one means whatever directory the app
              happens to be running in.
            </p>
          )}
        </div>
      ))}

      <div>
        <Button
          variant="ghost"
          onClick={() =>
            onChange([
              ...sources,
              { id: `src-${Date.now().toString(36)}`, path: "", include: undefined },
            ])
          }
        >
          Add a path
        </Button>
      </div>

      {sources.length === 0 && (
        <p className="num text-2xs leading-relaxed text-ink-4">
          None declared. Many connections still report their running configuration directly —
          Redis, Postgres, Docker and Kubernetes all do — and that needs no setup here.
        </p>
      )}
    </div>
  );
}

/**
 * Which Prometheus holds this connection's long history, and how its series
 * map onto root's metrics.
 *
 * The mapping is typed out rather than guessed. `memory` and
 * `redis_memory_used_bytes` are not the same series and nothing can infer that
 * they correspond — a wrong guess would forecast the wrong number
 * convincingly, which is worse than forecasting nothing.
 */
function PrometheusHistory({
  source,
  metricKeys,
  candidates,
  onChange,
}: {
  source?: HistorySource;
  /** Metric keys this connection actually reports, so the mapping is checkable. */
  metricKeys: string[];
  candidates: ConnectionProfile[];
  onChange: (source: HistorySource | undefined) => void;
}) {
  if (candidates.length === 0) {
    return (
      <p className="num text-2xs leading-relaxed text-ink-4">
        No Prometheus connection is configured. Add one and it can serve as the history
        source for everything else.
      </p>
    );
  }

  const current = source ?? { prometheusProfileId: "", queries: [] };
  const update = (patch: Partial<HistorySource>) => {
    const next = { ...current, ...patch };
    // Clearing the server clears the whole thing rather than leaving orphan
    // queries pointed at nothing.
    onChange(next.prometheusProfileId ? next : undefined);
  };

  return (
    <div className="flex flex-col gap-2">
      <Select
        value={current.prometheusProfileId}
        onChange={(e) => update({ prometheusProfileId: e.target.value })}
      >
        <option value="">Not set — use root's own history</option>
        {candidates.map((c) => (
          <option key={c.id} value={c.id}>
            {c.name || c.id}
          </option>
        ))}
      </Select>

      {current.prometheusProfileId && (
        <>
          {current.queries.map((query, i) => (
            <div key={i} className="flex gap-1.5">
              {metricKeys.length > 0 ? (
                <Select
                  value={query.metricKey}
                  onChange={(e) => {
                    const queries = [...current.queries];
                    queries[i] = { ...query, metricKey: e.target.value };
                    update({ queries });
                  }}
                  className="w-[140px] shrink-0"
                >
                  <option value="">metric…</option>
                  {metricKeys.map((key) => (
                    <option key={key} value={key}>
                      {key}
                    </option>
                  ))}
                </Select>
              ) : (
                <Input
                  value={query.metricKey}
                  placeholder="metric key"
                  onChange={(e) => {
                    const queries = [...current.queries];
                    queries[i] = { ...query, metricKey: e.target.value };
                    update({ queries });
                  }}
                  className="w-[140px] shrink-0"
                />
              )}
              <Input
                value={query.expr}
                placeholder={'redis_memory_used_bytes{instance="10.0.0.2:9121"}'}
                onChange={(e) => {
                  const queries = [...current.queries];
                  queries[i] = { ...query, expr: e.target.value };
                  update({ queries });
                }}
                className="flex-1"
              />
              <Button
                variant="ghost"
                onClick={() => update({ queries: current.queries.filter((_, j) => j !== i) })}
              >
                Remove
              </Button>
            </div>
          ))}

          <div>
            <Button
              variant="ghost"
              onClick={() =>
                update({ queries: [...current.queries, { metricKey: "", expr: "" }] })
              }
            >
              Map a metric
            </Button>
          </div>

          <p className="num text-2xs leading-relaxed text-ink-4">
            Each expression must resolve to a single series — a query matching several is
            refused rather than having one picked for you, because a forecast for the wrong
            host looks exactly like a forecast for the right one.
          </p>
        </>
      )}
    </div>
  );
}

/**
 * Where a connection actually runs, declared rather than inferred.
 *
 * Deliberately three small inputs and no browse button. The point is a claim
 * somebody is willing to make — a picker that listed the running containers
 * and let you choose one would just be root's own inference wearing a
 * different hat, and it would agree with itself every time.
 */
function RuntimeField({
  runtime,
  hosts,
  onChange,
}: {
  runtime: Runtime;
  /** Docker hosts and Kubernetes clusters — the things that host others. */
  hosts: ConnectionProfile[];
  onChange: (runtime: Runtime) => void;
}) {
  const hosted = runtime.kind === "container" || runtime.kind === "pod";

  return (
    <div className="flex flex-col gap-2">
      <Select
        value={runtime.kind}
        onChange={(e) => {
          const kind = e.target.value as Runtime["kind"];
          // Clearing the kind clears what belonged to it, rather than leaving
          // a host and a name attached to "managed service".
          onChange(
            kind === "container" || kind === "pod"
              ? { ...runtime, kind }
              : { kind },
          );
        }}
      >
        <option value="undeclared">Not declared</option>
        <option value="container">A container</option>
        <option value="pod">A Kubernetes pod</option>
        <option value="host">A process on a machine</option>
        <option value="managed">A managed cloud service</option>
      </Select>

      {hosted && hosts.length === 0 && (
        <p className="num text-2xs leading-relaxed text-ink-4">
          No Docker host or Kubernetes cluster is configured, so there is nothing to check
          this against yet. The declaration is still recorded.
        </p>
      )}

      {hosted && hosts.length > 0 && (
        <>
          <Select
            value={runtime.hostProfileId ?? ""}
            onChange={(e) =>
              onChange({ ...runtime, hostProfileId: e.target.value || undefined })
            }
          >
            <option value="">Which host…</option>
            {hosts.map((host) => (
              <option key={host.id} value={host.id}>
                {host.name || host.id}
              </option>
            ))}
          </Select>
          <Input
            value={runtime.name ?? ""}
            placeholder={runtime.kind === "pod" ? "pod name" : "container name"}
            onChange={(e) => onChange({ ...runtime, name: e.target.value || undefined })}
          />
          {/* Half a declaration is a claim nobody can verify, so it is worth
              saying before it is saved rather than reporting it later. */}
          {(!runtime.hostProfileId || !runtime.name) && (
            <p className="num text-2xs leading-relaxed text-ink-4">
              Both a host and a name are needed. Without them there is nothing for root to
              check its own answer against.
            </p>
          )}
        </>
      )}
    </div>
  );
}
