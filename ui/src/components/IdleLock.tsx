import { Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";
import { useLocation } from "@solidjs/router";

import { ApiError, api } from "../api/client";
import { CodePad } from "./CodePad";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import {
  employeeId,
  idleLockSeconds,
  loadSession,
  lockTill,
  locked,
  rememberedCode,
  signedIn,
  signedInAs,
} from "../state/session";

// The till's idle lock (ADR-0160 decision 2, `docs/pos-spec.md` §16 "device screen lock after N idle
// minutes, unlocked by PIN"). After `session.idle_lock_seconds` with no touch, key, click or scroll,
// the till signs its person out on the store server and covers the screen until a PIN opens it.
//
// # Why the screen stays underneath
//
// The lock covers the screen rather than leaving it. A pay screen holds a split's taken shares until
// the last one lands (`Pay.tsx`), and cash already handed over must still be on screen when the
// person comes back. So the screen keeps its state under the lock, inert, and the person carries on
// where they were. The sign-in itself is gone: the edge refuses every command from this device until
// a PIN signs it in again, so the covered screen cannot be used by whoever finds the till.
//
// # Where it never locks
//
// The kitchen board and the pass are read from across a kitchen, with nobody's hands on them, and a
// board that locked mid-service would hide the food. The screens before anyone signs in have nobody
// to lock out. A store that publishes `0`, the default, never locks.

/** The screens nobody stands at, or where nobody is signed in yet. */
const NEVER_LOCKS: readonly string[] = ["/kds", "/expo", "/pair", "/signin", "/setup", "/claim"];

/** Whether the idle lock applies on `pathname`. */
export function locksHere(pathname: string): boolean {
  return !NEVER_LOCKS.some((path) => pathname === path || pathname.startsWith(`${path}/`));
}

/** What says a person is at the till. A scroll counts: someone reading a long order is using it. */
const ACTIVITY = ["pointerdown", "keydown", "touchstart", "wheel"] as const;

export function IdleLock() {
  const location = useLocation();
  // The lock's own form, as the sign-in screen's: the code (when the till does not know it), the
  // PIN, and which of the two the on-screen pad types into.
  const [code, setCode] = createSignal("");
  const [pin, setPin] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [filling, setFilling] = createSignal<"code" | "pin">("pin");
  // The PIN field, so a refusal can hand the cursor back to it.
  let pinField: HTMLInputElement | undefined;
  // Who was signed in when the till locked, so the PIN that opens it can be told from someone
  // else's.
  let lockedFor: string | null = null;

  // When a person last touched the till, by the page's own clock.
  let lastActive = Date.now();
  let timer: ReturnType<typeof setTimeout> | undefined;

  const armed = () =>
    !locked() && signedIn() && idleLockSeconds() > 0 && locksHere(location.pathname);

  // One timer, set for when the till would next have sat idle long enough. A touch only moves
  // `lastActive`; the timer then finds it too early and sets itself again for what is left, so a
  // busy till is not clearing and setting a timer on every tap.
  const schedule = () => {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
    if (!armed()) {
      return;
    }
    const span = idleLockSeconds() * 1000;
    timer = setTimeout(
      () => {
        timer = undefined;
        if (!armed()) {
          return;
        }
        if (Date.now() - lastActive >= span) {
          void lock();
        } else {
          schedule();
        }
      },
      Math.max(span - (Date.now() - lastActive), 0),
    );
  };

  const touched = () => {
    lastActive = Date.now();
    if (timer === undefined) {
      schedule();
    }
  };

  // Signed in, a new window, another screen, the lock lifted: the count starts again from now.
  createEffect(
    on([signedIn, idleLockSeconds, locked, () => location.pathname], () => {
      lastActive = Date.now();
      schedule();
    }),
  );

  onMount(() => {
    for (const name of ACTIVITY) {
      window.addEventListener(name, touched, { capture: true, passive: true });
    }
  });
  onCleanup(() => {
    for (const name of ACTIVITY) {
      window.removeEventListener(name, touched, { capture: true });
    }
    if (timer !== undefined) {
      clearTimeout(timer);
    }
  });

  const lock = async () => {
    lockedFor = employeeId();
    lockTill();
    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur();
    }
    setCode(rememberedCode() ?? "");
    setFilling(rememberedCode() === null ? "code" : "pin");
    // Signed out on the store server, so nothing can be done from this device without a PIN. A
    // sign-out that does not arrive leaves the screen locked all the same, and the edge's own idle
    // window then signs the device out.
    await api.signOut().catch(() => undefined);
  };

  // Whether the person's code is known, so the lock asks for their PIN alone.
  const knowsCode = () => rememberedCode() !== null;

  const unlock = async () => {
    setError(null);
    setBusy(true);
    const attempted = pin();
    const typed = code().trim();
    try {
      const result = await api.signIn(typed, attempted);
      if (result.ok) {
        // Someone else signed in: their till starts afresh, as after a sign-out, rather than on the
        // screen of the person before them.
        if (lockedFor !== null && result.employeeId !== lockedFor) {
          window.location.replace("/");
          return;
        }
        // The same person: read the session again before the lock lifts, so what they may do is
        // the store's current answer and not what the lock forgot (ADR-0158 decision 6).
        await loadSession();
        signedInAs(typed, result.employeeId);
        setPin("");
        return;
      }
      if (result.outcome === "locked_out") {
        setError(t("signin.locked"));
      } else if (typeof result.remaining === "number") {
        setError(t("signin.wrong_remaining", { count: result.remaining }));
      } else {
        setError(t("signin.wrong"));
      }
      if (pin() === attempted) {
        setPin("");
      }
      pinField?.focus();
    } catch (caught) {
      if (caught instanceof ApiError && caught.isUnauthorized) {
        window.location.replace("/pair");
        return;
      }
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const padValue = () => (filling() === "code" ? code() : pin());
  const padChange = (next: string) => {
    if (filling() === "code") {
      setCode(next);
    } else {
      setPin(next.replace(/\D/g, ""));
    }
  };

  return (
    <Show when={locked()}>
      <div
        class="fixed inset-0 z-50 overflow-y-auto bg-canvas"
        role="dialog"
        aria-modal="true"
        aria-labelledby="lock-title"
        data-outcome="locked"
      >
        <section class="mx-auto max-w-sm p-4 terminal:grid terminal:max-w-4xl terminal:grid-cols-2 terminal:items-start terminal:gap-8">
          <div>
            <h1 id="lock-title" class="mb-4 text-lg font-semibold">
              {t("lock.title")}
            </h1>
            <p class="text-ink-muted">{knowsCode() ? t("lock.hint") : t("lock.hint_code")}</p>

            <Show when={!knowsCode()}>
              <label class="mt-3 block text-ink-muted" for="lock-code">
                {t("signin.code")}
              </label>
              <input
                id="lock-code"
                autocomplete="username"
                class="mt-1 w-full rounded-token border border-line bg-surface p-3 text-lg"
                value={code()}
                onFocus={() => setFilling("code")}
                onInput={(event) => setCode(event.currentTarget.value)}
              />
            </Show>

            <label class="mt-3 block text-ink-muted" for="lock-pin">
              {t("signin.pin")}
            </label>
            <input
              id="lock-pin"
              ref={pinField}
              type="password"
              inputmode="numeric"
              autocomplete="current-password"
              class="mt-1 w-full rounded-token border border-line bg-surface p-3 text-center text-xl tracking-[0.4em] tabular-nums"
              value={pin()}
              onFocus={() => setFilling("pin")}
              onInput={(event) => setPin(event.currentTarget.value.replace(/\D/g, ""))}
            />

            <Show when={error()}>
              {(message) => (
                <p
                  class="mt-3 rounded-token border border-danger px-3 py-2 text-danger"
                  role="alert"
                >
                  {message()}
                </p>
              )}
            </Show>

            <button
              id="lock-submit"
              type="button"
              class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent disabled:opacity-50"
              disabled={busy() || code().trim().length === 0 || pin().length === 0}
              onClick={() => void unlock()}
            >
              {t("lock.unlock")}
            </button>
            <button
              id="lock-someone-else"
              type="button"
              class="mt-3 min-h-touch w-full rounded-token border border-line px-3 text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
              onClick={() => window.location.replace("/signin")}
            >
              {t("lock.someone_else")}
            </button>
          </div>

          <CodePad
            id="lock-pad"
            value={padValue()}
            onChange={padChange}
            mode={filling() === "code" ? "text" : "digits"}
            label={filling() === "code" ? t("signin.pad_code") : t("signin.pad_pin")}
          />
        </section>
      </div>
    </Show>
  );
}
