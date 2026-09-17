import { useEffect } from "react";

import { healthColor, healthLabel, type Catalog } from "../catalog";
import { formatDateTime, formatRelative } from "../format";
import type { Entity, Issue } from "../model";
import { SEVERITY } from "../ui/AlertList";
import { AssistantIcon, ChevronRight, CloseIcon } from "../ui/icons";
import { Button, Dot, Panel, ServiceMark, Tag } from "../ui/primitives";

/**
 * What is wrong with one component, and nothing else.
 *
 * This only ever opens for something that is actually failing — a healthy cell
 * navigates straight to its detail page instead, because opening a dialog to
 * say "this is fine" wastes the click that was meant to be fast.
 *
 * So the first line answers the question that made someone click: what is
 * wrong. Everything after it is the evidence and the two things worth doing.
 */
export function ComponentDialog({
  entity,
  issue,
  catalog,
  onClose,
  onOpenComponent,
  onExplain,
  onAcknowledge,
}: {
  entity: Entity;
  issue?: Issue;
  catalog: Catalog;
  onClose: () => void;
  onOpenComponent: () => void;
  onExplain: () => void;
  onAcknowledge?: (alertId: string) => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const alert = issue?.alert;
  const severity = alert ? SEVERITY[alert.severity] : null;

  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-page/70 p-6"
      onClick={onClose}
      role="presentation"
    >
      <div
        className="panel w-full max-w-lg overflow-hidden overlay-plate"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={`${entity.name} — ${healthLabel(entity.health)}`}
      >
        <header className="flex items-center gap-2.5 border-b border-line px-4 py-3">
          <ServiceMark descriptor={catalog.get(entity.kind)} size={20} />
          <div className="min-w-0 flex-1">
            <div className="num truncate text-base text-ink">{entity.name}</div>
            <div className="num mt-0.5 truncate text-2xs text-ink-4">
              {entity.profileName} · {entity.section}
            </div>
          </div>
          <span className="flex shrink-0 items-center gap-1.5">
            <Dot health={entity.health} />
            <span
              className="font-mono text-2xs tracking-[.05em]"
              style={{ color: healthColor(entity.health) }}
            >
              {healthLabel(entity.health)}
            </span>
          </span>
          <Button variant="ghost" className="px-1.5" onClick={onClose} title="Close  Esc">
            <CloseIcon size={14} />
          </Button>
        </header>

        <div className="flex flex-col gap-3.5 px-4 py-4">
          <div>
            <div className="label mb-1.5">What is wrong</div>
            <p className="num text-xs leading-relaxed text-ink-2">
              {alert?.title ?? entity.summary ?? "The collector reported this component as failing."}
            </p>
          </div>

          {alert && (
            <>
              {alert.detail && (
                <p className="num text-2xs leading-relaxed text-ink-3">{alert.detail}</p>
              )}
              <div className="flex flex-wrap items-center gap-2">
                {severity && (
                  <Tag color={severity.color}>{severity.label.toUpperCase()}</Tag>
                )}
                <span className="num text-2xs text-ink-4" title={formatDateTime(alert.firstSeen)}>
                  firing {formatRelative(alert.firstSeen)}
                </span>
                {alert.acknowledged && <Tag>acknowledged</Tag>}
              </div>
            </>
          )}

          {!alert && (
            // A failing row with no alert behind it is a real state, not a gap:
            // the collector set health directly without a threshold to cross.
            <p className="num text-2xs leading-relaxed text-ink-4">
              No alert is attached — the collector reported this health directly rather than
              crossing a threshold.
            </p>
          )}

          {entity.ownership.assigned && (
            <div className="flex items-center gap-2 border-t border-line pt-3">
              <span className="label">Owner</span>
              <span className="num text-2xs text-ink-2">{entity.ownership.team}</span>
              {entity.ownership.contact && (
                <span className="num text-2xs text-ink-4">{entity.ownership.contact}</span>
              )}
            </div>
          )}
        </div>

        <footer className="flex items-center gap-2 border-t border-line px-4 py-3">
          <Button onClick={onExplain} className="gap-1.5 text-accent">
            <AssistantIcon size={13} />
            Explain
          </Button>
          {alert && !alert.acknowledged && onAcknowledge && (
            <Button onClick={() => onAcknowledge(alert.id)}>Acknowledge</Button>
          )}
          <Button variant="primary" className="ml-auto gap-1.5" onClick={onOpenComponent}>
            Open component
            <ChevronRight size={12} />
          </Button>
        </footer>
      </div>
    </div>
  );
}

export { Panel };
