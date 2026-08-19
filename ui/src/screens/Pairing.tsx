import { Show, createSignal } from "solid-js";
import { useSearchParams } from "@solidjs/router";

import { ApiError, api } from "../api/client";

// Pairing a device: the operator reads a six-digit code off the edge and enters it here (or opens
// the QR link, which lands here with the code pre-filled). Redeeming is single-use; an unknown or
// expired code gets the same answer, so a wrong guess learns nothing (ADR-0030). The token is kept
// only in memory for now — persisting it in the device keystore is the auth-integration follow-up.
export function Pairing() {
  const [params] = useSearchParams();
  const initial = typeof params["code"] === "string" ? params["code"] : "";
  const [code, setCode] = createSignal(initial);
  const [error, setError] = createSignal<string | null>(null);
  const [paired, setPaired] = createSignal(false);

  const submit = async () => {
    setError(null);
    try {
      await api.pair(code().trim());
      setPaired(true);
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : "The store did not respond.");
    }
  };

  return (
    <section class="mx-auto max-w-sm p-4">
      <h1 class="mb-4 text-lg font-semibold">Pair this device</h1>

      <Show
        when={!paired()}
        fallback={
          <div class="rounded-token border border-line bg-surface p-4">
            <p class="font-semibold text-ok">Paired</p>
            <p class="mt-1 text-ink-muted">This device is now trusted by the store.</p>
          </div>
        }
      >
        <p class="text-ink-muted">Enter the six-digit code shown on the store server.</p>
        <input
          inputmode="numeric"
          maxLength={6}
          autocomplete="one-time-code"
          class="mt-3 w-full rounded-token border border-line bg-surface p-3 text-center text-xl tracking-[0.4em] tabular-nums"
          value={code()}
          onInput={(event) => setCode(event.currentTarget.value.replace(/\D/g, ""))}
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
          class="mt-3 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink disabled:opacity-50"
          disabled={code().length !== 6}
          onClick={() => void submit()}
        >
          Pair
        </button>
      </Show>
    </section>
  );
}
