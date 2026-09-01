import { Show, createSignal } from "solid-js";
import { useNavigate, useSearchParams } from "@solidjs/router";

import { ApiError, api } from "../api/client";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";

// Pairing a device: the operator reads a six-digit code off the edge and enters it here (or opens
// the QR link, which lands here with the code pre-filled). Redeeming is single-use; an unknown or
// expired code gets the same answer, so a wrong guess learns nothing (ADR-0030). The issued token is
// persisted by the API client (`api.pair`) and carried on every later domain call, which the edge now
// requires (ADR-0084); once paired, the device goes to sign-in, where a member of staff signs in
// before the floor accepts a command (S0b).
export function Pairing() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const initial = typeof params["code"] === "string" ? params["code"] : "";
  const [code, setCode] = createSignal(initial);
  const [error, setError] = createSignal<string | null>(null);
  const [paired, setPaired] = createSignal(false);

  const submit = async () => {
    setError(null);
    try {
      await api.pair(code().trim());
      setPaired(true);
      // The device now holds a token, but a command needs a signed-in employee (S0b/ADR-0084); land
      // on sign-in, not the floor.
      navigate("/signin", { replace: true });
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    }
  };

  return (
    <section class="mx-auto max-w-sm p-4">
      <PageHeader title={t("pair.title")} />

      <Show
        when={!paired()}
        fallback={
          <div class="rounded-token border border-line bg-surface p-4">
            <p class="font-semibold text-ok">{t("pair.paired")}</p>
            <p class="mt-1 text-ink-muted">{t("pair.paired_hint")}</p>
          </div>
        }
      >
        <p class="text-ink-muted">{t("pair.hint")}</p>
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
          {t("pair.submit")}
        </button>
      </Show>
    </section>
  );
}
