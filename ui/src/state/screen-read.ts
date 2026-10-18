// A read a screen makes for itself (ADR-0173 decision 2): the settled bills, the paired devices,
// the guests waiting, the counter list, the box's activation and claim. None of them gates the till,
// so none is in the registry; each is retried with the engine's backoff while its screen is open,
// and its failure is shown on that screen with a Retry.
//
// The status bar's poll of `GET /api/sync` is not one of these: its timer is its retry, and the
// status bar is where it says what it found.

import { createSignal, onCleanup } from "solid-js";

import { ApiError } from "../api/client";
import { backoff, retryable } from "./engine";

export interface ScreenRead {
  // The last failure, until a read answers; `null` while none.
  readonly failure: () => unknown;
  // Reads now, and goes on retrying after a failure the edge may recover from.
  readonly run: () => Promise<void>;
  // Reads now, from the start of the backoff: the in-place Retry.
  readonly retry: () => Promise<void>;
}

export interface ScreenReadOptions {
  // A failure the screen answers itself, such as a refusal it shows as a hidden tile, or an older
  // edge's `404` it shows as an empty list. Neither shown as a failure nor retried.
  readonly handled?: (error: unknown) => boolean;
  // Called on each failure that is not handled, before any retry.
  readonly onFailure?: (error: unknown) => void;
}

// One retry at a time, after the engine's backoff, for a read that failed in a way the edge may
// recover from. Usable outside a component: the queue numbers' read keeps one.
export interface Retry {
  // Schedules `again` after the next backoff, where `error` may pass; returns whether it did.
  readonly after: (error: unknown, again: () => void) => boolean;
  // A read answered: the backoff starts over.
  readonly reset: () => void;
  // Nothing more is retried.
  readonly stop: () => void;
}

export function createRetry(): Retry {
  let failures = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  return {
    after: (error, again) => {
      clearTimeout(timer);
      if (!retryable(error)) {
        return false;
      }
      failures += 1;
      timer = setTimeout(again, backoff(failures));
      return true;
    },
    reset: () => {
      clearTimeout(timer);
      failures = 0;
    },
    stop: () => clearTimeout(timer),
  };
}

// Must be called while a component is being created: its retry stops when the screen is left.
export function createScreenRead<T>(
  read: () => Promise<T>,
  apply: (value: T) => void,
  options: ScreenReadOptions = {},
): ScreenRead {
  const [failure, setFailure] = createSignal<unknown>(null);
  const retry = createRetry();
  let open = true;
  onCleanup(() => {
    open = false;
    retry.stop();
  });
  const run = async (): Promise<void> => {
    try {
      const value = await read();
      if (open) {
        retry.reset();
        setFailure(null);
        apply(value);
      }
    } catch (error) {
      if (!open) {
        return;
      }
      if (options.handled?.(error) === true) {
        setFailure(null);
        return;
      }
      setFailure(() => error);
      options.onFailure?.(error);
      retry.after(error, () => void run());
    }
  };
  return {
    failure,
    run,
    retry: () => {
      retry.reset();
      return run();
    },
  };
}

// A failure the screen answers itself when it is a refusal with `status`: `then` is called, and the
// failure is neither shown nor retried. For an older edge's `404`, a `403`, a store without a cloud.
export function onStatus(status: number, then: () => void): (error: unknown) => boolean {
  return (error) => {
    const matched = error instanceof ApiError && error.status === status;
    if (matched) {
      then();
    }
    return matched;
  };
}
