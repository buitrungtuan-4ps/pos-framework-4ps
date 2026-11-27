// The editor for `order_channels.order_types` and `order_channels.sources` (ADR-0260), which the
// settings screen draws for a setting of either kind at the chosen scope.
//
// One row per entry: its English and Vietnamese names, and for a type its base kind (dine-in,
// takeaway or delivery, which alone sets an order's price and tax), whether it is the default and
// which outside orders take it. A new entry's token is made from its English name; an entry this
// scope already saved keeps its token and cannot be removed, only set inactive, as the cloud
// requires. A fault is said under the list before any write, and the cloud checks again.

import { For, Show } from "solid-js";

import type { Json } from "../../api/types";
import { Button, CheckboxField, SelectField, SwitchField, TextField } from "../../components/ui";
import { ReceiptTemplatesEditor } from "./receipt/ReceiptTemplatesEditor";
import { t, tFromServer } from "../../i18n";
import {
  APPLIES_TO,
  BASE_KINDS,
  BUILT_IN_SOURCES,
  type ListFault,
  type OrderTypeEntry,
  ORDER_TYPES_KIND,
  readEntries,
  SALE_SOURCES_KIND,
  type SourceEntry,
  sourcesFault,
  toValue,
  tokenFrom,
  typesFault,
  withName,
} from "../../lib/order-channels";

function faultText(fault: ListFault): string {
  switch (fault.kind) {
    case "too_many":
      return t("settings.orderChannels.fault.tooMany", { max: fault.max });
    case "bad_token":
      return t("settings.orderChannels.fault.badToken", { token: fault.token });
    case "duplicate":
      return t("settings.orderChannels.fault.duplicate", { token: fault.token });
    case "needs_english":
      return t("settings.orderChannels.fault.needsEnglish", { token: fault.token });
    case "name_too_long":
      return t("settings.orderChannels.fault.nameTooLong", { token: fault.token });
    case "two_defaults":
      return t("settings.orderChannels.fault.twoDefaults");
    case "inactive_default":
      return t("settings.orderChannels.fault.inactiveDefault", { token: fault.token });
  }
}

/** A token in the console's words where it ships them, else the token. */
function tokenLabel(token: string): string {
  return tFromServer(`settings.value.${token}`, token);
}

/** The names of one entry: English and Vietnamese. */
function NameFields(props: {
  names: OrderTypeEntry["names"];
  disabled: boolean;
  onChange: (language: string, name: string) => void;
}) {
  return (
    <div class="grid gap-2 sm:grid-cols-2">
      <TextField label={t("settings.orderChannels.nameEn")} value={props.names?.en ?? ""}
        onInput={(typed) => props.onChange("en", typed)} />
      <TextField label={t("settings.orderChannels.nameVi")} value={props.names?.vi ?? ""}
        onInput={(typed) => props.onChange("vi", typed)} />
    </div>
  );
}

type Entry = OrderTypeEntry | SourceEntry;

const tokenOf = (entry: Entry) => ("order_type" in entry ? entry.order_type : entry.source);
const builtIn = (token: string) =>
  (BASE_KINDS as readonly string[]).includes(token) || (BUILT_IN_SOURCES as readonly string[]).includes(token);

/** A type's own fields: its base kind, whether it is the default, and the outside orders it takes. */
function TypeFields(props: {
  entry: OrderTypeEntry;
  disabled: boolean;
  onChange: (change: Partial<OrderTypeEntry>) => void;
  onDefault: (on: boolean) => void;
}) {
  return (
    <>
      <SelectField
        label={t("settings.orderChannels.base")}
        value={props.entry.base_order_type}
        options={BASE_KINDS.map((kind) => ({ value: kind, label: tokenLabel(kind) }))}
        onChange={(kind) => props.onChange({ base_order_type: kind })}
        disabled={props.disabled || builtIn(props.entry.order_type)}
        hint={t("settings.orderChannels.baseHint")}
      />
      <SwitchField label={t("settings.orderChannels.default")} checked={props.entry.default === true}
        onChange={props.onDefault} onLabel={t("settings.on")} offLabel={t("settings.off")} disabled={props.disabled} />
      <div class="flex flex-wrap gap-3" role="group" aria-label={t("settings.orderChannels.appliesTo")}>
        <span class="text-sm font-medium text-ink">{t("settings.orderChannels.appliesTo")}</span>
        <For each={APPLIES_TO}>
          {(source) => (
            <CheckboxField
              label={tokenLabel(source)}
              checked={(props.entry.applies_to_sources ?? []).includes(source)}
              disabled={props.disabled}
              onChange={(on) => {
                const now = (props.entry.applies_to_sources ?? []).filter((kept) => kept !== source);
                props.onChange({ applies_to_sources: on ? [...now, source] : now });
              }}
            />
          )}
        </For>
      </div>
    </>
  );
}

/** One entry: its names, a type's own fields, offered or retired, and removal while unsaved. */
function EntryRow(props: {
  entry: Entry;
  saved: boolean;
  disabled: boolean;
  onChange: (change: Partial<OrderTypeEntry & SourceEntry>) => void;
  onRename: (language: string, name: string) => void;
  onDefault: (on: boolean) => void;
  onRemove: () => void;
}) {
  return (
    <fieldset class="flex flex-col gap-2 rounded-token border border-line p-3" data-entry={tokenOf(props.entry)}>
      <legend class="px-1 font-mono text-xs text-ink-muted">{tokenOf(props.entry)}</legend>
      <NameFields names={props.entry.names} disabled={props.disabled} onChange={props.onRename} />
      <Show when={"order_type" in props.entry ? (props.entry as OrderTypeEntry) : undefined}>
        {(typed) => (
          <TypeFields entry={typed()} disabled={props.disabled} onChange={props.onChange} onDefault={props.onDefault} />
        )}
      </Show>
      <SwitchField label={t("settings.orderChannels.active")} checked={props.entry.active !== false}
        onChange={(on) => props.onChange({ active: on })} onLabel={t("settings.orderChannels.offered")}
        offLabel={t("settings.orderChannels.retired")} disabled={props.disabled} />
      <Show when={!props.saved} fallback={<p class="text-xs text-ink-muted">{t("settings.orderChannels.keptHint")}</p>}>
        <div>
          <Button variant="danger-ghost" size="sm" disabled={props.disabled} onClick={props.onRemove}>
            {t("settings.orderChannels.remove")}
          </Button>
        </div>
      </Show>
    </fieldset>
  );
}

/** The two lists, edited as structure. `saved` is what this scope holds now. */
export function OrderChannelsEditor(props: {
  kind: string;
  value: Json | undefined;
  saved: Json | undefined;
  fallback: Json | undefined;
  disabled: boolean;
  onChange: (value: Json) => void;
}) {
  const types = () => props.kind === ORDER_TYPES_KIND;
  const entries = (): readonly Entry[] => readEntries<Entry>(props.value ?? props.fallback);
  const savedTokens = () => new Set(readEntries<Entry>(props.saved).map(tokenOf));
  const write = (next: readonly Entry[]) => props.onChange(toValue(next));
  const update = (index: number, change: Partial<OrderTypeEntry & SourceEntry>) =>
    write(entries().map((entry, at) => (at === index ? { ...entry, ...change } : entry)));
  const rename = (index: number, language: string, name: string) => {
    const entry = entries()[index];
    if (entry === undefined) return;
    const token = tokenOf(entry);
    // A new entry's token follows its English name until it is saved.
    const fresh = !savedTokens().has(token) && !builtIn(token) && language === "en";
    const field = types() ? "order_type" : "source";
    const retoken = fresh ? { [field]: tokenFrom(types() ? "ORDER_TYPE_" : "SALE_SOURCE_", name) } : {};
    update(index, { names: withName(entry.names, language, name), ...retoken });
  };
  const setDefault = (index: number, on: boolean) =>
    write(entries().map((other, at) => (at === index ? { ...other, default: on } : on ? { ...other, default: false } : other)));
  const add = () =>
    write([...entries(), types() ? { order_type: "ORDER_TYPE_", base_order_type: "ORDER_TYPE_TAKEAWAY" } : { source: "SALE_SOURCE_" }]);
  const fault = () =>
    types() ? typesFault(entries() as readonly OrderTypeEntry[]) : sourcesFault(entries() as readonly SourceEntry[]);
  return (
    <div class="flex flex-col gap-4" data-editor="order-channels">
      <p class="text-sm text-ink-muted">
        {types() ? t("settings.orderChannels.typesHint") : t("settings.orderChannels.sourcesHint")}
      </p>
      <For each={entries()}>
        {(entry, index) => (
          <EntryRow entry={entry} saved={savedTokens().has(tokenOf(entry))} disabled={props.disabled}
            onChange={(change) => update(index(), change)} onRename={(language, name) => rename(index(), language, name)}
            onDefault={(on) => setDefault(index(), on)} onRemove={() => write(entries().filter((_, at) => at !== index()))} />
        )}
      </For>
      <div>
        <Button variant="secondary" disabled={props.disabled} onClick={add} data-step="addOrderChannel">
          {types() ? t("settings.orderChannels.addType") : t("settings.orderChannels.addSource")}
        </Button>
      </div>
      <Show when={fault()}>
        {(found) => <p class="text-sm text-danger" role="alert">{faultText(found())}</p>}
      </Show>
    </div>
  );
}

/** The kinds of the two lists, which the settings screen offers this editor for. */
export const LIST_KINDS: readonly string[] = [ORDER_TYPES_KIND, SALE_SOURCES_KIND];

/** Whether a list value is one the register takes, as far as the console can tell. */
export function listTakes(kind: string, value: Json | undefined): boolean {
  if (!Array.isArray(value)) return false;
  if (kind === ORDER_TYPES_KIND) return typesFault(readEntries(value)) === null;
  return kind === SALE_SOURCES_KIND && sourcesFault(readEntries(value)) === null;
}

/** A list value's label, how many entries it holds; `undefined` for any other kind. */
export function listLabel(kind: string, value: Json): string | undefined {
  return LIST_KINDS.includes(kind) && Array.isArray(value) ? t("settings.orderChannels.count", { count: value.length }) : undefined;
}

/** The editor for a setting whose value is structure: receipt templates, or one of the two lists. */
export function StructuredEditor(props: {
  kind: string;
  tenantId: string;
  value: Json | undefined;
  saved: Json | undefined;
  fallback: Json | undefined;
  disabled: boolean;
  onChange: (value: Json) => void;
}) {
  return (
    <Show when={LIST_KINDS.includes(props.kind)}
      fallback={<ReceiptTemplatesEditor tenantId={props.tenantId} value={props.value} onChange={props.onChange} disabled={props.disabled} />}>
      <OrderChannelsEditor kind={props.kind} value={props.value} saved={props.saved} fallback={props.fallback}
        disabled={props.disabled} onChange={props.onChange} />
    </Show>
  );
}
