// The editor for `receipt.templates` (ADR-0207), which the settings screen draws for a setting of
// that kind: the base template and each sales channel's, their header and footer lines as keys of
// the tenant's translations, and the optional sections on or off.
//
// The operator picks keys rather than typing text: a receipt holds no free text, and a key prints in
// the receipt's language, falling back to English. The keys offered are the ones with English text;
// one typed that has none is said beside it before any write, and the cloud refuses it too. A
// section that a country compiled into this cloud requires cannot be turned off anywhere, so its
// switch is drawn on and disabled, with the reason under it. Nothing here is personal data.

import { createMemo, createResource, createSignal, For, Index, Show } from "solid-js";

import { api } from "../../../api/client";
import type { Json } from "../../../api/types";
import { Button, SelectField, SwitchField, TextField } from "../../../components/ui";
import { t } from "../../../i18n";
import { channelLabel, SALES_CHANNELS } from "../../../lib/channels";
import {
  KEYS_MAX,
  type ReceiptTemplate,
  type ReceiptTemplates,
  readTemplates,
  SECTIONS,
  sectionOn,
  type TemplateFault,
  type TemplateSlot,
  templateAt,
  templateFault,
  withKeys,
  withSection,
  withTemplate,
} from "../../../lib/receipt-templates";

const SECTION_LABEL = {
  modifiers: "settings.receipt.section.modifiers",
  payments: "settings.receipt.section.payments",
  change_line: "settings.receipt.section.changeLine",
  second_language: "settings.receipt.section.secondLanguage",
} as const;

const PART_LABEL = {
  header_keys: "settings.receipt.header",
  footer_keys: "settings.receipt.footer",
} as const;

/** The keys of the tenant's grid that have English text. */
async function englishKeys(tenantId: string): Promise<ReadonlySet<string>> {
  const { value } = await api.getTranslations(tenantId);
  return new Set(
    Object.entries(value)
      .filter(([, texts]) => (texts?.en ?? "").trim() !== "")
      .map(([key]) => key),
  );
}

/** Every section a country compiled into this cloud requires: the cloud refuses turning any off. */
async function requiredSections(): Promise<ReadonlySet<string>> {
  const countries = await api.listCountries();
  return new Set(countries.flatMap((country) => country.receipt_required_sections));
}

/** A fault in the operator's words. */
function faultText(fault: TemplateFault): string {
  switch (fault.kind) {
    case "too_many_keys":
      return t("settings.receipt.fault.tooMany", { part: t(PART_LABEL[fault.part]), max: KEYS_MAX });
    case "blank_key":
      return t("settings.receipt.fault.blank", { part: t(PART_LABEL[fault.part]) });
    case "key_without_english":
      return t("settings.receipt.fault.noEnglish", { key: fault.key });
    case "section_required":
      return t("settings.receipt.fault.required", { section: t(SECTION_LABEL[fault.field]) });
  }
}

/**
 * One template's header or footer lines: a field per key, a way to remove one, and one to add. Rows
 * are kept by position, so a field being typed in is not redrawn under the operator's cursor.
 */
function KeyLines(props: {
  part: "header_keys" | "footer_keys";
  template: ReceiptTemplate;
  suggestions: readonly { readonly value: string }[];
  disabled: boolean;
  onChange: (template: ReceiptTemplate) => void;
}) {
  const keys = () => props.template[props.part] ?? [];
  const set = (next: readonly string[]) => props.onChange(withKeys(props.template, props.part, next));
  return (
    <fieldset class="flex flex-col gap-2">
      <legend class="text-sm font-medium text-ink">{t(PART_LABEL[props.part])}</legend>
      <Index each={keys()}>
        {(key, index) => (
          <div class="flex items-end gap-2">
            <div class="flex-1">
              <TextField
                label={t("settings.receipt.line", { number: index + 1 })}
                value={key()}
                suggestions={props.suggestions}
                onInput={(typed) => set(keys().map((old, at) => (at === index ? typed : old)))}
              />
            </div>
            <Button variant="secondary" size="sm" disabled={props.disabled}
              onClick={() => set(keys().filter((_, at) => at !== index))}>
              {t("settings.receipt.removeLine")}
            </Button>
          </div>
        )}
      </Index>
      <Show when={keys().length < KEYS_MAX}>
        <div>
          <Button variant="secondary" size="sm" disabled={props.disabled} onClick={() => set([...keys(), ""])}>
            {t("settings.receipt.addLine")}
          </Button>
        </div>
      </Show>
    </fieldset>
  );
}

/** The receipt templates written at the chosen scope, edited as structure. */
export function ReceiptTemplatesEditor(props: {
  tenantId: string;
  value: Json | undefined;
  disabled: boolean;
  onChange: (value: Json) => void;
}) {
  const [slot, setSlot] = createSignal<TemplateSlot>("base");
  const [english] = createResource(() => props.tenantId, englishKeys);
  const [required] = createResource(requiredSections);
  const templates = (): ReceiptTemplates => readTemplates(props.value);
  const template = () => templateAt(templates(), slot());
  const write = (next: ReceiptTemplate | undefined) =>
    props.onChange(withTemplate(templates(), slot(), next) as Json);
  const suggestions = createMemo(() => [...(english.error ? [] : (english() ?? []))].sort().map((value) => ({ value })));
  const requiredSet = () => (required.error ? new Set<string>() : (required() ?? new Set<string>()));
  const fault = () => {
    const current = template();
    const known = english.error ? undefined : english();
    return current === undefined ? null : templateFault(current, known ? (key) => known.has(key) : null, requiredSet());
  };
  const slotLabel = (value: TemplateSlot) => {
    const name = value === "base" ? t("settings.receipt.base") : channelLabel(value);
    return templateAt(templates(), value) === undefined ? t("settings.receipt.unwritten", { name }) : name;
  };
  return (
    <div class="flex flex-col gap-4" data-editor="receipt-templates">
      <SelectField
        label={t("settings.receipt.template")}
        value={slot()}
        options={["base", ...SALES_CHANNELS].map((value) => ({ value, label: slotLabel(value) }))}
        onChange={setSlot}
        hint={t("settings.receipt.templateHint")}
      />
      <Show
        when={template()}
        fallback={
          <div class="flex flex-col gap-2">
            <p class="text-sm text-ink-muted">
              {slot() === "base" ? t("settings.receipt.noBase") : t("settings.receipt.noChannel")}
            </p>
            <div>
              <Button variant="secondary" disabled={props.disabled} onClick={() => write({})}>
                {t("settings.receipt.add")}
              </Button>
            </div>
          </div>
        }
      >
        {(current) => (
          <>
            <KeyLines part="header_keys" template={current()} suggestions={suggestions()}
              disabled={props.disabled} onChange={write} />
            <KeyLines part="footer_keys" template={current()} suggestions={suggestions()}
              disabled={props.disabled} onChange={write} />
            <Show when={english.error}>
              <p class="text-sm text-ink-muted">{t("settings.receipt.gridUnread")}</p>
            </Show>
            <For each={SECTIONS}>
              {({ field, token }) => (
                <SwitchField
                  label={t(SECTION_LABEL[field])}
                  checked={requiredSet().has(token) || sectionOn(current(), field)}
                  onChange={(on) => write(withSection(current(), field, on))}
                  onLabel={t("settings.receipt.prints")}
                  offLabel={t("settings.receipt.leftOut")}
                  disabled={props.disabled || requiredSet().has(token)}
                  hint={requiredSet().has(token) ? t("settings.receipt.requiredByLaw") : undefined}
                />
              )}
            </For>
            <Show when={fault()}>
              {(found) => <p class="text-sm text-danger" role="alert">{faultText(found())}</p>}
            </Show>
            <div>
              <Button variant="danger-ghost" disabled={props.disabled} onClick={() => write(undefined)}>
                {t("settings.receipt.remove")}
              </Button>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
