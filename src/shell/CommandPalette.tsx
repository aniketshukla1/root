import { useEffect, useMemo, useRef, useState } from "react";

import type { Catalog } from "../catalog";
import { healthLabel } from "../catalog";
import type { EstateModel } from "../model";
import type { View } from "../nav";
import { cx, Kbd, ServiceMark } from "../ui/primitives";
import {
  AlertsIcon,
  AssistantIcon,
  ScorecardIcon,
  LogsIcon,
  MetricsIcon,
  OverviewIcon,
  SearchIcon,
  ServicesIcon,
  SettingsIcon,
} from "../ui/icons";

interface Command {
  id: string;
  label: string;
  hint?: string;
  group: string;
  icon?: React.ReactNode;
  run: () => void;
}

export function CommandPalette({
  model,
  catalog,
  onNavigate,
  onRefresh,
  onClose,
}: {
  model: EstateModel;
  catalog: Catalog;
  onNavigate: (view: View) => void;
  onRefresh: () => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  useEffect(() => inputRef.current?.focus(), []);

  const commands = useMemo<Command[]>(() => {
    const go = (view: View) => () => {
      onNavigate(view);
      onClose();
    };

    const screens: Command[] = [
      { id: "overview", label: "Overview", group: "Go to", icon: <OverviewIcon size={14} />, run: go({ name: "overview" }) },
      { id: "services", label: "Services", group: "Go to", icon: <ServicesIcon size={14} />, run: go({ name: "services" }) },
      { id: "alerts", label: "Alerts", group: "Go to", icon: <AlertsIcon size={14} />, run: go({ name: "alerts" }) },
      { id: "metrics", label: "Metrics", group: "Go to", icon: <MetricsIcon size={14} />, run: go({ name: "metrics" }) },
      { id: "logs", label: "Logs", group: "Go to", icon: <LogsIcon size={14} />, run: go({ name: "logs" }) },
      { id: "assistant", label: "Assistant", group: "Go to", icon: <AssistantIcon size={14} />, run: go({ name: "assistant" }) },
      { id: "scorecard", label: "Scorecard", group: "Go to", icon: <ScorecardIcon size={14} />, run: go({ name: "scorecard" }) },
      { id: "settings", label: "Settings", group: "Go to", icon: <SettingsIcon size={14} />, run: go({ name: "settings" }) },
    ];

    const connections: Command[] = model.services.map((service) => ({
      id: `svc-${service.snapshot.profileId}`,
      label: service.snapshot.profileName,
      hint: `${catalog.label(service.snapshot.kind)} · ${healthLabel(service.health)}${
        service.issues.length > 0 ? ` · ${service.issues.length} open` : ""
      }`,
      group: "Connections",
      icon: <ServiceMark descriptor={catalog.get(service.snapshot.kind)} size={16} />,
      run: go({ name: "service", profileId: service.snapshot.profileId }),
    }));

    const actions: Command[] = [
      {
        id: "refresh",
        label: "Refresh all connections",
        group: "Actions",
        run: () => {
          onRefresh();
          onClose();
        },
      },
      {
        id: "add",
        label: "Add a connection",
        group: "Actions",
        run: go({ name: "settings" }),
      },
    ];

    return [...connections, ...screens, ...actions];
  }, [model.services, catalog, onNavigate, onRefresh, onClose]);

  const results = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return commands;
    return commands.filter(
      (command) =>
        command.label.toLowerCase().includes(needle) ||
        command.group.toLowerCase().includes(needle) ||
        (command.hint ?? "").toLowerCase().includes(needle),
    );
  }, [commands, query]);

  useEffect(() => setCursor(0), [query]);

  useEffect(() => {
    listRef.current
      ?.querySelector(`[data-index="${cursor}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setCursor((c) => Math.min(c + 1, results.length - 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setCursor((c) => Math.max(c - 1, 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      results[cursor]?.run();
    } else if (event.key === "Escape") {
      onClose();
    }
  };

  let lastGroup = "";

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 p-4 pt-[12vh] backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="panel flex max-h-[60vh] w-full max-w-lg flex-col overflow-hidden bg-overlay shadow-2xl shadow-black/50"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal
      >
        <div className="flex h-10 shrink-0 items-center gap-2 border-b border-line px-3">
          <SearchIcon size={14} />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder="Search connections and screens…"
            className="flex-1 bg-transparent text-[13px] text-ink placeholder:text-ink-4 focus:outline-none"
          />
          <Kbd>esc</Kbd>
        </div>

        <ul ref={listRef} className="min-h-0 flex-1 overflow-y-auto py-1">
          {results.length === 0 && (
            <li className="px-3 py-6 text-center text-xs text-ink-4">
              Nothing matches “{query}”.
            </li>
          )}
          {results.map((command, index) => {
            const header = command.group !== lastGroup ? command.group : null;
            lastGroup = command.group;
            return (
              <li key={command.id}>
                {header && <div className="label px-3 pt-2 pb-1">{header}</div>}
                <button
                  data-index={index}
                  onMouseEnter={() => setCursor(index)}
                  onClick={command.run}
                  className={cx(
                    "flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-[13px]",
                    index === cursor ? "bg-raised text-ink" : "text-ink-2",
                  )}
                >
                  {command.icon ?? <span className="w-4" />}
                  <span className="truncate">{command.label}</span>
                  {command.hint && (
                    <span className="ml-auto shrink-0 truncate text-2xs text-ink-4">
                      {command.hint}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>

        <div className="flex shrink-0 items-center gap-3 border-t border-line px-3 py-1.5 text-2xs text-ink-4">
          <span className="flex items-center gap-1">
            <Kbd>↑</Kbd>
            <Kbd>↓</Kbd> navigate
          </span>
          <span className="flex items-center gap-1">
            <Kbd>↵</Kbd> open
          </span>
          <span className="num ml-auto">{results.length}</span>
        </div>
      </div>
    </div>
  );
}
