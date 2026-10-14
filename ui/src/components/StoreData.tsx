// What the till says while the store's data loads, and when part of it could not be loaded
// (ADR-0173 decision 2). The engine (`state/engine.ts`) decides; these only draw what it decided.

import { For, Show, type ParentProps } from "solid-js";
import { useLocation } from "@solidjs/router";

import { t, type MessageKey } from "../i18n";
import { notLoaded, retryFailing, storeReady, unavailable, waitingFor } from "../state/engine";

// The screens that draw nothing of the store's: pairing, signing in and activating a box. A device
// there has not started the store's load, and must reach it without it.
const UNGATED = ["/pair", "/signin", "/setup"];

// Each registry entry's name, as the operator reads it (`state/registry.ts`).
const NAMES: Record<string, MessageKey> = {
  floor: "data.name.floor",
  menu: "data.name.menu",
  menu_takeaway: "data.name.menu_takeaway",
  menu_dine_in: "data.name.menu_dine_in",
  layout: "data.name.layout",
  locale: "data.name.locale",
  reason_codes: "data.name.reason_codes",
};

// The name of a piece of store data, in the operator's language.
function nameOf(key: string): string {
  const name = NAMES[key];
  return name === undefined ? key : t(name);
}

function RetryButton() {
  return (
    <button
      type="button"
      class="min-h-touch rounded-token border border-line px-3 text-sm font-semibold text-ink"
      data-step="retryLoad"
      onClick={() => void retryFailing()}
    >
      {t("data.retry")}
    </button>
  );
}

// Holds a screen back until every piece of data the till needs to sell has loaded. It says what it
// is waiting for, and, where a piece could not be loaded, which, with a Retry. The engine retries on
// its own as well; the button only saves the wait.
export function StoreGate(props: ParentProps) {
  const location = useLocation();
  const open = () => storeReady() || UNGATED.includes(location.pathname);
  return (
    <Show when={open()} fallback={<Waiting />}>
      {props.children}
    </Show>
  );
}

function Waiting() {
  return (
    <section class="mx-auto max-w-md p-6" role="status" data-outcome="store-loading">
      <h1 class="text-lg font-semibold text-ink">{t("data.waiting")}</h1>
      <ul class="mt-3 text-sm text-ink-muted">
        <For each={waitingFor()}>
          {(key) => (
            <li>
              {notLoaded().includes(key)
                ? t("data.unavailable_failed", { name: nameOf(key) })
                : t("data.unavailable_loading", { name: nameOf(key) })}
            </li>
          )}
        </For>
      </ul>
      <Show when={notLoaded().length > 0}>
        <div class="mt-4">
          <RetryButton />
        </div>
      </Show>
    </section>
  );
}

// A piece of data the till can be drawn without that could not be loaded, such as the button plan
// or the reason codes: the till works, and this says what is missing and offers a Retry.
export function StoreDataBanner() {
  const missing = () => (storeReady() ? notLoaded() : []);
  return (
    <Show when={missing().length > 0}>
      <div
        role="alert"
        class="flex flex-wrap items-center gap-3 border-b border-line border-l-4 border-l-awaiting bg-surface-raised px-4 py-2"
        data-outcome="store-data-missing"
      >
        <div class="flex-1">
          <p class="text-sm font-semibold text-ink">{t("data.failed_title")}</p>
          <ul class="text-sm text-ink-muted">
            <For each={missing()}>{(key) => <li>{nameOf(key)}</li>}</For>
          </ul>
        </div>
        <RetryButton />
      </div>
    </Show>
  );
}

// Why a button that needs `entry` is disabled, beside it, while the entry is loading or could not be
// loaded; nothing once it has. Its `id` is what the button names in `aria-describedby`.
export function WhyUnavailable(props: { entry: string; id: string }) {
  return (
    <Show when={unavailable(props.entry)}>
      {(reason) => (
        <span id={props.id} class="text-sm text-ink-muted" data-outcome="why-unavailable">
          {t(reason(), { name: nameOf(props.entry) })}
        </span>
      )}
    </Show>
  );
}
