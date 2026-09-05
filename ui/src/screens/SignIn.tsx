import { Show, createSignal, onMount } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { ApiError, api } from "../api/client";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";

// Staff sign-in on a paired device (S0b, ADR-0084). A paired device commands nothing until a real
// employee signs in with their badge code and PIN; the edge verifies the PIN offline against the
// synced roster and binds the device to that person, so every sale is attributable. A wrong code and
// a wrong PIN get the same answer, so a guess learns nothing; repeated wrong PINs lock the account
// (ADR-0030). Once signed in, the device goes to the floor.
export function SignIn() {
  const navigate = useNavigate();
  const [code, setCode] = createSignal("");
  const [pin, setPin] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  onMount(() => {
    // A device already signed in has no business here; a device not paired must pair first. Both are
    // resolved by asking the edge who is signed in, so a manual visit or a reload lands correctly.
    void api
      .session()
      .then((session) => {
        if (session.signed_in) {
          navigate("/", { replace: true });
        }
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
    try {
      const result = await api.signIn(code().trim(), pin());
      if (result.ok) {
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
      setPin("");
    } catch (caught) {
      if (caught instanceof ApiError && caught.isUnauthorized) {
        navigate("/pair", { replace: true });
        return;
      }
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="mx-auto max-w-sm p-4">
      <PageHeader title={t("signin.title")} />
      <p class="text-ink-muted">{t("signin.hint")}</p>

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
        class="mt-4 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink disabled:opacity-50"
        disabled={busy() || code().trim().length === 0 || pin().length === 0}
        data-step="submit"
        onClick={() => void submit()}
      >
        {t("signin.submit")}
      </button>
    </section>
  );
}
