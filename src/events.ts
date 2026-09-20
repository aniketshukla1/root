import { listen as tauriListen, type Event, type UnlistenFn } from "@tauri-apps/api/event";

import { isTauri } from "./fixtures";
import * as remote from "./remote";

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
  // Against a server, one WebSocket carries every channel, so each caller
  // gets a filter over the shared stream rather than a socket of its own.
  const server = remote.serverUrl();
  if (server) return Promise.resolve(stream(server).on(event, handler as Handler));

  if (!isTauri()) return Promise.resolve(() => {});
  return tauriListen<T>(event, handler);
}

type Handler = (event: { payload: unknown }) => void;

/**
 * The shared connection to one server.
 *
 * A socket per subscriber would open five for the same five channels and
 * reconnect five times after a laptop wakes. One connection, opened on the
 * first subscription and closed with the last, keeps the reconnect logic in
 * `remote.ts` answering to a single client.
 */
function stream(base: string) {
  if (!shared || shared.base !== base) {
    shared?.close();
    const handlers = new Map<string, Set<Handler>>();
    const close = remote.subscribe(
      base,
      (event, payload) => {
        for (const handler of handlers.get(event) ?? []) handler({ payload });
      },
      (connected) => {
        // Surfaced as an ordinary event so a screen can show it without
        // knowing which transport it is on.
        for (const handler of handlers.get(CONNECTION) ?? []) {
          handler({ payload: connected });
        }
      },
    );
    shared = {
      base,
      close,
      on(event: string, handler: Handler) {
        const set = handlers.get(event) ?? new Set();
        set.add(handler);
        handlers.set(event, set);
        return () => {
          set.delete(handler);
          if ([...handlers.values()].every((s) => s.size === 0)) {
            shared?.close();
            shared = null;
          }
        };
      },
    };
  }
  return shared;
}

let shared: {
  base: string;
  close: () => void;
  on: (event: string, handler: Handler) => UnlistenFn;
} | null = null;

/** Whether the event stream is up. Only ever fires against a server. */
export const CONNECTION = "root://connection";
