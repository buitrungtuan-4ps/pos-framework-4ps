// A provider's settings form, drawn from the schema the cloud serves
// ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decision 3).
//
// There is no form per vendor, and that is the point: the adapter describes its own fields in the
// catalogue (`GET /admin/integrations/providers`), so a vendor added to the cloud appears here with
// its form and nothing in the console changes. The schema is small on purpose — six kinds, no
// conditional logic — and each kind maps to one of the kit's inputs.
//
// A secret is write-only. The input starts empty whatever the connection holds, because the cloud
// never sends a stored value back; it says only that one is set, and leaving the input blank keeps
// it. That is why a secret has its own two maps here rather than living in `values`.

import { For, Match, Show, Switch } from "solid-js";

import type { ProviderField, SettingValue } from "../api/types";
import { t, tFromServer } from "../i18n";
import { CheckboxField, NumberField, SelectField, TextField } from "./ui";

/** A field's label: its translation if this console ships one, else its key, marked if required. */
function labelOf(field: ProviderField): string {
  const label = tFromServer(field.label_key, field.key);
  return field.required ? t("integrations.requiredLabel", { label }) : label;
}

export function ProviderFields(props: {
  fields: readonly ProviderField[];
  /** The non-secret settings as they stand in the form. */
  values: Readonly<Record<string, SettingValue>>;
  /** Sets one setting, or removes it when the operator emptied the input. */
  onValue: (key: string, value: SettingValue | undefined) => void;
  /** Secret values typed in this edit. Never pre-filled. */
  secrets: Readonly<Record<string, string>>;
  onSecret: (key: string, value: string) => void;
  /** The secret fields the connection already holds a value for. */
  secretsSet: readonly string[];
  /** The stored secrets the operator has chosen to remove. */
  clearing: readonly string[];
  onClear: (key: string, clear: boolean) => void;
}) {
  const text = (key: string) => {
    const value = props.values[key];
    return typeof value === "string" ? value : "";
  };
  const setText = (key: string, value: string) =>
    props.onValue(key, value.trim() === "" ? undefined : value);

  return (
    <div class="flex flex-col gap-4">
      <For each={props.fields}>
        {(field) => (
          <Switch>
            <Match when={field.kind === "SETTING_KIND_TEXT"}>
              <TextField
                label={labelOf(field)}
                value={text(field.key)}
                onInput={(value) => setText(field.key, value)}
              />
            </Match>
            <Match when={field.kind === "SETTING_KIND_URL"}>
              <TextField
                label={labelOf(field)}
                type="url"
                value={text(field.key)}
                onInput={(value) => setText(field.key, value)}
                placeholder={t("integrations.urlPlaceholder")}
                hint={t("integrations.urlHint")}
              />
            </Match>
            <Match when={field.kind === "SETTING_KIND_NUMBER"}>
              <NumberField
                label={labelOf(field)}
                value={
                  typeof props.values[field.key] === "number"
                    ? (props.values[field.key] as number)
                    : null
                }
                min={field.min}
                max={field.max}
                step={1}
                onChange={(value) => props.onValue(field.key, value ?? undefined)}
                hint={
                  field.min !== undefined && field.max !== undefined
                    ? t("integrations.numberRange", {
                        min: String(field.min),
                        max: String(field.max),
                      })
                    : undefined
                }
              />
            </Match>
            <Match when={field.kind === "SETTING_KIND_CHOICE"}>
              <SelectField
                label={labelOf(field)}
                value={text(field.key)}
                options={(field.options ?? []).map((option) => ({
                  value: option,
                  label: tFromServer(`${field.label_key}.${option}`, option),
                }))}
                placeholder={field.required ? undefined : t("integrations.choiceNone")}
                onChange={(value) => setText(field.key, value)}
              />
            </Match>
            <Match when={field.kind === "SETTING_KIND_FLAG"}>
              <CheckboxField
                label={labelOf(field)}
                checked={props.values[field.key] === true}
                onChange={(on) => props.onValue(field.key, on)}
              />
            </Match>
            <Match when={field.kind === "SETTING_KIND_SECRET"}>
              <div class="flex flex-col gap-1" data-secret-field={field.key}>
                <TextField
                  label={labelOf(field)}
                  type="password"
                  autocomplete="new-password"
                  value={props.secrets[field.key] ?? ""}
                  onInput={(value) => props.onSecret(field.key, value)}
                  hint={
                    props.secretsSet.includes(field.key)
                      ? t("integrations.secretStored")
                      : t("integrations.secretWriteOnly")
                  }
                />
                <Show when={props.secretsSet.includes(field.key) && !field.required}>
                  <CheckboxField
                    label={t("integrations.secretClear")}
                    checked={props.clearing.includes(field.key)}
                    onChange={(on) => props.onClear(field.key, on)}
                  />
                </Show>
              </div>
            </Match>
          </Switch>
        )}
      </For>
    </div>
  );
}
