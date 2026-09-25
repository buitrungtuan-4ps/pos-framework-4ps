import { Show, createSignal, onMount } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { ApiError, api } from "../api/client";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { loadStore } from "../state/store";
import { errorMessage } from "../lib/errors";

// Staff sign-in on a paired device (S0b, ADR-0084). A paired device commands nothing until a real
// employee signs in with their badge code and PIN; the edge verifies the PIN offline against the
// synced roster and binds the device to that person, so every sale is attributable. A wrong code and
// a wrong PIN get the same answer, so a guess learns nothing; repeated wrong PINs lock the account
// (ADR-0030). Once signed in, the device goes to the floor.
export function SignIn() {
  const navigate = useNavigate();
  // The PIN field, so a refusal can hand the cursor back to it.
  let pinField: HTMLInputElement | undefined;
  const [code, setCode] = createSignal("");
  const [pin, setPin] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  // Nobody on this store has a PIN yet, so every code would be refused as a wrong one.
  const [unstaffed, setUnstaffed] = createSignal(false);

  onMount(() => {
    // A device already signed in has no business here; a device not paired must pair first. Both are
    // resolved by asking the edge who is signed in, so a manual visit or a reload lands correctly.
    void api
      .session()
      .then((session) => {
        if (session.signed_in) {
          navigate("/", { replace: true });
          return;
        }
        // Only an explicit `false`: an edge too old to say is not a store with no staff.
        setUnstaffed(session.sign_in_ready === false);
      })
      .catch((caught) => {
        if (caught instanceof ApiError && caught.isUnauthorized) {
          navigate("/pair", { replace: true });
        }
      });
  });

  const submit = async () => {
    setError(null);
    setBusy(true);
    // The PIN this attempt is for. Held because the field stays live while the request is out, and
    // what the operator types next must not be thrown away by this attempt's refusal — see the
    // clear below.
    const attempted = pin();
    try {
      const result = await api.signIn(code().trim(), attempted);
      if (result.ok) {
        // The device can sell now, so read what it sells: the floor, the price book, the button plan
        // and the money settings. `App`'s boot gate loads the same set, but it runs once on page
        // load and this navigation is client-side — without this a freshly signed-in till drew the
        // fallback floor and an empty menu until somebody reloaded it.
        await loadStore();
        navigate("/", { replace: true });
        return;
      }
      if (result.outcome === "locked_out") {
        setError(t("signin.locked"));
      } else if (typeof result.remaining === "number") {
        setError(t("signin.wrong_remaining", { count: result.remaining }));
      } else {
        setError(t("signin.wrong"));
      }
      // Clear the field for the retype — but only if it still holds the PIN that was refused.
      //
      // A sign-in takes a moment on a busy store server, and the button is disabled while it does
      // while the field is not. An operator who has already started retyping had those keystrokes
      // wiped when the refusal landed: digits vanished mid-typing, so they typed again, and each
      // confused attempt counts toward the lockout (ADR-0030) — a badge locked in the middle of
      // service by the screen rather than by the person.
      if (pin() === attempted) {
        setPin("");
      }
      // And put the cursor back where the next attempt is typed. A refusal that costs a tap to
      // recover from is a refusal that costs a tap every time, and this screen is where a shift
      // starts.
      pinField?.focus();
    } catch (caught) {
      if (caught instanceof ApiError && caught.isUnauthorized) {
        navigate("/pair", { replace: true });
        return;
      }
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="mx-auto max-w-sm p-4">
      <PageHeader title={t("signin.title")} />
      <p class="text-ink-muted">{t("signin.hint")}</p>
      <Show when={unstaffed()}>
        <p id="signin-no-staff" class="mt-3 rounded-token border border-awaiting px-3 py-2 text-ink" role="status">
          {t("signin.no_staff")}
        </p>
      </Show>

      <label class="mt-3 block text-ink-muted" for="signin-code">
        {t("signin.code")}
      </label>
      <input
        id="signin-code"
        autocomplete="username"
        class="mt-1 w-full rounded-token border border-line bg-surface p-3 text-lg"
        value={code()}
        onInput={(event) => setCode(event.currentTarget.value)}
      />

      <label class="mt-3 block text-ink-muted" for="signin-pin">
        {t("signin.pin")}
      </label>
      <input
        id="signin-pin"
        ref={pinField}
        type="password"
        inputmode="numeric"
        autocomplete="current-password"
        class="mt-1 w-full rounded-token border border-line bg-surface p-3 text-center text-xl tracking-[0.4em] tabular-nums"
        value={pin()}
        onInput={(event) => setPin(event.currentTarget.value.replace(/\D/g, ""))}
      />

      <Show when={error()}>
        {(message) => (
          <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <button
        type="button"
        class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
        disabled={busy() || code().trim().length === 0 || pin().length === 0}
        data-step="submit"
        onClick={() => void submit()}
      >
        {t("signin.submit")}
      </button>
    </section>
  );
}
