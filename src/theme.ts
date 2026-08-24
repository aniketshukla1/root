/**
 * Theme works exactly like density: one attribute on <html>, so switching is a
 * single DOM write rather than a re-render of every panel in the app.
 *
 * "system" is a real third state, not a synonym for dark — it follows the OS
 * and keeps following it, so a machine that flips at sunset flips with it.
 */

export type Theme = "dark" | "light" | "system";

const KEY = "root-theme";
const media = () => window.matchMedia("(prefers-color-scheme: light)");

export function readTheme(): Theme {
  try {
    const stored = localStorage.getItem(KEY);
    return stored === "light" || stored === "dark" || stored === "system" ? stored : "dark";
  } catch {
    return "dark";
  }
}

/** What `theme` actually resolves to right now. */
export function resolveTheme(theme: Theme): "dark" | "light" {
  if (theme !== "system") return theme;
  try {
    return media().matches ? "light" : "dark";
  } catch {
    return "dark";
  }
}

export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = resolveTheme(theme);
  try {
    localStorage.setItem(KEY, theme);
  } catch {
    // A session with no storage still gets a working toggle, just not a sticky one.
  }
}

/**
 * Keeps "system" honest after the first paint. Returns an unsubscribe, and is
 * a no-op for the two explicit choices.
 */
export function watchSystemTheme(theme: Theme, onChange: () => void): () => void {
  if (theme !== "system") return () => {};
  const mq = media();
  const handler = () => {
    document.documentElement.dataset.theme = resolveTheme("system");
    onChange();
  };
  mq.addEventListener("change", handler);
  return () => mq.removeEventListener("change", handler);
}
