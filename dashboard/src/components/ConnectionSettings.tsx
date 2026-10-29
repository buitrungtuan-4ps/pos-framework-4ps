// A connection's provider settings, as one fieldset of the integrations editor (ADR-0153).
//
// Moved out of the Integrations screen unchanged, so the screen has room for the fields it owns
// itself. It edits three pieces of the screen's state through their setters: the settings, the
// secrets typed in this edit, and the stored secrets the operator chose to remove.

import { Show, type Setter } from "solid-js";

import type { SettingValue } from "../api/types";
import type { Provider } from "../api/schemas";
import { t } from "../i18n";
import { ProviderFields } from "./ProviderFields";

export function ConnectionSettings(props: {
  provider: Provider;
  values: Record<string, SettingValue>;
  setValues: Setter<Record<string, SettingValue>>;
  secrets: Record<string, string>;
  setSecrets: Setter<Record<string, string>>;
  /** The secret fields the connection being edited already holds, for this provider. */
  secretsSet: readonly string[];
  clearing: string[];
  setClearing: Setter<string[]>;
}) {
  return (
    <fieldset class="border-t border-line pt-4">
      <legend class="mb-2 text-sm font-medium text-ink">{t("integrations.settings")}</legend>
      <Show
        when={props.provider.fields.length > 0}
        fallback={<p class="text-sm text-ink-muted">{t("integrations.noSettings")}</p>}
      >
        <ProviderFields
          fields={props.provider.fields}
          values={props.values}
          onValue={(key, value) =>
            props.setValues((current) => {
              const next = { ...current };
              if (value === undefined) {
                delete next[key];
              } else {
                next[key] = value;
              }
              return next;
            })
          }
          secrets={props.secrets}
          onSecret={(key, value) => props.setSecrets((current) => ({ ...current, [key]: value }))}
          secretsSet={props.secretsSet}
          clearing={props.clearing}
          onClear={(key, clear) =>
            props.setClearing((current) =>
              clear ? [...current, key] : current.filter((entry) => entry !== key),
            )
          }
        />
      </Show>
    </fieldset>
  );
}
