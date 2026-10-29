// The guided new-store wizard (ADR-0065, WS-C). Onboarding from zero without a ULID or a curl: name
// the store (optionally under a brand) → create it in the registry → optionally issue a scoped API
// key (shown once) → a handoff summary pointing at the next steps (activation, configuration). The
// key is optional because an activated machine syncs with its device credential (ADR-0143); it is
// for a box set up by hand. Composes the registry + API-key routes; tenant comes from the picker.
//
// A store it creates is given the owner's new-store values as soon as it exists
// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
// decision 1) — refusing to sell while no shift is open, and a drawer for each till among them, which
// the closing step says what the store's devices need for. That call is the wizard's only write
// after the store itself, and it is deliberately not part of creating it: a failure says so and
// points at Shared settings, where the same values can be given again, but it neither undoes the
// store nor holds the operator on the first step.

import { createSignal, For, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { api } from "../api/client";
import { type MessageKey, t, tFromServer } from "../i18n";
import { tenantId } from "../state/session";
import { screenHref, STORE_PARAM } from "../state/screens";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  PageHeader,
  SelectField,
  TextField,
} from "../components/ui";
import { apiMessage } from "../lib/errors";
import { WizardFiles } from "../components/WizardFiles";
import type { Brand, CreateApiKeyResponse, Store } from "../api/schemas";

// Scopes offered for the store's key, each mapped to a static i18n key (a template-literal key would
// not be a MessageKey and would defeat the type check).
//
// `read_config` and `relay_orders` are pre-selected together because a store box needs **both** or it
// is half-connected in a way that is hard to see: with only `read_config` it pulls its configuration
// happily while the order relay retries a `403` forever, so cloud-placed orders never reach the
// kitchen and the only symptom is a log line. That pairing is why the relay scope is offered here at
// all — it was missing from this list, which meant no operator could grant it from the console
// (roadmap-v3 E6).
const SCOPES: readonly { wire: string; key: MessageKey }[] = [
  { wire: "read_config", key: "scope.read_config" },
  { wire: "relay_orders", key: "scope.relay_orders" },
  { wire: "publish_events", key: "scope.publish_events" },
  { wire: "place_orders", key: "scope.place_orders" },
  { wire: "read_rollups", key: "scope.read_rollups" },
  { wire: "manage_devices", key: "scope.manage_devices" },
];

const STEP_KEYS: readonly MessageKey[] = ["wizard.step1", "wizard.step2", "wizard.step3"];

/** The setting whose new-store value has a store keep a drawer for each till (ADR-0167). */
const DRAWER_MODEL = "shift.drawer_model";

/** Where giving the new store the owner's new-store values stands. */
type Presets =
  | { readonly state: "applying" }
  | {
      readonly state: "applied";
      /** The settings written, by key. */
      readonly applied: readonly string[];
      /** Whether the store's publish went through; the values are saved either way. */
      readonly published: boolean;
    }
  | { readonly state: "failed"; readonly message: string };

/**
 * Shared settings, opened on `store`.
 *
 * The screen is tenant-scoped, so `screenHref` gives it no `?store=`; it opens on the store in
 * context, and this link makes that the store the wizard just created rather than whatever store
 * was in context before the wizard ran — the same reason {@link NextSteps}' links carry it.
 */
function settingsHref(tenant: string, store: string): string {
  return `${screenHref("settings", tenant, "")}?${STORE_PARAM}=${encodeURIComponent(store)}`;
}

/**
 * What became of the new store's new-store values, in a line or two.
 *
 * Exported for `tests/new-store-presets.test.tsx`, which pins the half that matters most: a failure
 * says the store exists and where to finish the job, and does not read as the store having failed.
 */
export function PresetsNote(props: { presets: Presets; tenant: string; store: string }) {
  const titles = (keys: readonly string[]) =>
    keys.map((key) => tFromServer(`settings.item.${key}.title`, key)).join(", ");
  return (
    <div class="flex flex-col gap-1">
      <Show when={props.presets.state === "applying"}>
        <p role="status" class="text-sm text-ink-muted">
          {t("wizard.presetsApplying")}
        </p>
      </Show>
      <Show when={props.presets.state === "applied" ? props.presets : null}>
        {(applied) => (
          <Show
            when={applied().published}
            fallback={<Banner tone="danger" message={t("wizard.presetsUnpublished")} />}
          >
            <p role="status" class="text-sm text-ink-muted">
              {t("wizard.presetsApplied", {
                count: applied().applied.length,
                list: titles(applied().applied),
              })}
            </p>
          </Show>
        )}
      </Show>
      <Show when={props.presets.state === "failed" ? props.presets : null}>
        {(failed) => (
          <Banner tone="danger" message={t("wizard.presetsFailed", { reason: failed().message })} />
        )}
      </Show>
      <Show
        when={
          props.presets.state === "failed" ||
          (props.presets.state === "applied" && !props.presets.published)
        }
      >
        <a class="text-sm text-accent underline" href={settingsHref(props.tenant, props.store)}>
          {t("wizard.presetsRetry")}
        </a>
      </Show>
    </div>
  );
}

/**
 * The two steps that remain once the wizard has finished, as links rather than as two screen names
 * in prose — and, where the new-store values gave the store a drawer for each till (`applied`, the
 * settings they wrote), what its devices need before it takes cash: a device bound to no till takes
 * none, and a drawer at a printer nobody marked does not open (ADR-0167).
 *
 * The closing banner used to read "Next: activate the store's devices (Activation) and publish its
 * configuration (Configuration)". That is the right advice, addressed to somebody who already knows
 * where those two screens live and can find this store in the top bar again — which is nobody who
 * needed a wizard. Both links carry the store that was just created rather than the ambient
 * context, which at this point is still whatever store the operator had chosen before they opened
 * the wizard.
 *
 * Exported so `tests/wizard-next-steps.test.tsx` can assert both links exist and both are scoped:
 * the defect was silent, because prose naming a screen renders exactly as well as a link to it.
 */
export function NextSteps(props: {
  tenant: string;
  store: string;
  applied?: readonly string[];
}) {
  return (
    <div class="flex flex-col gap-2 border-t border-line pt-4">
      <span class="text-sm font-medium text-ink">{t("wizard.nextTitle")}</span>
      <p class="text-sm text-ink-muted">{t("wizard.doneHint")}</p>
      <ul class="flex list-inside list-disc flex-col gap-1 text-sm">
        <li>
          <a
            class="text-accent underline"
            href={screenHref("activation", props.tenant, props.store)}
          >
            {t("wizard.nextActivation")}
          </a>
        </li>
        <li>
          {/* Store settings, not Configuration: the four publishes a shop needs (locale, tax, menu,
              roster) start there, and Configuration is the one screen among them a new store does
              not need. Same reasoning as the get-started's publish step (`lib/get-started.ts`). */}
          <a
            class="text-accent underline"
            href={screenHref("storeSettings", props.tenant, props.store)}
          >
            {t("wizard.nextConfig")}
          </a>
        </li>
      </ul>
      {/* The order matters and nothing else says it: a menu published over a missing tax node boots
          a store that cannot close a bill (ADR-0122 §7). */}
      <p class="text-sm text-ink-muted">{t("wizard.publishOrder")}</p>
      <Show when={props.applied?.includes(DRAWER_MODEL)}>
        <p class="text-sm text-ink-muted">{t("wizard.nextDrawers")}</p>
      </Show>
    </div>
  );
}

export function NewStore() {
  const navigate = useNavigate();

  const [step, setStep] = createSignal(1);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [brands, setBrands] = createSignal<Brand[]>([]);
  const [name, setName] = createSignal("");
  const [brandId, setBrandId] = createSignal("");
  const [created, setCreated] = createSignal<Store | null>(null);
  const [presets, setPresets] = createSignal<Presets | null>(null);

  const [scopes, setScopes] = createSignal<string[]>([
    "read_config",
    "relay_orders",
    "place_orders",
  ]);
  const [issued, setIssued] = createSignal<CreateApiKeyResponse | null>(null);

  const fail = (caught: unknown) =>
    setError(apiMessage(caught));

  const loadBrands = async () => {
    try {
      setBrands(await api.listBrands(tenantId()));
    } catch (caught) {
      fail(caught);
    }
  };

  // Load the tenant's brands once, so step 1's brand picker is populated.
  if (tenantId()) {
    void loadBrands();
  }

  const toggleScope = (wire: string) => {
    setScopes((current) =>
      current.includes(wire) ? current.filter((s) => s !== wire) : [...current, wire],
    );
  };

  // The settings the new-store values wrote, for the closing step to say what they ask of the store.
  const appliedPresets = () => {
    const state = presets();
    return state !== null && state.state === "applied" ? state.applied : [];
  };

  // The owner's new-store values for the store just created. Never thrown: whatever happens here,
  // the store exists and the wizard carries on, so the outcome is a note rather than a refusal.
  const applyPresets = async (store: string) => {
    setPresets({ state: "applying" });
    try {
      const report = await api.applySettingPresets(tenantId(), store);
      setPresets({
        state: "applied",
        applied: report.applied,
        published: report.stores.every((row) => row.outcome !== "SETTING_PUBLISH_FAILED"),
      });
    } catch (caught) {
      setPresets({ state: "failed", message: apiMessage(caught) });
    }
  };

  const createStore = async () => {
    // Idempotency (F0): the wizard mints the store exactly once. If it already did — the operator
    // stepped back to step 1 and forward again — advance without creating a second, orphaned store.
    if (created()) {
      setStep(2);
      return;
    }
    const storeName = name().trim();
    if (!storeName) {
      setError(t("stores.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      const store = await api.createStore(tenantId(), storeName, brandId() || undefined);
      setCreated(store);
      setStep(2);
      // Not awaited: the wizard moves on while the store is given its values.
      void applyPresets(store.store_id);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const issueKey = async () => {
    if (scopes().length === 0) {
      setError(t("wizard.scopesRequired"));
      return;
    }
    const store = created();
    if (!store) {
      return;
    }
    setError("");
    setBusy(true);
    try {
      // Bound to the store step 1 just created (S1). The store sync routes refuse a key that names
      // another store or none, so a wizard that issued a tenant-wide key here would hand the
      // operator a credential the box cannot use.
      setIssued(await api.createApiKey(tenantId(), scopes(), store.store_id));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("wizard.title")} description={t("wizard.description")} />
      <Show
        when={tenantId()}
        fallback={<Banner tone="danger" message={t("context.tenantRequired")} />}
      >
        <ol class="mb-6 flex flex-wrap gap-2" aria-label={t("wizard.title")}>
          <For each={STEP_KEYS}>
            {(key, index) => (
              <li
                class={`rounded-token border px-3 py-1 text-sm ${
                  step() === index() + 1
                    ? "border-accent bg-accent text-accent-ink"
                    : step() > index() + 1
                      ? "border-line bg-surface-raised text-ink-muted"
                      : "border-line text-ink-muted"
                }`}
                aria-current={step() === index() + 1 ? "step" : undefined}
              >
                {index() + 1}. {t(key)}
              </li>
            )}
          </For>
        </ol>

        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

        {/* Above the step cards, so the outcome stays in view from the key step to the handoff. */}
        <Show when={step() >= 2 ? presets() : null}>
          {(state) => (
            <Show when={created()}>
              {(store) => (
                <div class="mb-4">
                  <PresetsNote presets={state()} tenant={tenantId()} store={store().store_id} />
                </div>
              )}
            </Show>
          )}
        </Show>

        {/* Step 1 — store details */}
        <Show when={step() === 1}>
          <Card title={t("wizard.step1")}>
            <div class="flex flex-col gap-4">
              <p class="text-sm text-ink-muted">{t("wizard.step1Hint")}</p>
              <TextField
                label={t("stores.name")}
                value={name()}
                onInput={setName}
                placeholder={t("stores.namePlaceholder")}
              />
              <SelectField
                label={t("stores.brand")}
                value={brandId()}
                options={brands().map((brand) => ({ value: brand.brand_id, label: brand.name }))}
                onChange={setBrandId}
                placeholder={t("stores.noBrand")}
              />
              <div>
                <Button data-step="createStore" disabled={busy()} onClick={() => void createStore()}>
                  {t("wizard.next")}
                </Button>
              </div>
            </div>
          </Card>
        </Show>

        {/* Step 2 — API key */}
        <Show when={step() === 2}>
          <Card title={t("wizard.step2")}>
            <div class="flex flex-col gap-4">
              <Banner tone="ok" message={t("wizard.created")} />
              <p class="text-sm text-ink-muted">{t("wizard.step2Hint")}</p>
              <Show
                when={issued()}
                fallback={
                  <>
                    <fieldset class="flex flex-col gap-2">
                      <legend class="mb-1 text-sm font-medium text-ink">{t("wizard.scopes")}</legend>
                      <For each={SCOPES}>
                        {(scope) => (
                          <CheckboxField
                            label={t(scope.key)}
                            checked={scopes().includes(scope.wire)}
                            onChange={() => toggleScope(scope.wire)}
                          />
                        )}
                      </For>
                    </fieldset>
                    <div class="flex flex-wrap gap-2">
                      <Button
                        data-step="setStep"
                        data-step-value="details"
                        variant="secondary"
                        onClick={() => setStep(1)}
                      >
                        {t("wizard.back")}
                      </Button>
                      <Button data-step="issueKey" disabled={busy()} onClick={() => void issueKey()}>
                        {t("wizard.issueKey")}
                      </Button>
                      {/* The short path, named (decision D4). The Stores screen used to carry a
                          second create form for exactly this — a registry row and nothing else.
                          It is this step instead: it goes on to the setup files without a key,
                          and the hint below says why a store needs none (ADR-0143). */}
                      <Button
                        data-step="setStep"
                        data-step-value="handoff-without-key"
                        variant="ghost"
                        onClick={() => setStep(3)}
                      >
                        {t("wizard.skipKey")}
                      </Button>
                    </div>
                    <p class="text-sm text-ink-muted">{t("wizard.skipKeyHint")}</p>
                  </>
                }
              >
                {(key) => (
                  <>
                    <Banner tone="ok" message={t("wizard.keyIssued")} />
                    <div class="break-all rounded-token border border-line bg-surface-raised p-3 font-mono text-sm text-ink">
                      {key().token}
                    </div>
                    <div>
                      {/* The click the step budget did not know about until the browser gate ran
                          the flow: the key is shown once, and moving past it to the files is its
                          own decision. Three buttons share `setStep`, so each says which. */}
                      <Button data-step="setStep" data-step-value="handoff" onClick={() => setStep(3)}>
                        {t("wizard.next")}
                      </Button>
                    </div>
                  </>
                )}
              </Show>
            </div>
          </Card>
        </Show>

        {/* Step 3 — handoff */}
        <Show when={step() === 3}>
          <Card title={t("wizard.doneTitle")}>
            <div class="flex flex-col gap-4">
              <table class="w-full text-left text-sm">
                <tbody>
                  <tr class="border-b border-line">
                    <th class="py-2 pr-4 font-medium text-ink-muted">{t("wizard.storeLabel")}</th>
                    <td class="py-2">
                      <div class="flex flex-col">
                        <span class="text-ink">{created()?.name}</span>
                        <span class="font-mono text-xs text-ink-muted">{created()?.store_id}</span>
                      </div>
                    </td>
                  </tr>
                  <Show when={issued()}>
                    {(key) => (
                      <tr class="border-b border-line">
                        <th class="py-2 pr-4 font-medium text-ink-muted">{t("wizard.keyLabel")}</th>
                        <td class="py-2 font-mono text-xs text-ink-muted">{key().id}</td>
                      </tr>
                    )}
                  </Show>
                </tbody>
              </table>

              <Show when={created()}>
                {(store) => <WizardFiles store={store()} keyToken={issued()?.token ?? null} />}
              </Show>

              <Show when={created()}>
                {(store) => (
                  <NextSteps
                    tenant={tenantId()}
                    store={store().store_id}
                    applied={appliedPresets()}
                  />
                )}
              </Show>
              <div>
                <Button onClick={() => navigate(screenHref("stores", tenantId(), ""), { replace: true })}>
                  {t("wizard.finish")}
                </Button>
              </div>
            </div>
          </Card>
        </Show>
      </Show>
    </div>
  );
}
