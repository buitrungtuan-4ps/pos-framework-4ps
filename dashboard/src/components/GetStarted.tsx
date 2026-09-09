// The get-started checklist on the landing screen: what still has to happen before a shop can trade.
//
// The rules live in `lib/get-started.ts` and are unit tested there. This file is the reads and the
// markup: seven requests the console already makes elsewhere, each answering one step, and a list
// that stands down once every required step is done.
//
// # It never blocks and never writes
//
// Every row links to the screen that does the work; nothing is done here. A step whose read was
// refused says "could not check" rather than showing a cross — a role without permission on the
// fleet has not failed to install anything, and a checklist that reports work as undone because it
// could not look is worse than no checklist.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import { t } from "../i18n";
import type { MessageKey } from "../i18n";
import {
  requiredProgress,
  setupComplete,
  STEPS,
  statuses,
  type StepStatus,
} from "../lib/get-started";
import { LOADING, type Panel, panelOf } from "../lib/panel";
import { onScopedContext } from "../lib/scoped";
import { screenHref } from "../state/screens";
import { storeId, tenantId } from "../state/session";
import { Card } from "./ui";

/** The hue for a status. `todo` is deliberately not a danger colour: it is work, not a fault. */
function statusClass(status: StepStatus): string {
  switch (status) {
    case "done":
      return "text-ok";
    case "unknown":
      return "text-danger";
    default:
      return "text-ink-muted";
  }
}

const STATUS_LABEL: Record<StepStatus, MessageKey> = {
  checking: "getStarted.status.checking",
  done: "getStarted.status.done",
  todo: "getStarted.status.todo",
  waiting: "getStarted.status.waiting",
  unknown: "getStarted.status.unknown",
};

export function GetStarted() {
  const [tenants, setTenants] = createSignal<Panel<number>>(LOADING);
  const [brands, setBrands] = createSignal<Panel<number>>(LOADING);
  const [stores, setStores] = createSignal<Panel<number>>(LOADING);
  const [keys, setKeys] = createSignal<Panel<number>>(LOADING);
  const [versions, setVersions] = createSignal<Panel<number>>(LOADING);
  const [devices, setDevices] = createSignal<Panel<number>>(LOADING);
  const [reporting, setReporting] = createSignal<Panel<boolean>>(LOADING);

  // Counts, never the lists themselves: this asks "is there one" and nothing more, so a name, an
  // email or a key id cannot reach the checklist by accident.
  void panelOf(
    api.listTenants().then((list) => list.length),
    setTenants,
  );

  onScopedContext("tenant", (tenant) => {
    setBrands(LOADING);
    setStores(LOADING);
    void panelOf(
      api.listBrands(tenant).then((list) => list.length),
      setBrands,
    );
    void panelOf(
      api.listStores(tenant).then((list) => list.length),
      setStores,
    );
  });

  onScopedContext("store", (tenant, store) => {
    setKeys(LOADING);
    setVersions(LOADING);
    setDevices(LOADING);
    setReporting(LOADING);
    // Narrowed to keys this store can actually present: S1 refuses a tenant-wide key on a store's
    // own `/sync` routes, so a tenant holding one integration key and no store key is not set up.
    void panelOf(
      api
        .listApiKeys(tenant)
        .then((list) => list.filter((key) => !key.revoked && key.store_id === store).length),
      setKeys,
    );
    void panelOf(
      api.configVersions(tenant, store).then((list) => list.length),
      setVersions,
    );
    void panelOf(
      api.admittedDevices(tenant, store).then((list) => list.length),
      setDevices,
    );
    // Ever, not now: a shop closed for the night is still installed. `last_seen_at_ms` is the field
    // the cloud writes on the first heartbeat and never clears (ADR-0068).
    void panelOf(
      api.fleetStore(tenant, store).then((row) => row.last_seen_at_ms !== null),
      setReporting,
    );
  });

  const by = () =>
    statuses({
      tenantChosen: Boolean(tenantId()),
      storeChosen: Boolean(tenantId() && storeId()),
      tenants: tenants(),
      brands: brands(),
      stores: stores(),
      keys: keys(),
      versions: versions(),
      devices: devices(),
      reporting: reporting(),
    });

  return (
    <Show when={!setupComplete(by())}>
      <Card title={t("getStarted.title")}>
        <p class="text-sm text-ink-muted">{t("getStarted.description")}</p>
        <p class="mt-1 text-sm font-medium">{t("getStarted.progress", requiredProgress(by()))}</p>
        <ol class="mt-3 flex flex-col gap-3">
          {/* An ordered list, and the order is the whole point: the chain has to happen in this
              sequence. The step number is left to the list rather than printed into the title, so
              the title is one text node and assistive tech gets the position from the markup
              instead of from a string somebody has to keep in step with the array. */}
          <For each={STEPS}>
            {(step) => {
              const status = () => by()[step.id];
              return (
                <li class="flex flex-col gap-1 border-t border-line pt-3 first:border-0 first:pt-0">
                  <div class="flex flex-wrap items-baseline gap-2">
                    <span class="text-sm font-medium text-ink">{t(step.title)}</span>
                    <span class={`text-xs font-medium ${statusClass(status())}`}>
                      {t(STATUS_LABEL[status()])}
                    </span>
                    <Show when={step.optional}>
                      <span class="text-xs text-ink-muted">{t("getStarted.optional")}</span>
                    </Show>
                  </div>
                  <p class="text-sm text-ink-muted">{t(step.hint)}</p>
                  <Show when={step.go}>
                    {(go) => (
                      <p class="text-sm">
                        <a
                          class="text-accent underline"
                          href={screenHref(go().screen, tenantId(), storeId())}
                        >
                          {t(go().label)}
                        </a>
                      </p>
                    )}
                  </Show>
                </li>
              );
            }}
          </For>
        </ol>
      </Card>
    </Show>
  );
}
