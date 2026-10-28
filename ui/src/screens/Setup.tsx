import { Match, Show, Switch, createSignal, onMount } from "solid-js";
import { useNavigate, useSearchParams } from "@solidjs/router";

import { api, deviceToken } from "../api/client";
import { ReadFailed } from "../components/StoreData";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { createScreenRead, onStatus } from "../state/screen-read";

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

/// The most characters the field keeps: a grouped code (14) and the longest cell prefix with its
/// hyphen (33), with room to spare.
const MAX_TYPED = 64;

// Codes are written in Crockford's base-32 alphabet, which omits `I`, `L`, `O` and `U` so a printed
// sheet reads unambiguously; `ActivationCode::parse` folds the ambiguous glyphs on the box. A code may
// also carry its cell's id as a prefix, `VN1-XXXX-XXXX-XXXX` (ADR-0181), and a cell id is never
// folded (`lon1`, `us1`) and may hold hyphens (`jp-east-2`). So the field never folds and never drops
// a letter. While what is typed reads as groups of four, it groups the symbols as the sheet prints
// them; as soon as the operator puts a hyphen or a space anywhere else, a prefix is being typed, and
// the field keeps exactly what was typed, upper-cased, for the box to read from the end.
function normalize(raw: string): string {
  const upper = raw.toUpperCase().trimStart();
  const segments = upper.split(/[-\s]+/u);
  const prefixTyped = segments
    .slice(0, -1)
    .some((segment) => segment.length !== 4 || /[^0-9A-Z]/u.test(segment));
  if (prefixTyped) {
    return upper.replace(/[^0-9A-Z\s-]/gu, "").slice(0, MAX_TYPED);
  }
  const symbols = upper.replace(/[^0-9A-Z]/gu, "");
  // Displayed as hyphenated groups of four, the way the setup sheet prints a code.
  return symbols.replace(/(.{4})(?=.)/gu, "$1-").slice(0, MAX_TYPED);
}

// The code's symbols, counted from the end as the box reads them: separators skipped, the ambiguous
// glyphs counted as the symbols they fold to, and a cell prefix's own letters not counted once a
// character that is no code symbol, or the twelfth symbol, is reached.
function symbolCount(typed: string): number {
  let count = 0;
  for (const character of [...typed].reverse()) {
    if (count === CODE_LENGTH) {
      break;
    }
    if (/[-\s]/u.test(character)) {
      continue;
    }
    if (!/[0-9A-TV-Z]/u.test(character)) {
      break;
    }
    count += 1;
  }
  return count;
}

// What the box says about itself, resolved once on mount. `unavailable` is the LAN-only store server
// (or a keyring that cannot be read) — neither is an error the operator can fix from here.
type Standing = "checking" | "needed" | "restarting" | "activated" | "unavailable";

// How long to wait for the store server to come back after activating, and how often to ask. It
// restarts itself so its cloud sync starts with the new credential (ADR-0140); a store PC is back in
// a few seconds, and one still not answering after a minute is reported rather than waited on.
const RESTART_POLL_MS = 1_000;
const RESTART_GIVE_UP_MS = 60_000;

const pause = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

// A store server with no cloud does not mount the activation route, and answers `404`: there is
// nothing to activate. Anything else is retried with the engine's backoff and shown in place,
// rather than read as a box without a cloud.
function createStandingRead(
  setStanding: (standing: Standing) => void,
  setTooOpen: (tooOpen: boolean) => void,
) {
  return createScreenRead(
    () => api.activation(),
    (state) => {
      setStanding(state.activated ? "activated" : "needed");
      setTooOpen(state.state_access === "STATE_ACCESS_TOO_OPEN");
    },
    { handled: onStatus(404, () => setStanding("unavailable")) },
  );
}

// Other accounts on this computer can read the store's data, and running the installer again closes
// it (ADR-0174 decision 5).
function StateTooOpen(props: { when: boolean }) {
  return (
    <Show when={props.when}>
      <p class="mb-4 rounded-token border border-danger px-3 py-2 text-ink" role="alert" data-outcome="state-too-open">
        {t("setup.state_too_open")}
      </p>
    </Show>
  );
}

export function Setup() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const initial = typeof params["code"] === "string" ? normalize(params["code"]) : "";
  const [code, setCode] = createSignal(initial);
  const [error, setError] = createSignal<string | null>(null);
  const [standing, setStanding] = createSignal<Standing>("checking");
  const [busy, setBusy] = createSignal(false);

  // Other accounts on this computer can read the store's data (ADR-0174): said whatever the standing.
  const [tooOpen, setTooOpen] = createSignal(false);
  const standingRead = createStandingRead(setStanding, setTooOpen);
  onMount(() => void standingRead.run());

  const submit = async () => {
    setError(null);
    setBusy(true);
    try {
      await api.activate(code());
      // The box restarts itself now, so its cloud sync starts with the credential it was just given.
      // Wait for it to answer again before moving on, rather than dropping the operator on a pairing
      // screen whose first request would fail while it is down.
      setStanding("restarting");
      const deadline = Date.now() + RESTART_GIVE_UP_MS;
      await pause(RESTART_POLL_MS);
      while (Date.now() < deadline) {
        const back = await api.activation().then(
          (state) => state.activated,
          () => false,
        );
        if (back) {
          break;
        }
        await pause(RESTART_POLL_MS);
      }
      setStanding("activated");
      // The store is now known to the cloud; this browser still needs its own device token before it
      // may send a command (ADR-0084), unless it already holds one.
      navigate(deviceToken() === null ? "/pair" : "/", { replace: true });
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="mx-auto max-w-sm p-4">
      <PageHeader title={t("setup.title")} />
      <StateTooOpen when={tooOpen()} />

      <Switch>
        <Match when={standing() === "checking"}>
          <p class="text-ink-muted">{t("setup.checking")}</p>
          <ReadFailed read={standingRead} />
        </Match>

        <Match when={standing() === "restarting"}>
          <div class="rounded-token border border-line bg-surface p-4" role="status">
            <p class="font-semibold text-ok">{t("setup.activated")}</p>
            <p class="mt-1 text-ink-muted">{t("setup.restarting")}</p>
          </div>
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
          {/* The count is the code's symbols, not characters: not the hyphens, nor a cell prefix. */}
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
            class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
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
