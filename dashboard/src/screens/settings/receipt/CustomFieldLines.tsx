// The custom fields a receipt template prints under its header (ADR-0207; phase 10 open item O8): one
// switch per order or bill field the tenant's packages declare, read from their versions. The edge
// prints each as its label and the bill's value, skips a field the running package does not declare,
// and prints a personal value only on the person's own copy (ADR-0207 decision 5).

import { createResource, For, Show } from "solid-js";

import { listPackages, listPackageVersions } from "../../../api/packages";
import { t, tFromServer } from "../../../i18n";
import { type ReceiptTemplate, customFieldsOf, printableFields, withCustomFields } from "../../../lib/receipt-templates";
import { SwitchField } from "../../../components/ui";

async function loadFields(tenantId: string) {
  const packages = await listPackages(tenantId);
  const versions = await Promise.all(packages.map((record) => listPackageVersions(tenantId, record.package_id)));
  return printableFields(versions.flat());
}

export function CustomFieldLines(props: {
  tenantId: string;
  template: ReceiptTemplate;
  disabled: boolean;
  onChange: (next: ReceiptTemplate) => void;
}) {
  const [fields] = createResource(() => props.tenantId, loadFields);
  const chosen = () => customFieldsOf(props.template);
  const set = (id: string, on: boolean) =>
    props.onChange(withCustomFields(props.template, on ? [...chosen(), id] : chosen().filter((held) => held !== id)));
  return (
    <div class="flex flex-col gap-2" data-editor="receipt-custom-fields">
      <p class="text-sm font-semibold">{t("settings.receipt.customFields")}</p>
      <Show when={(fields() ?? []).length > 0} fallback={<p class="text-sm text-ink-muted">{t("settings.receipt.noCustomFields")}</p>}>
        <For each={fields()}>
          {(field) => (
            <SwitchField
              label={tFromServer(field.labelKey, field.labelKey)}
              checked={chosen().includes(field.id)}
              onChange={(on) => set(field.id, on)}
              onLabel={t("settings.receipt.prints")}
              offLabel={t("settings.receipt.leftOut")}
              disabled={props.disabled}
              hint={field.personal ? t("settings.receipt.personalField") : undefined}
            />
          )}
        </For>
      </Show>
    </div>
  );
}
