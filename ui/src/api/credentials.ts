// Where this device keeps the two facts that make it a paired till: which edge it paired with, and
// the token that edge issued it (ADR-0111, ADR-0084).
//
// # Why a seam rather than four calls to `localStorage`
//
// `localStorage` is what a *browser* has, and it stays the store for the in-store till: the existing
// try/catch wrappers already handle a private window or blocked storage, and nothing about a shop
// changes. A native shell has somewhere better — the OS credential store, which the framework
// already reaches through `pos_ports::key_vault::KeyVault` and its `key-vault-keyring` adapter
// (ADR-0086) — and a shell that holds a device credential is exactly the case that port's own
// documentation describes. Writing a second credential store beside it would be inventing a parallel
// answer to a question that has one.
//
// So this is a three-method seam with the browser implementation installed by default. A shell calls
// `installCredentials` once at boot and every caller below is unchanged.
//
// **No shell has been spiked.** ADR-0111 says so plainly, and this file does not pretend otherwise:
// what survives the spike failing is the seam.
//
// # The base and the token are one record
//
// They are written together when the device pairs and read together afterwards, because a token is
// only meaningful against the edge that issued it: a token carried to a different base is a `401`
// waiting to happen, and a base with no token is a device that has not paired.
//
// One asymmetry, and it is deliberate: a stale token is cleared and **the base is not**. A device
// whose token expired must re-pair to *the same edge*; an app that forgot the address would send the
// operator looking for a QR code that may be in another building, or on a screen nobody can reach.

// What a device keeps. Two keys rather than one blob, so a till that paired before this existed
// keeps its token exactly where it left it — the read below is the same read it always was.
const TOKEN_KEY = "pos-edge.device-token";
const BASE_KEY = "pos-edge.base-url";

// The store this device uses. Every method is total: an implementation that cannot reach its
// storage answers `null` and swallows a write, because a device that cannot persist its token
// re-pairs each session, which is degraded rather than broken.
export interface CredentialStore {
  read(key: string): string | null;
  write(key: string, value: string): void;
  clear(key: string): void;
}

// The browser's own storage — today's code, moved. Every call is wrapped because a private window,
// blocked site data, or a browser configured to refuse storage throws rather than returning null.
const browserCredentials: CredentialStore = {
  read(key) {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
  write(key, value) {
    try {
      localStorage.setItem(key, value);
    } catch {
      // A device that cannot persist re-pairs each session; that is degraded, not broken.
    }
  },
  clear(key) {
    try {
      localStorage.removeItem(key);
    } catch {
      // Nothing to do — the next domain call will 401 and route the operator to pair anyway.
    }
  },
};

let store: CredentialStore = browserCredentials;

// Swap the store, once, at boot. A native shell calls this with an implementation that reaches the
// OS credential store; nothing else in the app changes.
export function installCredentials(next: CredentialStore): void {
  store = next;
}

// The bearer token this device was issued when it paired (ADR-0084), or `null` if it has not.
export function deviceToken(): string | null {
  return store.read(TOKEN_KEY);
}

// The origin this device's edge is reached at, or `""` for the same origin that served the app.
//
// **The default is the empty string, not `window.location.origin`.** That matters more than it
// looks: `fetch("" + "/api/floor")` sends the identical bytes an in-store till sends today, while
// an absolute URL would be a different request string with a different set of things that can go
// wrong with it. The in-store path is not "equivalent" after this change; it is unchanged.
export function edgeBase(): string {
  return store.read(BASE_KEY) ?? "";
}

// Records both halves of a successful pairing. `base` is empty for an in-store till, which is what
// keeps its requests root-relative.
export function rememberPairing(base: string, token: string): void {
  store.write(TOKEN_KEY, token);
  if (base === "") {
    store.clear(BASE_KEY);
  } else {
    store.write(BASE_KEY, base);
  }
}

// Drops the token and keeps the base — the asymmetry this module's header explains. Called on a
// `401`, which means this device must pair again with the edge it already knows.
export function clearDeviceToken(): void {
  store.clear(TOKEN_KEY);
}

// The `/ws` URL for the base this device paired against.
//
// Base empty — today's expression, from `window.location`, unchanged. Base set — resolve `/ws`
// against it and swap the scheme, because `https:` carries `wss:` and `http:` carries `ws:` and a
// socket opened on the wrong one is refused rather than downgraded.
export function liveSocketUrl(): string {
  const base = edgeBase();
  if (base === "") {
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    return `${scheme}://${window.location.host}/ws`;
  }
  const resolved = new URL("/ws", base);
  resolved.protocol = resolved.protocol === "https:" ? "wss:" : "ws:";
  return resolved.toString();
}
