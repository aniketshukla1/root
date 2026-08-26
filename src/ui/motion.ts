import type { Transition, Variants } from "motion/react";

/**
 * Motion vocabulary.
 *
 * One rule: motion explains where a value came from, it never decorates. A
 * line draws along its own time axis, a bar grows from its baseline, a series
 * staggers in reading order. Everything is intro-only — nothing loops, because
 * a console that pulses forever is a console you stop looking at.
 *
 * `motion` respects `prefers-reduced-motion` through `<MotionConfig reducedMotion="user">`,
 * set once in App. These variants therefore do not each re-check it.
 */

export const EASE_OUT: Transition = { duration: 0.55, ease: [0.22, 0.9, 0.3, 1] };
export const EASE_DRAW: Transition = { duration: 1.15, ease: [0.22, 0.8, 0.3, 1] };

/** A plotted line, drawn along its own time axis. */
export const drawLine: Variants = {
  hidden: { pathLength: 0, opacity: 0 },
  shown: { pathLength: 1, opacity: 1, transition: EASE_DRAW },
};

/** The fill under a line arrives after it, never with it. */
export const washIn: Variants = {
  hidden: { opacity: 0 },
  shown: { opacity: 1, transition: { duration: 0.7, delay: 0.25, ease: "easeOut" } },
};

/** A bar grows from its baseline. */
export const riseBar: Variants = {
  hidden: { scaleY: 0 },
  shown: { scaleY: 1, transition: EASE_OUT },
};

/** A measure reaches along its track from the left. */
export const reachBar: Variants = {
  hidden: { scaleX: 0 },
  shown: { scaleX: 1, transition: EASE_OUT },
};

/** A discrete mark — a pip, a dot, a cell. */
export const popIn: Variants = {
  hidden: { scale: 0, opacity: 0 },
  shown: { scale: 1, opacity: 1, transition: { duration: 0.34, ease: [0.3, 1.4, 0.5, 1] } },
};

/**
 * Reading order, not arrival order: a container staggers its children so the
 * eye is led left to right, or top to bottom, the way the data is meant to be
 * read. `delayChildren` gives the container's own frame time to settle first.
 */
export function stagger(each = 0.03, delay = 0.05): Variants {
  return {
    hidden: {},
    shown: { transition: { staggerChildren: each, delayChildren: delay } },
  };
}

/** Every plot mounts the same way, so no caller repeats these three props. */
export const enter = { initial: "hidden", animate: "shown" } as const;
