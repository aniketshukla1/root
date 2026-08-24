import { listen as tauriListen, type Event, type UnlistenFn } from "@tauri-apps/api/event";

import { isTauri } from "./fixtures";

/**
 * `listen` reaches for Tauri's IPC bridge the moment it is called, so outside
 * the webview it throws rather than simply never firing. Development runs in a
 * plain browser, where there is no event stream to subscribe to at all — so
 * this returns an unsubscribe that does nothing and lets the polling backstop
 * in App.tsx carry the UI.
 */
export function listen<T>(
  event: string,
  handler: (event: Event<T>) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return Promise.resolve(() => {});
  return tauriListen<T>(event, handler);
}
