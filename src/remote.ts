// Talking to a root server instead of to a local backend.
//
// The desktop app reaches its engine over Tauri's IPC. A browser reaches the
// same engine over HTTP, and the point of this file is that nothing else in
// the frontend can tell which is happening. `api.ts` has one `invoke`
// function; `events.ts` has one subscription; both branch here and no screen,
// no hook and none of the sixty-four call sites knows a network exists.
//
// The server's RPC surface is shaped like `invoke(command, args)` rather than
// as REST for exactly this reason. Sixty-four REST routes would have read
// better in a URL bar and would have meant rewriting sixty-four call sites.
//
// ## Where the server address comes from
//
// Three places, most specific first:
//
//   1. `?server=` in the URL, which is how somebody tries an instance without
//      committing to it.
//   2. `localStorage`, which is how it stays chosen.
//   3. The page's own origin, when the server is serving this bundle — the
//      common case, and the one that needs no configuration at all.
//
// ## The token
//
// Held in `localStorage` and sent as a bearer header. That is the same
// exposure as any single-page app holding a session: an XSS on this origin
// reads it. The mitigation is that root serves its own bundle from its own
// origin with no third-party scripts, not that the token is hidden somewhere
// JavaScript cannot reach — there is no such place in a browser.

const SERVER_KEY = "root.server";
const TOKEN_KEY = "root.token";

/** Where the server is, or null when this build talks to a local backend. */
export function serverUrl(): string | null {
  // A URL parameter wins, and is remembered, so a link somebody was sent
  // survives the refresh they do thirty seconds later.
  const fromQuery = new URLSearchParams(window.location.search).get("server");
  if (fromQuery) {
    try {
      localStorage.setItem(SERVER_KEY, fromQuery);
    } catch {
      // Private browsing. The parameter still works for this page load.
    }
    return fromQuery.replace(/\/$/, "");
  }

  try {
    const stored = localStorage.getItem(SERVER_KEY);
    if (stored) return stored.replace(/\/$/, "");
  } catch {
    // No storage. Fall through to the origin.
  }

  // Served by the server itself: same origin, nothing to configure. Excludes
  // the Vite dev server, which serves the bundle but is not a root server.
  if (import.meta.env.PROD && window.location.protocol.startsWith("http")) {
    return window.location.origin;
  }
  return null;
}

/**
 * Take a session out of the URL fragment, if single sign-on just put one
 * there, and remove it from the address bar.
 *
 * A fragment rather than a query parameter because a fragment is never sent to
 * a server: it stays out of access logs and out of the `Referer` header on the
 * next request. It is still in the address bar until this runs, which is why
 * this runs before anything renders.
 */
export function claimTokenFromUrl(): boolean {
  const hash = window.location.hash;
  if (!hash.startsWith("#token=")) return false;

  setToken(decodeURIComponent(hash.slice("#token=".length)));
  window.history.replaceState(null, "", window.location.pathname + window.location.search);
  return true;
}

export function token(): string | null {
  try {
    return localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

export function setToken(value: string | null) {
  try {
    if (value) localStorage.setItem(TOKEN_KEY, value);
    else localStorage.removeItem(TOKEN_KEY);
  } catch {
    // Nothing to do: the caller will find out on the next request.
  }
}

export function setServer(value: string | null) {
  try {
    if (value) localStorage.setItem(SERVER_KEY, value.replace(/\/$/, ""));
    else localStorage.removeItem(SERVER_KEY);
  } catch {
    /* empty */
  }
}

/** Thrown when the server says the token is wrong, so the UI can ask again. */
export class Unauthorized extends Error {
  constructor() {
    super("This server did not accept the token.");
    this.name = "Unauthorized";
  }
}

/**
 * One command, over HTTP.
 *
 * The error path matters as much as the success one: the server returns the
 * command's own message, written for an operator, and throwing anything else
 * would replace a sentence somebody can act on with a status code.
 */
export async function call<T>(
  base: string,
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  const auth = token();
  const response = await fetch(`${base}/rpc/${command}`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(auth ? { authorization: `Bearer ${auth}` } : {}),
    },
    body: JSON.stringify(args ?? {}),
  });

  if (response.status === 401) throw new Unauthorized();

  const text = await response.text();
  const body = text ? JSON.parse(text) : null;

  if (!response.ok) {
    throw new Error(body?.error ?? `${command} failed with ${response.status}`);
  }
  return body as T;
}

/**
 * Subscribe to the server's event stream.
 *
 * Reconnects with a backoff, because a laptop that slept is the normal case
 * rather than the exceptional one and a monitoring screen that silently stops
 * updating is worse than one that says it is offline.
 *
 * The server also sends `monitor://lagged` when this client fell too far
 * behind to catch up. That is surfaced rather than swallowed: the honest
 * response is to refetch, and only the caller knows how.
 */
export function subscribe(
  base: string,
  onEvent: (event: string, payload: unknown) => void,
  onStatus?: (connected: boolean) => void,
): () => void {
  let socket: WebSocket | null = null;
  let closed = false;
  let attempt = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const open = () => {
    if (closed) return;
    const url = new URL(`${base}/events`);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";

    // A browser cannot set headers on a handshake, so the token rides as a
    // subprotocol — the standard workaround, and what the server also accepts.
    const auth = token();
    socket = auth
      ? new WebSocket(url.toString(), [`root.${auth}`])
      : new WebSocket(url.toString());

    socket.onopen = () => {
      attempt = 0;
      onStatus?.(true);
    };

    socket.onmessage = (message) => {
      try {
        const envelope = JSON.parse(message.data as string);
        onEvent(envelope.event, envelope.payload);
      } catch {
        // A frame that is not ours. Nothing useful to do with it.
      }
    };

    socket.onclose = () => {
      onStatus?.(false);
      if (closed) return;
      // Capped exponential backoff: fast enough that a blip is invisible,
      // slow enough that a server that is down is not being hammered by
      // every open tab.
      const delay = Math.min(30_000, 500 * 2 ** attempt++);
      timer = setTimeout(open, delay);
    };

    socket.onerror = () => socket?.close();
  };

  open();

  return () => {
    closed = true;
    if (timer) clearTimeout(timer);
    socket?.close();
  };
}
