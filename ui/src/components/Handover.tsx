// The handover panel on the Shift screen, "Giao ca" (ADR-0187): one flow on one till from the
// outgoing count to the incoming opening, whose state and acts are `state/handover.ts`. Nobody is
// named on screen; the edge prints the handover slip as the incoming shift opens.

import { For, Show, createSignal } from "solid-js";
import { Dynamic } from "solid-js/web";

import { api } from "../api/client";
import type { DrawerEntry } from "../api/schemas";
import { t } from "../i18n";
import { errorMessage } from "../lib/errors";
import { money } from "../lib/money";
import { printOutcomeKey } from "../lib/print";
import {
  ask,
  busy,
  closeOut,
  count,
  countingOut,
  difference,
  dismiss,
  error,
  goTo,
  handIn,
  handOut,
  outgoing,
  pending,
  printed,
  refusal,
  setCount,
  setText,
  signInIncoming,
  step,
  text,
  typed,
} from "../state/handover";
import { holdsOwn } from "../state/permissions";
import { formatAmount, reasonsFor, storeCurrency } from "../state/store";
import { ApproverFields } from "./ApproverFields";
import { CountField } from "./CountField";
import { PendingSettle } from "./PendingSettle";
import { RefusalPanel } from "./RefusalPanel";

export { handingOver } from "../state/handover";

const CASH_VARIANCE = "REASON_ACTION_CASH_VARIANCE";

/** The flow's button, which says so while its act is with the edge. */
function Act(props: { label: string; step: string; disabled: boolean; onClick: () => void; plain?: boolean }) {
  return (
    <button
      type="button"
      class="mt-3 min-h-touch w-full rounded-token font-semibold disabled:opacity-50"
      classList={{
        "bg-primary text-primary-ink": props.plain !== true,
        "border border-line": props.plain === true,
      }}
      disabled={props.disabled || busy()}
      data-step={props.step}
      onClick={() => props.onClick()}
    >
      {props.label}
    </button>
  );
}

/** The count field, for the outgoing and the incoming count alike. */
function Count(props: { id: string; label: string; step: string }) {
  return (
    <CountField
      id={props.id}
      label={props.label}
      text={text()}
      onText={setText}
      count={count()}
      onCount={setCount}
      step={props.step}
    />
  );
}

function HandOut() {
  return (
    <>
      <Show when={countingOut()}>
        <Count id="handover-out" label={t("handover.count_out", { currency: storeCurrency() })} step="handoverOutKeypad" />
      </Show>
      <Act
        label={t("handover.close_out")}
        step="handOut"
        disabled={countingOut() && typed() === null}
        onClick={() => handOut()}
      />
    </>
  );
}

function Reasons() {
  return (
    <>
      <p class="mt-2 text-sm text-ink-muted">{t("shift.variance_reason")}</p>
      <div class="mt-2 grid grid-cols-2 gap-2">
        <For each={reasonsFor(CASH_VARIANCE)}>
          {(reason) => (
            <button
              type="button"
              class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
              disabled={busy()}
              data-step="handoverReason"
              onClick={() => closeOut(reason.reason_code_id)}
            >
              {reason.display_name}
            </button>
          )}
        </For>
      </div>
    </>
  );
}

function SignIn() {
  const [code, setCode] = createSignal("");
  const [pin, setPin] = createSignal("");
  const counted = () => formatAmount(outgoing()?.counted ?? money(storeCurrency(), 0));
  return (
    <>
      <p class="mt-2 text-sm" role="status" data-outcome="handover-closed">
        {t("handover.closed_out", { amount: counted() })}
      </p>
      <p class="mt-2 text-sm text-ink-muted">{t("handover.sign_in")}</p>
      <ApproverFields
        id="handover-incoming"
        code={code()}
        pin={pin()}
        onCode={setCode}
        onPin={setPin}
        codeLabel={t("signin.code")}
        pinLabel={t("signin.pin")}
      />
      <Act
        label={t("handover.sign_in_button")}
        step="handoverSignIn"
        disabled={code().trim() === "" || pin() === ""}
        onClick={() => {
          const typedPin = pin();
          setPin("");
          void signInIncoming(code().trim(), typedPin);
        }}
      />
    </>
  );
}

function HandIn() {
  return (
    <>
      <Count id="handover-in" label={t("handover.count_in", { currency: storeCurrency() })} step="handoverInKeypad" />
      <Show when={ask() !== null && difference()}>
        {(gap) => (
          <p class="mt-2 text-sm text-danger tabular-nums" role="alert" data-outcome="handover-differs">
            {t(ask() === "confirm" ? "handover.differs_confirm" : "handover.differs_recount", {
              amount: formatAmount(gap()),
            })}
          </p>
        )}
      </Show>
      <Act label={t("handover.open_in")} step="handIn" disabled={typed() === null} onClick={() => handIn(false)} />
      <Show when={ask() === "confirm"}>
        <Act
          label={t("handover.confirm_in")}
          step="confirmHandIn"
          disabled={typed() === null}
          plain
          onClick={() => handIn(true)}
        />
      </Show>
    </>
  );
}

function Done() {
  return (
    <>
      <p class="mt-2 tabular-nums" role="status" data-outcome="handover-done">
        {t("handover.done", { amount: formatAmount(difference() ?? money(storeCurrency(), 0)) })}
      </p>
      <Show when={printed()}>
        {(outcome) => (
          <p class="mt-1 text-sm text-ink-muted" role="status" data-outcome="handover-slip">
            {t(printOutcomeKey(outcome(), "handover.slip_printed"))}
          </p>
        )}
      </Show>
      <button type="button" class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm" onClick={() => goTo(null)}>
        {t("handover.finish")}
      </button>
    </>
  );
}

const STEPS = { out: HandOut, reason: Reasons, signin: SignIn, in: HandIn, done: Done } as const;

export function Handover() {
  return (
    <div class="mt-4" data-outcome="handover">
      <Show
        when={step()}
        fallback={
          <button
            type="button"
            class="min-h-touch w-full rounded-token border border-line bg-surface font-semibold"
            data-step="beginHandover"
            onClick={() => goTo("out")}
          >
            {t("handover.begin")}
          </button>
        }
      >
        {(current) => (
          <div class="rounded-token border border-line bg-surface p-3">
            <h2 class="font-semibold">{t("handover.title")}</h2>
            <Dynamic component={STEPS[current()]} />
            <Show when={pending()}>
              {(out) => <PendingSettle act={out().act} phase={out().phase} amount="" onLeave={() => goTo(null)} />}
            </Show>
            <RefusalPanel message={refusal()} onDismiss={dismiss} />
            <Show when={error()}>
              {(message) => (
                <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                  {message()}
                </p>
              )}
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}

/**
 * Prints the handover slip of `drawer`'s shift again, from the Shift screen and the drawer list
 * (ADR-0187 decision 3): only for a shift that took its drawer over, and a person whose own role
 * reads an X's expected cash, as the edge asks.
 */
export function ReprintSlip(props: { drawer?: DrawerEntry }) {
  const [outcome, setOutcome] = createSignal<string | null>(null);
  const [failed, setFailed] = createSignal<string | null>(null);
  const [sending, setSending] = createSignal(false);
  const shiftId = () =>
    props.drawer?.shift?.handed_over_from_shift_id === undefined ? undefined : props.drawer.shift.shift_id;
  const print = async (id: string) => {
    setSending(true);
    setFailed(null);
    try {
      setOutcome((await api.reprintHandoverSlip(id)).handover_slip_print);
    } catch (caught) {
      setFailed(errorMessage(caught));
    } finally {
      setSending(false);
    }
  };
  return (
    <Show when={holdsOwn("reports.shift.read_x") && shiftId()}>
      {(id) => (
        <div class="mt-3">
          <button
            type="button"
            class="min-h-touch w-full rounded-token border border-line text-sm disabled:opacity-50"
            disabled={sending()}
            data-step="reprintSlip"
            onClick={() => void print(id())}
          >
            {t(sending() ? "common.sending" : "handover.reprint")}
          </button>
          <Show when={outcome()}>
            {(done) => (
              <p class="mt-1 text-sm text-ink-muted" role="status" data-outcome="handover-slip-again">
                {t(printOutcomeKey(done(), "handover.slip_printed"))}
              </p>
            )}
          </Show>
          <Show when={failed()}>{(message) => <p class="mt-1 text-sm text-danger">{message()}</p>}</Show>
        </div>
      )}
    </Show>
  );
}
