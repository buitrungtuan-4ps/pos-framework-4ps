import { Match, Switch, createSignal, onMount } from "solid-js";
import { useNavigate, useSearchParams } from "@solidjs/router";

import { ApiError, api, deviceToken } from "../api/client";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";

// Activating the store server: the operator reads the `XXXX-XXXX-XXXX` code off the box's setup sheet
// and enters it here. The edge exchanges it with the cloud once, keeps the device credential it gets
// back in the operating system's keyring, and is then permanently activated (ADR-0050, ADR-0086); the
// code is spent, so a replacement box is a five-minute job rather than a credential hand-off
// (ADR-0003). This is the *store's* cloud identity, not this device's token — a browser still pairs
// afterwards (ADR-0084) — and none of it gates trading: an unactivated box still takes orders on the
// LAN (ADR-0001), it simply does not sync.
//
// A store server with no `cloud_url` never mounts these routes, so the standing check fails outright;
// that is a legitimate configuration, and the screen says so instead of showing a form that cannot
// work.

/// The number of symbols in an activation code, checksum symbol included (`pos_core::activation`).
const CODE_LENGTH = 12;

// Codes are written in Crockford's base-32 alphabet, which omits `I`, `L`, `O` and `U` so a printed
// sheet reads unambiguously. `ActivationCode::parse` folds the ambiguous glyphs on the way in; so does
// this field, so what the operator sees is exactly what the edge will parse — a typo is caught by the
// checksum on the box, not after a round-trip to the cloud.
function normalize(raw: string): string {
  const symbols = raw
    .toUpperCase()
    .replace(/[IL]/gu, "1")
    .replace(/O/gu, "0")
    .replace(/[^0-9ABCDEFGHJKMNPQRSTVWXYZ]/gu, "")
    .slice(0, CODE_LENGTH);
  // Displayed as three hyphenated groups of four, the way the setup sheet prints it.
  return symbols.replace(/(.{4})(?=.)/gu, "$1-");
}

function symbolCount(formatted: string): number {
  return formatted.replace(/-/gu, "").length;
}

// What the box says about itself, resolved once on mount. `unavailable` is the LAN-only store server
// (or a keyring that cannot be read) — neither is an error the operator can fix from here.
type Standing = "checking" | "needed" | "activated" | "unavailable";

export function Setup() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const initial = typeof params["code"] === "string" ? normalize(params["code"]) : "";
  const [code, setCode] = createSignal(initial);
  const [error, setError] = createSignal<string | null>(null);
  const [standing, setStanding] = createSignal<Standing>("checking");
  const [busy, setBusy] = createSignal(false);

  onMount(() => {
    void api
      .activation()
      .then((state) => setStanding(state.activated ? "activated" : "needed"))
      .catch(() => setStanding("unavailable"));
  });

  const submit = async () => {
    setError(null);
    setBusy(true);
    try {
      await api.activate(code());
      setStanding("activated");
      // The store is now known to the cloud; this browser still needs its own device token before it
      // may send a command (ADR-0084), unless it already holds one.
      navigate(deviceToken() === null ? "/pair" : "/", { replace: true });
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="mx-auto max-w-sm p-4">
      <PageHeader title={t("setup.title")} />

      <Switch>
        <Match when={standing() === "checking"}>
          <p class="text-ink-muted">{t("setup.checking")}</p>
        </Match>

        <Match when={standing() === "activated"}>
          <div class="rounded-token border border-line bg-surface p-4">
            <p class="font-semibold text-ok">{t("setup.activated")}</p>
            <p class="mt-1 text-ink-muted">{t("setup.activated_hint")}</p>
          </div>
        </Match>

        <Match when={standing() === "unavailable"}>
          <div class="rounded-token border border-line bg-surface p-4">
            <p class="text-ink-muted">{t("setup.unavailable")}</p>
          </div>
        </Match>

        <Match when={standing() === "needed"}>
          <p class="text-ink-muted">{t("setup.hint")}</p>
          <input
            inputmode="text"
            autocapitalize="characters"
            autocomplete="off"
            spellcheck={false}
            aria-label={t("setup.code_label")}
            class="mt-3 w-full rounded-token border border-line bg-surface p-3 text-center text-xl tracking-[0.25em] uppercase"
            value={code()}
            onInput={(event) => setCode(normalize(event.currentTarget.value))}
          />
          {/* The count is symbols, not characters: the hyphens are formatting the field adds. */}
          <p class="mt-2 text-center text-ink-muted tabular-nums">
            {t("setup.code_progress", { entered: symbolCount(code()), total: CODE_LENGTH })}
          </p>
          {error() !== null && (
            <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
              {error()}
            </p>
          )}
          <button
            type="button"
            class="mt-3 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink disabled:opacity-50"
            disabled={busy() || symbolCount(code()) !== CODE_LENGTH}
            onClick={() => void submit()}
          >
            {t("setup.submit")}
          </button>
        </Match>
      </Switch>
    </section>
  );
}
