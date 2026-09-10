import { useCallback, useEffect, useState } from "react";

import { api, errorMessage } from "../api";
import { formatRelative } from "../format";
import type { Member, Role, TeamState, Workspace } from "../types";
import { Button, cx, Empty, Input, Panel, Tag } from "../ui/primitives";
import { CheckIcon, ShieldIcon } from "../ui/icons";

const ROLE_LABEL: Record<Role, string> = {
  viewer: "Viewer",
  responder: "Responder",
  admin: "Admin",
  owner: "Owner",
};

/**
 * What each role may do, in the app's own words.
 *
 * This is what the UI should bother offering, not a permission check. The
 * server is the only place a Viewer can actually be stopped — the person this
 * would be enforcing against owns the machine it runs on.
 */
const ROLE_SUMMARY: Record<Role, string> = {
  viewer: "Read everything. Change nothing, and the assistant stays read-only.",
  responder: "Acknowledge alerts and run collector actions. Cannot change connections.",
  admin: "Add, edit and remove connections, and manage members.",
  owner: "Everything an admin can do, plus transferring the workspace.",
};

const ROLE_ORDER: Role[] = ["owner", "admin", "responder", "viewer"];

/**
 * Workspace settings.
 *
 * Local mode is the default and is not a degraded state, so this tab leads
 * with what local mode is rather than with an empty member list and a nag to
 * sign up. There is no hosted service to sign up to; a workspace is a server
 * the operator runs, and pointing root at one is additive.
 */
export function TeamSettings() {
  const [team, setTeam] = useState<TeamState | null>(null);
  const [workspace, setWorkspace] = useState<Workspace | null>(null);
  const [draftServer, setDraftServer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [membersError, setMembersError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const next = await api.getTeam();
      setTeam(next);
      setDraftServer(next.serverUrl ?? "");
      if (next.serverUrl && next.hasSession) {
        try {
          setWorkspace(await api.getWorkspace());
          setMembersError(null);
        } catch (e) {
          setWorkspace(null);
          setMembersError(errorMessage(e));
        }
      } else {
        setWorkspace(null);
      }
    } catch (e) {
      setError(errorMessage(e));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const saveServer = async (url?: string) => {
    setBusy(true);
    setError(null);
    try {
      const next = await api.saveTeamServer(url);
      setTeam(next);
      setDraftServer(next.serverUrl ?? "");
      await load();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const signOut = async () => {
    setBusy(true);
    try {
      setTeam(await api.signOut());
      setWorkspace(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  if (!team) return null;

  const local = !team.serverUrl;

  return (
    <div className="flex flex-col gap-3">
      <Panel
        title="Workspace"
        subtitle={local ? "local mode" : team.hasSession ? "signed in" : "not signed in"}
      >
        {local ? (
          <>
            <p className="max-w-[80ch] text-[12.5px] leading-relaxed text-ink-2">
              root is running locally. It reads the config file on this machine, keeps every
              credential in the OS keychain, and answers to nobody. Nothing leaves this computer,
              and nothing about it needs an account.
            </p>
            <p className="mt-2.5 max-w-[80ch] text-[12.5px] leading-relaxed text-ink-3">
              A workspace is a root server that <em className="text-ink-2 not-italic">you</em> run.
              It adds one thing: a shared identity, so several people can look at the same estate
              and be told apart, with a role attached. There is no hosted service and no account to
              create.
            </p>
          </>
        ) : (
          <div className="flex flex-col gap-2.5">
            <div className="flex items-center gap-2.5">
              <ShieldIcon size={14} style={{ color: "var(--color-accent)" }} />
              <span className="num text-xs text-ink-2">{team.serverUrl}</span>
              {team.account ? (
                <Tag color="var(--color-ok)">{ROLE_LABEL[team.account.role].toUpperCase()}</Tag>
              ) : (
                <Tag color="var(--color-warn)">NOT SIGNED IN</Tag>
              )}
            </div>
            {team.account && (
              <p className="num text-2xs text-ink-4">
                {team.account.name} · {team.account.email}
              </p>
            )}
            {team.account && (
              <p className="max-w-[80ch] text-2xs leading-relaxed text-ink-4">
                {ROLE_SUMMARY[team.account.role]} This is what root offers you; the server is what
                enforces it.
              </p>
            )}
          </div>
        )}

        <div className="mt-4 border-t border-line pt-3.5">
          <span className="label mb-1.5 block">Server address</span>
          <div className="flex max-w-[520px] gap-2">
            <Input
              value={draftServer}
              onChange={(e) => setDraftServer(e.target.value)}
              placeholder="root.example.internal"
              className="flex-1"
              spellCheck={false}
            />
            <Button
              variant="primary"
              disabled={busy || !draftServer.trim() || draftServer.trim() === team.serverUrl}
              onClick={() => saveServer(draftServer.trim())}
            >
              {team.serverUrl ? "Change" : "Connect"}
            </Button>
            {team.serverUrl && (
              <Button disabled={busy} onClick={() => saveServer(undefined)}>
                Use locally
              </Button>
            )}
            {team.hasSession && (
              <Button disabled={busy} onClick={signOut}>
                Sign out
              </Button>
            )}
          </div>
          <p className="mt-2 max-w-[80ch] text-2xs leading-relaxed text-ink-4">
            Clearing the address returns root to local mode and signs this machine out. Connections,
            history and credentials are untouched — they were never on the server.
          </p>
          {error && (
            <p className="mt-2 text-2xs leading-relaxed text-crit" role="alert">
              {error}
            </p>
          )}
        </div>
      </Panel>

      {!local && (
        <Panel
          title="Members"
          subtitle={workspace ? `${workspace.members.length} in this workspace` : "unavailable"}
          flush
          bodyClassName="overflow-x-auto"
        >
          {membersError ? (
            <div className="p-3">
              <p className="max-w-[80ch] text-2xs leading-relaxed text-ink-3">{membersError}</p>
            </div>
          ) : !workspace ? (
            <Empty
              title="Not signed in"
              hint="Sign in to this workspace to see who else is in it."
            />
          ) : (
            <table className="w-full min-w-[640px] text-left">
              <thead>
                <tr className="border-b border-line">
                  <th className="label px-3 py-1.5">Member</th>
                  <th className="label px-3 py-1.5">Role</th>
                  <th className="label px-3 py-1.5">Joined via</th>
                  <th className="label px-3 py-1.5 text-right">Last seen</th>
                </tr>
              </thead>
              <tbody>
                {[...workspace.members]
                  .sort(
                    (a, b) => ROLE_ORDER.indexOf(a.role) - ROLE_ORDER.indexOf(b.role) ||
                      a.name.localeCompare(b.name),
                  )
                  .map((member) => (
                    <MemberRow key={member.email} member={member} me={team.account?.email} />
                  ))}
              </tbody>
            </table>
          )}
        </Panel>
      )}

      {!local && workspace && (
        <Panel title="Single sign-on" subtitle="configured on the server">
          {workspace.sso.domain ? (
            <div className="flex flex-col gap-2.5">
              <div className="flex items-center gap-2.5">
                <CheckIcon size={14} style={{ color: "var(--color-ok)" }} />
                <span className="num text-xs text-ink-2">{workspace.sso.domain}</span>
                {workspace.sso.enforced && <Tag color="var(--color-accent)">ENFORCED</Tag>}
              </div>
              <p className="max-w-[80ch] text-2xs leading-relaxed text-ink-3">
                Anyone with an address at this domain can sign in without an invite, and arrives as{" "}
                <span className="text-ink-2">{ROLE_LABEL[workspace.sso.defaultRole]}</span>.
                {workspace.sso.defaultRole === "viewer" &&
                  " Read-only by default, because joining a workspace should never be an escalation."}
              </p>
            </div>
          ) : (
            <p className="text-2xs leading-relaxed text-ink-4">
              No SSO domain is configured, so every member arrives by invite.
            </p>
          )}
        </Panel>
      )}

      <Panel title="Roles" subtitle="what root offers each one">
        <div className="flex flex-col gap-2">
          {ROLE_ORDER.map((role) => (
            <div key={role} className="flex items-start gap-3">
              <span className="num w-[86px] shrink-0 text-xs text-ink-2">{ROLE_LABEL[role]}</span>
              <span className="min-w-0 flex-1 text-2xs leading-relaxed text-ink-4">
                {ROLE_SUMMARY[role]}
              </span>
            </div>
          ))}
        </div>
      </Panel>
    </div>
  );
}

function MemberRow({ member, me }: { member: Member; me?: string }) {
  return (
    <tr className={cx("border-b border-line-soft last:border-0", !member.lastSeen && "opacity-60")}>
      <td className="px-3 py-2">
        <span className="block text-xs text-ink">
          {member.name}
          {member.email === me && <span className="ml-1.5 text-2xs text-ink-4">you</span>}
        </span>
        <span className="num block text-2xs text-ink-4">{member.email}</span>
      </td>
      <td className="px-3 py-2">
        <span className="num text-2xs text-ink-2">{ROLE_LABEL[member.role]}</span>
      </td>
      <td className="px-3 py-2">
        <span className="num text-2xs text-ink-4">{member.viaSso ? "SSO" : "invite"}</span>
      </td>
      <td className="num px-3 py-2 text-right text-2xs text-ink-4">
        {/* Never signed in is not "a long time ago" — it is a different fact,
            and an invite that was never accepted is worth seeing as one. */}
        {member.lastSeen ? formatRelative(member.lastSeen) : "never signed in"}
      </td>
    </tr>
  );
}
