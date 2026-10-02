import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { api, errorMessage } from "../api";
import { formatDateTime, formatRelative } from "../format";
import type {
  AlertSeverity,
  ChannelKind,
  Delivery,
  DeliveryEvent,
  Environment,
  NotificationsView,
  NotifyChannel,
} from "../types";
import { Button, Chip, cx, Empty, Field, Input, Panel, Segmented, Tag } from "../ui/primitives";
import { serverUrl } from "../remote";
import { CHANNEL_BRAND, ChannelMark } from "../ui/ChannelMark";
import { CheckIcon, CloseIcon, PlusIcon, TrashIcon } from "../ui/icons";

// ------------------------------------------------------------------- kinds

/** What the editor says about each kind. The brand itself is in `ui/ChannelMark`. */
interface KindInfo {
  pitch: string;
  namePlaceholder: string;
  targetLabel: string;
  targetPlaceholder: string;
  targetHelp: ReactNode;
}

const KINDS: Record<ChannelKind, KindInfo> = {
  desktop: {
    pitch: "A banner on this computer, even with root in the background.",
    namePlaceholder: "This Mac",
    targetLabel: "",
    targetPlaceholder: "",
    targetHelp: null,
  },
  slack: {
    pitch: "A channel, through an incoming webhook.",
    namePlaceholder: "#prod-alerts",
    targetLabel: "Incoming webhook URL",
    targetPlaceholder: "https://hooks.slack.com/services/…",
    targetHelp: (
      <>
        In Slack: your app → <b className="font-medium text-ink-3">Incoming Webhooks</b> → Add New
        Webhook to Workspace, pick the channel, copy the URL.
      </>
    ),
  },
  teams: {
    pitch: "A channel, through a Workflows webhook.",
    namePlaceholder: "Platform channel",
    targetLabel: "Workflow URL",
    targetPlaceholder: "https://prod-00.westus.logic.azure.com/workflows/…",
    targetHelp: (
      <>
        In the channel: ⋯ → <b className="font-medium text-ink-3">Workflows</b> → “Post to a
        channel when a webhook request is received”, then copy the URL it gives you. Older Office
        365 connector URLs work too, until Microsoft retires them.
      </>
    ),
  },
  discord: {
    pitch: "A channel, through a channel webhook.",
    namePlaceholder: "#ops",
    targetLabel: "Webhook URL",
    targetPlaceholder: "https://discord.com/api/webhooks/…",
    targetHelp: (
      <>
        Channel → Edit Channel → <b className="font-medium text-ink-3">Integrations</b> → Webhooks →
        New Webhook → Copy Webhook URL.
      </>
    ),
  },
  pagerduty: {
    pitch: "Pages whoever is on call for a service.",
    namePlaceholder: "Payments on-call",
    targetLabel: "Integration key",
    targetPlaceholder: "32 characters",
    targetHelp: (
      <>
        Service → <b className="font-medium text-ink-3">Integrations</b> → Add an integration →
        Events API v2, then copy its integration key. US service region.
      </>
    ),
  },
  webhook: {
    pitch: "Any URL that takes a JSON POST.",
    namePlaceholder: "Incident bot",
    targetLabel: "URL",
    targetPlaceholder: "https://…",
    targetHelp: (
      <>
        root POSTs <code className="num text-ink-3">{"{ version, event, summary, alerts[] }"}</code>
        . Plain http is allowed here, for a receiver inside your network.
      </>
    ),
  },
};

const KIND_ORDER: ChannelKind[] = ["desktop", "slack", "teams", "pagerduty", "discord", "webhook"];

/**
 * A desktop banner needs the desktop app: talking to a server, there is no
 * notification centre on this side to show one in, so the kind is not offered.
 */
const offered = (kind: ChannelKind) => kind !== "desktop" || !serverUrl();

// ------------------------------------------------------------------- rules

const SEVERITY_RULE: Record<AlertSeverity, { label: string; color: string }> = {
  critical: { label: "Critical only", color: "var(--color-crit)" },
  warning: { label: "Warning and up", color: "var(--color-warn)" },
  info: { label: "Everything", color: "var(--color-ink-3)" },
};

const ENVIRONMENTS: Environment[] = ["production", "staging", "development", "shared"];

const DELAYS = [0, 5, 15, 30];

const capitalise = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

function delayLabel(minutes: number): string {
  return minutes === 0 ? "At once" : `After ${minutes} min unacknowledged`;
}

function blank(kind: ChannelKind): NotifyChannel {
  return {
    id: "",
    name: "",
    kind,
    enabled: true,
    minSeverity: kind === "pagerduty" ? "critical" : "warning",
    environments: [],
    teams: [],
    delayMinutes: kind === "pagerduty" ? 5 : 0,
    sendResolved: true,
    targetHint: null,
  };
}

// -------------------------------------------------------------------- tab

/** `failed` rather than `error`: a Delivery has an `error` field of its own. */
type TestState = "sending" | Delivery | { failed: string };

/**
 * Where root tells people.
 *
 * Leads with the escalation path rather than a bare list of channels, because
 * the order is the thing to get right: what is told at once, and what only
 * hears about an alert nobody has picked up. A list of five channels does not
 * say that; a line with stops on it does.
 */
export function NotificationSettings() {
  const [view, setView] = useState<NotificationsView | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [draft, setDraft] = useState<NotifyChannel | null>(null);
  const [picking, setPicking] = useState(false);
  const [tests, setTests] = useState<Record<string, TestState>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [rowError, setRowError] = useState<{ id: string; message: string } | null>(null);
  const editorRef = useRef<HTMLDivElement>(null);

  const load = useCallback(() => {
    api
      .getNotifications()
      .then((next) => {
        setView(next);
        setLoadError(null);
      })
      .catch((e) => setLoadError(errorMessage(e)));
  }, []);

  useEffect(load, [load]);

  // The editor opens above the list; bring it into view when it does.
  useEffect(() => {
    if (draft) editorRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [draft?.id, draft?.kind]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!view) return <Empty title={loadError ?? "Loading…"} />;

  const startNew = (kind: ChannelKind) => {
    setPicking(false);
    setDraft(blank(kind));
  };

  const toggle = async (channel: NotifyChannel) => {
    setBusy(channel.id);
    setRowError(null);
    try {
      setView(await api.saveNotificationChannel({ ...channel, enabled: !channel.enabled }));
    } catch (e) {
      setRowError({ id: channel.id, message: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };

  const remove = async (channel: NotifyChannel) => {
    setBusy(channel.id);
    setRowError(null);
    try {
      setView(await api.deleteNotificationChannel(channel.id));
      if (draft?.id === channel.id) setDraft(null);
    } catch (e) {
      setRowError({ id: channel.id, message: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };

  const test = async (channel: NotifyChannel) => {
    setTests((t) => ({ ...t, [channel.id]: "sending" }));
    try {
      const delivery = await api.testNotificationChannel(channel.id);
      setTests((t) => ({ ...t, [channel.id]: delivery }));
    } catch (e) {
      setTests((t) => ({ ...t, [channel.id]: { failed: errorMessage(e) } }));
    }
    load();
  };

  const channels = view.channels;

  return (
    <div className="flex max-w-4xl flex-col gap-3">
      {!view.keychainAvailable && (
        <div
          className="rounded-md border px-3 py-2 text-xs text-crit"
          style={{ borderColor: "color-mix(in oklab, var(--color-crit) 40%, transparent)" }}
        >
          The system keychain is not available, and every webhook URL and key is kept there. Channels
          cannot be saved until it is.
        </div>
      )}

      {draft && (
        <div ref={editorRef}>
          <ChannelEditor
            key={draft.id || `new-${draft.kind}`}
            initial={draft}
            teams={view.teams}
            onCancel={() => setDraft(null)}
            onSaved={(next) => {
              setView(next);
              setDraft(null);
            }}
          />
        </div>
      )}

      <Panel
        title="Channels"
        subtitle={
          channels.length === 0
            ? "none yet"
            : `${channels.length} · each hears the alerts that match its rules`
        }
        actions={
          !draft && (
            <Button size="sm" onClick={() => setPicking((p) => !p)}>
              <PlusIcon size={12} />
              Add channel
            </Button>
          )
        }
        flush
      >
        {(picking || (channels.length === 0 && !draft)) && <KindPicker onPick={startNew} />}

        {channels.length > 0 && <EscalationPath channels={channels} />}

        {channels.map((channel) => (
          <ChannelRow
            key={channel.id}
            channel={channel}
            busy={busy === channel.id}
            test={tests[channel.id]}
            error={rowError?.id === channel.id ? rowError.message : undefined}
            editing={draft?.id === channel.id}
            onToggle={() => toggle(channel)}
            onTest={() => test(channel)}
            onEdit={() => {
              setPicking(false);
              setDraft({ ...channel });
            }}
            onDelete={() => remove(channel)}
          />
        ))}
      </Panel>

      <DeliveryLog deliveries={view.deliveries} />
    </div>
  );
}

// ------------------------------------------------------------- the picker

function KindPicker({ onPick }: { onPick: (kind: ChannelKind) => void }) {
  return (
    <div className="grid gap-2 border-b border-line-soft p-3 @2xl:grid-cols-2 @4xl:grid-cols-3">
      {KIND_ORDER.filter(offered).map((kind) => (
        <button
          key={kind}
          type="button"
          onClick={() => onPick(kind)}
          className="group flex items-center gap-3 rounded-md border border-line bg-sunk/60 p-2.5 text-left transition-colors hover:border-line-strong hover:bg-raised"
        >
          <ChannelMark kind={kind} size={32} />
          <span className="min-w-0">
            <span className="block text-xs font-medium text-ink">{CHANNEL_BRAND[kind].label}</span>
            <span className="block text-2xs leading-snug text-ink-4">{KINDS[kind].pitch}</span>
          </span>
          <PlusIcon
            size={13}
            className="ml-auto text-ink-4 opacity-0 transition-opacity group-hover:opacity-100"
          />
        </button>
      ))}
    </div>
  );
}

// -------------------------------------------------------- the escalation

/**
 * Who hears, and when, if nobody acknowledges: one stop per distinct delay.
 *
 * Drawn because it is the one thing the list cannot say. "PagerDuty, 5 min"
 * in a row reads as a setting; the same channel standing five minutes down a
 * line from the chat channels reads as what will happen at three in the
 * morning.
 */
function EscalationPath({ channels }: { channels: NotifyChannel[] }) {
  const live = channels.filter((c) => c.enabled);
  const stops = [...new Set(live.map((c) => c.delayMinutes))].sort((a, b) => a - b);
  if (stops.length === 0) return null;

  return (
    <div className="border-b border-line-soft px-3 pt-3 pb-3.5">
      <div className="mb-3 flex items-baseline gap-2">
        <span className="label">Escalation path</span>
        <span className="text-2xs text-ink-4">who hears, and when, if nobody acknowledges</span>
      </div>
      <ol className="flex flex-wrap gap-y-4">
        {stops.map((delay, i) => {
          // Later stops are escalations, and say so in the warning colour.
          const tone = delay === 0 ? "var(--color-ink-3)" : "var(--color-warn)";
          return (
            <li key={delay} className="relative min-w-[180px] flex-1 pr-4">
              <span
                className="absolute top-[3px] right-0 left-0 h-px"
                style={{
                  background:
                    i === stops.length - 1
                      ? `linear-gradient(to right, ${tone}, transparent)`
                      : `color-mix(in srgb, ${tone} 55%, transparent)`,
                }}
              />
              <span
                className="relative block h-[7px] w-[7px] rounded-full"
                style={{
                  background: tone,
                  // The panel ring and the glow in one shadow: an escalation
                  // stop is lit amber, "at once" only faintly.
                  boxShadow: `0 0 0 4px var(--color-panel), 0 0 10px 3px color-mix(in srgb, ${tone} ${delay === 0 ? 25 : 55}%, transparent)`,
                }}
              />
              <span
                className="mt-2 block text-2xs font-semibold tracking-[.06em] uppercase"
                style={{ color: tone }}
              >
                {delay === 0 ? "At once" : `+${delay} min, if unacknowledged`}
              </span>
              <ul className="mt-1.5 flex flex-col gap-1.5">
                {live
                  .filter((c) => c.delayMinutes === delay)
                  .map((c) => (
                    <li key={c.id} className="flex min-w-0 items-start gap-2">
                      <ChannelMark kind={c.kind} size={18} />
                      <span className="min-w-0">
                        <span className="block truncate text-xs leading-[18px] text-ink-2">
                          {c.name}
                        </span>
                        <span className="block truncate text-2xs text-ink-4">{ruleShort(c)}</span>
                      </span>
                    </li>
                  ))}
              </ul>
            </li>
          );
        })}
      </ol>
    </div>
  );
}

/** "critical · production" — the rule in the fewest words. */
function ruleShort(channel: NotifyChannel): string {
  const parts: string[] = [channel.minSeverity === "info" ? "all" : `${channel.minSeverity}+`];
  if (channel.minSeverity === "critical") parts[0] = "critical";
  if (channel.environments.length) parts.push(channel.environments.join(", "));
  if (channel.teams.length) parts.push(channel.teams.join(", "));
  return parts.join(" · ");
}

// -------------------------------------------------------------- the rows

function ChannelRow({
  channel,
  busy,
  test,
  error,
  editing,
  onToggle,
  onTest,
  onEdit,
  onDelete,
}: {
  channel: NotifyChannel;
  busy: boolean;
  test?: TestState;
  error?: string;
  editing: boolean;
  onToggle: () => void;
  onTest: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  // Two clicks to delete, the second within a few seconds: a channel is
  // somebody's pager, and there is no undo.
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), 3000);
    return () => clearTimeout(timer);
  }, [armed]);

  const severity = SEVERITY_RULE[channel.minSeverity];
  const resolves = channel.sendResolved || channel.kind === "pagerduty";

  return (
    <div
      className={cx(
        "flex flex-col gap-2 border-b border-line-soft px-3 py-3 last:border-0",
        !channel.enabled && "opacity-60",
        editing && "bg-wash",
      )}
    >
      <div className="flex min-w-0 items-start gap-3">
        <ChannelMark kind={channel.kind} size={30} />
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-2">
            <span className="truncate text-sm font-medium text-ink">{channel.name}</span>
            {!channel.enabled && <Tag>paused</Tag>}
          </div>
          <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-2xs text-ink-4">
            <span className="shrink-0">{CHANNEL_BRAND[channel.kind].label}</span>
            {channel.targetHint && (
              <>
                <span aria-hidden>·</span>
                <span className="num truncate" title="Stored in the system keychain">
                  {channel.targetHint}
                </span>
              </>
            )}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
          <label
            className="flex cursor-pointer items-center gap-1.5 text-2xs text-ink-3"
            title={channel.enabled ? "Pause this channel" : "Resume this channel"}
          >
            <input
              type="checkbox"
              checked={channel.enabled}
              disabled={busy}
              onChange={onToggle}
              className="accent-accent"
            />
            On
          </label>
          <Button size="sm" onClick={onTest} disabled={test === "sending" || !channel.enabled}>
            {test === "sending" ? "Sending…" : "Send test"}
          </Button>
          <Button size="sm" variant="ghost" onClick={onEdit}>
            Edit
          </Button>
          <Button
            size="sm"
            variant={armed ? "danger" : "ghost"}
            onClick={() => (armed ? onDelete() : setArmed(true))}
            title="Delete this channel"
            aria-label={armed ? "Confirm delete" : "Delete"}
            className={armed ? "" : "px-1.5"}
          >
            {armed ? "Delete?" : <TrashIcon size={12} />}
          </Button>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-1.5 pl-[42px]">
        <Tag color={severity.color}>{severity.label}</Tag>
        {channel.environments.length === 0 ? (
          <Tag>All environments</Tag>
        ) : (
          channel.environments.map((env) => <Tag key={env}>{capitalise(env)}</Tag>)
        )}
        {channel.teams.length === 0 ? (
          <Tag>All teams</Tag>
        ) : (
          channel.teams.map((team) => <Tag key={team}>team {team}</Tag>)
        )}
        <Tag color={channel.delayMinutes > 0 ? "var(--color-warn)" : undefined}>
          {delayLabel(channel.delayMinutes)}
        </Tag>
        <Tag title={resolves ? "Told when an alert clears" : "Not told when an alert clears"}>
          {resolves ? "Resolutions" : "No resolutions"}
        </Tag>
      </div>

      {(test && test !== "sending") || error ? (
        <div className="pl-[42px]">
          {error ? (
            <Outcome ok={false} text={error} />
          ) : test && test !== "sending" && "failed" in test ? (
            <Outcome ok={false} text={test.failed} />
          ) : test && test !== "sending" && "ok" in test ? (
            <Outcome
              ok={test.ok}
              text={
                test.ok
                  ? `Test delivered in ${test.latencyMs} ms${test.status ? ` · ${test.status}` : ""} — check ${channel.name}.`
                  : (test.error ?? "Not delivered")
              }
            />
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function Outcome({ ok, text }: { ok: boolean; text: string }) {
  return (
    <p
      role={ok ? "status" : "alert"}
      className="flex items-start gap-1.5 text-2xs leading-snug"
      style={{
        color: ok ? "var(--color-ok)" : "var(--color-crit)",
        textShadow: `0 0 10px color-mix(in srgb, ${ok ? "var(--color-ok)" : "var(--color-crit)"} 40%, transparent)`,
      }}
    >
      <span className="mt-px shrink-0">{ok ? <CheckIcon size={11} /> : <CloseIcon size={11} />}</span>
      <span className="min-w-0">{text}</span>
    </p>
  );
}

// ------------------------------------------------------------- the editor

function ChannelEditor({
  initial,
  teams,
  onCancel,
  onSaved,
}: {
  initial: NotifyChannel;
  teams: string[];
  onCancel: () => void;
  onSaved: (view: NotificationsView) => void;
}) {
  const [channel, setChannel] = useState<NotifyChannel>(initial);
  const [target, setTarget] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const info = KINDS[channel.kind];
  const creating = !initial.id;
  const set = <K extends keyof NotifyChannel>(key: K, value: NotifyChannel[K]) =>
    setChannel((c) => ({ ...c, [key]: value }));

  const toggleIn = <T extends string>(list: T[], value: T): T[] =>
    list.includes(value) ? list.filter((v) => v !== value) : [...list, value];

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      onSaved(await api.saveNotificationChannel(channel, target));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Panel
      title={
        <span className="flex items-center gap-2">
          <ChannelMark kind={channel.kind} size={18} />
          {creating ? `New ${CHANNEL_BRAND[channel.kind].label} channel` : `Edit ${initial.name}`}
        </span>
      }
      actions={
        <Button size="sm" variant="ghost" onClick={onCancel} aria-label="Close" className="px-1.5">
          <CloseIcon size={12} />
        </Button>
      }
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <div className="grid gap-3 @3xl:grid-cols-2">
          <Field label="Name" required>
            <Input
              autoFocus
              value={channel.name}
              placeholder={info.namePlaceholder}
              onChange={(e) => set("name", e.target.value)}
            />
          </Field>
          {channel.kind === "desktop" ? (
            <Field
              label="Where"
              help="The notification centre of the machine running root. macOS may ask once whether root may show notifications."
            >
              <div className="flex h-7 items-center gap-2 text-xs text-ink-2">
                <ChannelMark kind="desktop" size={18} />
                This computer — no URL or key needed
              </div>
            </Field>
          ) : (
          <Field
            label={info.targetLabel}
            required={creating}
            help={info.targetHelp}
          >
            <Input
              type="password"
              autoComplete="off"
              spellCheck={false}
              value={target}
              placeholder={
                !creating && initial.targetHint
                  ? `Stored · ${initial.targetHint} — paste a new one to replace it`
                  : info.targetPlaceholder
              }
              onChange={(e) => setTarget(e.target.value)}
            />
          </Field>
          )}
        </div>

        <div className="grid gap-4 @3xl:grid-cols-2">
          <Field label="Severity" help="The least severe alert this channel hears about.">
            <Segmented<AlertSeverity>
              label="Severity"
              value={channel.minSeverity}
              onChange={(v) => set("minSeverity", v)}
              options={[
                { value: "critical", label: "Critical" },
                { value: "warning", label: "Warning and up" },
                { value: "info", label: "Everything" },
              ]}
            />
          </Field>

          <Field
            label="When"
            help={
              channel.delayMinutes > 0
                ? "Only alerts still unacknowledged after this long — acknowledging one in time means this channel never hears of it."
                : "As soon as an alert has held for 30 seconds, so alerts that fire together arrive as one message."
            }
          >
            <Segmented<string>
              label="When"
              value={String(channel.delayMinutes)}
              onChange={(v) => set("delayMinutes", Number(v))}
              options={DELAYS.map((d) => ({
                value: String(d),
                label: d === 0 ? "At once" : `${d} min`,
              }))}
            />
          </Field>
        </div>

        <Field label="Environments" help="None selected: every environment.">
          <div className="flex flex-wrap gap-1.5">
            {ENVIRONMENTS.map((env) => (
              <Chip
                key={env}
                selected={channel.environments.includes(env)}
                onClick={() => set("environments", toggleIn(channel.environments, env))}
                className="font-sans"
              >
                {capitalise(env)}
              </Chip>
            ))}
          </div>
        </Field>

        <Field
          label="Teams"
          help={
            teams.length === 0
              ? "No connection names a team yet. Set one under Connections → Ownership."
              : "None selected: every team, and connections with no team."
          }
        >
          {teams.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {teams.map((team) => (
                <Chip
                  key={team}
                  selected={channel.teams.some((t) => t.toLowerCase() === team.toLowerCase())}
                  onClick={() =>
                    set(
                      "teams",
                      channel.teams.some((t) => t.toLowerCase() === team.toLowerCase())
                        ? channel.teams.filter((t) => t.toLowerCase() !== team.toLowerCase())
                        : [...channel.teams, team],
                    )
                  }
                  className="font-sans"
                >
                  {team}
                </Chip>
              ))}
            </div>
          )}
        </Field>

        <div className="flex flex-col gap-2">
          <label className="flex cursor-pointer items-start gap-2 text-xs text-ink-2">
            <input
              type="checkbox"
              className="mt-0.5 accent-accent"
              checked={channel.sendResolved || channel.kind === "pagerduty"}
              disabled={channel.kind === "pagerduty"}
              onChange={(e) => set("sendResolved", e.target.checked)}
            />
            <span>
              Tell this channel when an alert clears
              {channel.kind === "pagerduty" && (
                <span className="block text-2xs text-ink-4">
                  Always, for PagerDuty — otherwise the incident it opened stays open.
                </span>
              )}
            </span>
          </label>
          <label className="flex cursor-pointer items-center gap-2 text-xs text-ink-2">
            <input
              type="checkbox"
              className="accent-accent"
              checked={channel.enabled}
              onChange={(e) => set("enabled", e.target.checked)}
            />
            On
          </label>
        </div>

        {error && <Outcome ok={false} text={error} />}

        <div className="flex items-center gap-2 border-t border-line-soft pt-3">
          <Button type="submit" variant="primary" loading={saving}>
            {creating ? "Add channel" : "Save"}
          </Button>
          <Button type="button" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          {channel.kind !== "desktop" && (
            <span className="ml-auto text-2xs text-ink-4">
              The URL or key is kept in the system keychain, never in root's config file.
            </span>
          )}
        </div>
      </form>
    </Panel>
  );
}

// ---------------------------------------------------------------- the log

const EVENT_TAG: Record<DeliveryEvent, { label: string; color?: string }> = {
  firing: { label: "firing", color: "var(--color-crit)" },
  resolved: { label: "resolved", color: "var(--color-ok)" },
  acknowledged: { label: "acknowledged" },
  test: { label: "test", color: "var(--color-data)" },
};

function DeliveryLog({ deliveries }: { deliveries: Delivery[] }) {
  const failed = deliveries.filter((d) => !d.ok).length;
  return (
    <Panel
      title="Recent deliveries"
      subtitle={
        deliveries.length === 0
          ? "nothing sent yet"
          : failed > 0
            ? `${deliveries.length} · ${failed} failed`
            : `${deliveries.length} · all delivered`
      }
      flush
    >
      {deliveries.length === 0 ? (
        <p className="px-3 py-4 text-xs text-ink-4">
          Nothing has been sent this session. Use <span className="text-ink-3">Send test</span> on a
          channel to check it works before an alert has to.
        </p>
      ) : (
        <ul>
          {deliveries.map((d) => {
            const tag = EVENT_TAG[d.event];
            return (
              <li
                key={d.id}
                className={cx(
                  "flex flex-col gap-1 border-b border-line-soft px-3 py-2 last:border-0",
                  !d.ok && "hue",
                )}
                style={{ "--hue": "var(--color-crit)" } as React.CSSProperties}
              >
                <div className="flex min-w-0 items-center gap-2">
                  <span
                    className="h-1.5 w-1.5 shrink-0 rounded-full"
                    style={{
                      background: d.ok ? "var(--color-ok)" : "var(--color-crit)",
                      boxShadow: `0 0 6px 1px color-mix(in srgb, ${d.ok ? "var(--color-ok)" : "var(--color-crit)"} ${d.ok ? 35 : 65}%, transparent)`,
                    }}
                    title={d.ok ? "Delivered" : "Not delivered"}
                  />
                  <ChannelMark kind={d.kind} size={16} />
                  <span className="shrink-0 text-xs text-ink-2">{d.channelName}</span>
                  <Tag color={tag.color}>{tag.label}</Tag>
                  <span className="min-w-0 flex-1 truncate text-2xs text-ink-4" title={d.summary}>
                    {d.summary}
                  </span>
                  <span className="num shrink-0 text-2xs text-ink-4">
                    {d.status ?? "—"} · {d.latencyMs} ms
                  </span>
                  <span
                    className="num w-16 shrink-0 text-right text-2xs text-ink-4"
                    title={formatDateTime(d.at)}
                  >
                    {formatRelative(d.at)}
                  </span>
                </div>
                {!d.ok && d.error && (
                  <p className="pl-[34px] text-2xs leading-snug text-crit">
                    {d.error}
                    {d.attempts > 1 && <span className="text-ink-4"> · after {d.attempts} tries</span>}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </Panel>
  );
}
