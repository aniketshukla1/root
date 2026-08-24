// Brand marks for service kinds, auto-discovered.
//
// Same contract as `src/views/registry.ts`: adding an icon means adding one
// file to `./marks/`, never editing this one. That matters because the whole
// point of the descriptor model is that a service kind this build has never
// heard of still renders correctly — so an icon has to be an *enhancement* over
// the monogram, never a requirement.
//
// `iconFor` returning null is a normal, supported outcome, not a missing asset.

import type { ServiceKind } from "../types";

export interface BrandMark {
  /** Single path on a 24x24 grid, filled with currentColor. */
  path: string;
  title: string;
}

const modules = import.meta.glob<{ kind: string; mark: BrandMark }>(
  "./marks/*.ts",
  { eager: true },
);

const MARKS = new Map<string, BrandMark>();
for (const module of Object.values(modules)) {
  MARKS.set(module.kind, module.mark);
}

export function iconFor(kind: ServiceKind): BrandMark | null {
  return MARKS.get(kind) ?? null;
}

/** For tests and the settings picker: which kinds ship a mark. */
export function kindsWithIcons(): string[] {
  return [...MARKS.keys()].sort();
}
