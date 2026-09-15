// One screen's read, with the four things every screen was writing by hand (Wave 4 · PR-3, D5).
//
// # The Refresh button, and why it existed
//
// Thirty screens carried a Refresh button. Not one of them was there because an operator wanted it:
// each screen had hand-written its own `createSignal` pair for rows and error, fetched once on the
// context gate, and had no idea what to do after a save — so the button was the answer to "the list
// I am looking at is now wrong", asked thirty times. The owner's verdict was that the button should
// not exist, and the fix is not to hide it: it is that **a screen re-reads what it changed**.
//
// So this helper owns the four things a screen was re-inventing:
//
//   * the read, with `loading` / `ready` / `failed` kept apart — "nothing here yet" and "we could
//     not look" are different answers and a screen that renders both as an empty table is lying;
//   * `refetch()`, which a mutation calls instead of patching its local array (the patch is how a
//     list comes to disagree with the server about a row somebody else archived);
//   * revalidation on window focus, for the screens that watch something moving — a fleet, an
//     alert list, a rollout. Bounded by `staleAfterMs` so returning to a tab twice in a second
//     costs one request, not two;
//   * an interval, for the same screens, so a wall-mounted console stays true without anybody
//     touching it.
//
// A screen that does not watch anything moving passes neither, and gets a read that re-runs when
// its context changes and when it is asked to.
//
// # Why not a library
//
// Solid's own `createResource` covers the first item and nothing after it, and the console's need
// is the *after*: re-read on mutation, on focus, on a timer, without a screen holding three effects
// to do it. This is forty lines over `createSignal`; a query library would be a dependency, a cache
// to reason about, and a second answer to "what is on screen" beside the one the ETag rail already
// gives (ADR-0094).

import { createEffect, createSignal, onCleanup, type Accessor } from "solid-js";

import { apiMessage } from "./errors";
import { type Scope, onScopedContext } from "./scoped";

/** What the screen has: nothing yet, a value, or a refusal that says why. */
export type ResourceState<T> =
  | { readonly state: "loading" }
  | { readonly state: "ready"; readonly value: T }
  | { readonly state: "failed"; readonly message: string };

export type AdminResource<T> = {
  /** The current state. A screen renders a skeleton, the value, or the message. */
  readonly state: Accessor<ResourceState<T>>;
  /** The value when there is one, `null` while loading or after a refusal. */
  readonly value: Accessor<T | null>;
  /** True while a read is in flight, including a re-read over a value already on screen. */
  readonly loading: Accessor<boolean>;
  /**
   * Re-read now.
   *
   * What a mutation calls when it succeeds. Returns the promise so a caller that wants to wait —
   * a form that closes only once the list behind it is true — can.
   */
  readonly refetch: () => Promise<void>;
};

export type ResourceOptions = {
  /**
   * The context the read needs, so it runs when that context is there and again when it changes.
   *
   * `"tenant"` for a read scoped to the organisation, `"store"` for one scoped to a shop. Omit for
   * a read that needs neither — the console's own admins, the country registry.
   */
  readonly scope?: Scope;
  /** Re-read when the tab regains focus, if the last read is older than `staleAfterMs`. */
  readonly revalidateOnFocus?: boolean;
  /** Re-read on this interval, in milliseconds. For a screen watching something that moves. */
  readonly intervalMs?: number;
  /** How old a read must be before a focus revalidates it. Default 30 s. */
  readonly staleAfterMs?: number;
};

const DEFAULT_STALE_AFTER_MS = 30_000;

/**
 * The reason a read was refused, or `""` when there is none — the shape a `<Banner>` wants.
 *
 * A screen cannot narrow `state()` inline: `<Show when={r.state().state === "failed" && r.state()}>`
 * hands the child the union, not the `failed` arm, so every call site would repeat the same
 * re-check to get at `.message`. One function does the narrowing once, and `""` is falsy, so
 * `<Show when={failureOf(r)}>` renders the banner only when there is something to say.
 */
export function failureOf<T>(resource: AdminResource<T>): string {
  const current = resource.state();
  return current.state === "failed" ? current.message : "";
}

/**
 * A screen's read.
 *
 * `read` is called with the context the scope promised — a tenant-scoped read is never called with
 * an empty tenant id, which is the whole point of the gate (F0) and is what this preserves.
 */
export function createAdminResource<T>(
  read: (tenantId: string, storeId: string) => Promise<T>,
  options: ResourceOptions = {},
): AdminResource<T> {
  const [state, setState] = createSignal<ResourceState<T>>({ state: "loading" });
  const [loading, setLoading] = createSignal(false);
  // The context the last read ran with, so `refetch` can repeat it without the caller passing it
  // back. `null` until the first read: refetching before one is a no-op rather than a read with an
  // empty tenant id, which is the refusal the context gate exists to prevent.
  let context: { tenantId: string; storeId: string } | null = null;
  let lastReadAt = 0;
  // Only the newest read may write the state. Without this, a slow first read landing after a fast
  // second one puts the previous store's rows on screen — the defect that makes switching stores
  // twice quickly show the wrong shop's data.
  let generation = 0;

  const run = async (tenantId: string, storeId: string): Promise<void> => {
    context = { tenantId, storeId };
    generation += 1;
    const mine = generation;
    setLoading(true);
    try {
      const value = await read(tenantId, storeId);
      if (mine === generation) {
        setState({ state: "ready", value });
        lastReadAt = Date.now();
      }
    } catch (caught) {
      if (mine === generation) {
        setState({ state: "failed", message: apiMessage(caught) });
        lastReadAt = Date.now();
      }
    } finally {
      if (mine === generation) {
        setLoading(false);
      }
    }
  };

  const refetch = async (): Promise<void> => {
    if (context === null) {
      return;
    }
    await run(context.tenantId, context.storeId);
  };

  if (options.scope) {
    onScopedContext(options.scope, (tenantId, storeId) => void run(tenantId, storeId));
  } else {
    createEffect(() => {
      if (context === null) {
        void run("", "");
      }
    });
  }

  if (options.revalidateOnFocus) {
    const stale = options.staleAfterMs ?? DEFAULT_STALE_AFTER_MS;
    const onFocus = () => {
      if (Date.now() - lastReadAt >= stale) {
        void refetch();
      }
    };
    window.addEventListener("focus", onFocus);
    onCleanup(() => window.removeEventListener("focus", onFocus));
  }

  if (options.intervalMs !== undefined && options.intervalMs > 0) {
    const handle = setInterval(() => void refetch(), options.intervalMs);
    onCleanup(() => clearInterval(handle));
  }

  return {
    state,
    value: () => {
      const current = state();
      return current.state === "ready" ? current.value : null;
    },
    loading,
    refetch,
  };
}
