import { Match, Switch, createSignal, onCleanup, onMount } from "solid-js";

import { api } from "../api/client";
import type { ClaimStatus } from "../api/types";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";

// A box installed from the one image for every store, waiting to be claimed (ADR-0148). `pos-edge
// claim` serves this page on the box's own screen: it shows the code a person types at the console
// (Activation → Claim a box), and says so when the box has been claimed and the store server is about
// to start. There is no store yet, so nothing here needs a device token or a signed-in employee, and
// nothing is sent from this page: it only reads what the claim is doing.

// How often to ask what the claim is doing. The box polls the cloud every five seconds, so asking the
// box more often than this would only redraw the same answer.
const POLL_MS = 2_000;

export function Claim() {
  const [status, setStatus] = createSignal<ClaimStatus | null>(null);
  const [now, setNow] = createSignal(Date.now());

  let timer: ReturnType<typeof setInterval> | undefined;
  onMount(() => {
    const ask = () => {
      setNow(Date.now());
      void api
        .claimStatus()
        .then(setStatus)
        .catch(() => undefined);
    };
    ask();
    timer = setInterval(ask, POLL_MS);
  });
  onCleanup(() => clearInterval(timer));

  const minutesLeft = (expiresAtMs: number) =>
    Math.max(0, Math.ceil((expiresAtMs - now()) / 60_000));

  return (
    <section class="mx-auto max-w-md p-4">
      <PageHeader title={t("claim.title")} />
      <Switch>
        <Match when={status()?.state === "WAITING" && status()}>
          {(waiting) => {
            const current = waiting() as Extract<ClaimStatus, { state: "WAITING" }>;
            return (
              <div class="rounded-token border border-line bg-surface p-4 text-center">
                <p
                  class="font-mono text-4xl font-semibold tracking-[0.2em] text-ink"
                  aria-live="polite"
                >
                  {current.user_code}
                </p>
                <p class="mt-3 text-ink-muted">{t("claim.hint", { cloud: current.cloud })}</p>
                <p class="mt-2 text-sm text-ink-muted tabular-nums">
                  {t("claim.expires", { minutes: minutesLeft(current.expires_at_ms) })}
                </p>
              </div>
            );
          }}
        </Match>
        <Match when={status()?.state === "CLAIMED"}>
          <div class="rounded-token border border-line bg-surface p-4" role="status">
            <p class="font-semibold text-ok">{t("claim.claimed")}</p>
          </div>
        </Match>
        <Match when={status()?.state === "UNREACHABLE" && status()}>
          {(unreachable) => (
            <p class="rounded-token border border-danger px-3 py-2 text-danger" role="alert">
              {t("claim.unreachable", {
                cloud: (unreachable() as Extract<ClaimStatus, { state: "UNREACHABLE" }>).cloud,
              })}
            </p>
          )}
        </Match>
        <Match when={status()}>
          {(connecting) => (
            <p class="text-ink-muted">
              {t("claim.connecting", {
                cloud: "cloud" in connecting() ? (connecting() as { cloud: string }).cloud : "",
              })}
            </p>
          )}
        </Match>
      </Switch>
    </section>
  );
}
