/**
 * Row height, type scale, and padding all derive from one attribute on <html>,
 * so switching density is a single DOM write rather than a re-render of every
 * table in the app.
 */

export type Density = "compact" | "comfortable";

const KEY = "root-density";

export function readDensity(): Density {
  try {
    return localStorage.getItem(KEY) === "comfortable" ? "comfortable" : "compact";
  } catch {
    return "compact";
  }
}

export function applyDensity(density: Density): void {
  document.documentElement.dataset.density = density;
  try {
    localStorage.setItem(KEY, density);
  } catch {
    // A session with no storage still gets a working toggle, just not a sticky one.
  }
}
