// The data designer (ADR-0194): the custom fields a package adds to the objects a store has, the
// lists a choice field draws from, and object types of HQ's own with their fields. A part of the
// package designer (`designer/parts.tsx`): it edits the draft's `data` and shows the cloud's
// refusals of it, including one that removes or retypes what an earlier version declared.

import { createSignal, For, Index, type JSX } from "solid-js";

import type { CustomField, CustomList, PackageData } from "../../../api/schemas";
import { Button, Card, TextField } from "../../../components/ui";
import { t, type MessageKey } from "../../../i18n";
import { newField, newList, newObjectType, type ObjectType } from "../../../lib/data-draft";
import { idFrom } from "../../../lib/flow-graph";
import { newUlid } from "../../../lib/ulid";
import type { PartProps } from "../designer/parts";
import { RefusalList } from "../designer/refusals";
import { FieldEditor } from "./FieldEditor";

/** A name box and an add button, which add a thing with an id made from the name. */
function AddByName(props: { label: MessageKey; button: MessageKey; onAdd: (name: string) => void }) {
  const [name, setName] = createSignal("");
  const add = () => {
    if (name().trim() !== "") {
      props.onAdd(name().trim());
      setName("");
    }
  };
  return (
    <div class="flex items-end gap-2">
      <TextField label={t(props.label)} value={name()} onInput={setName} />
      <Button variant="secondary" onClick={add}>{t(props.button)}</Button>
    </div>
  );
}

/** Fields, each in its editor, and a way to add one. */
function Fields(props: { fields: readonly CustomField[]; lists: readonly CustomList[]; object?: string; onChange: (fields: CustomField[]) => void }) {
  const names = () => props.fields.map((field) => field.label_key.replace(/^field\./u, ""));
  return (
    <div class="flex flex-col gap-2">
      <Index each={props.fields}>
        {(field, index) => (
          <FieldEditor
            field={field()}
            lists={props.lists}
            withObject={props.object !== undefined}
            onChange={(next) => props.onChange(props.fields.map((old, at) => (at === index ? next : old)))}
            onRemove={() => props.onChange(props.fields.filter((_, at) => at !== index))}
          />
        )}
      </Index>
      <AddByName label="data.newField" button="data.addField" onAdd={(name) => props.onChange([...props.fields, newField(newUlid(), idFrom(name, names(), "field"), props.object)])} />
    </div>
  );
}

type Entry = CustomList["entries"][number];

/**
 * `list` with `entry` replaced by `next`, or taken out when `next` is `null` (O9). Removing is for an
 * entry no published version declares; the cloud refuses removing one that is, and such an entry is
 * retired instead, kept for reading and no longer offered.
 */
export function withEntry(list: CustomList, entry: Entry, next: Entry | null): CustomList {
  const entries = list.entries.flatMap((held) => (held.list_entry_id === entry.list_entry_id ? (next ? [next] : []) : [held]));
  return { ...list, entries };
}

/** One entry: its key, Retire or Restore, and Remove. */
function EntryChip(props: { entry: Entry; onChange: (next: Entry | null) => void }) {
  const retired = () => props.entry.deprecated === true;
  return (
    <li class="flex items-center gap-1 rounded-token border border-line px-2" data-list-entry={props.entry.list_entry_id}>
      <span class={retired() ? "line-through text-ink-muted" : ""}>{props.entry.label_key}</span>
      <Button variant="ghost" size="sm" onClick={() => props.onChange({ ...props.entry, deprecated: !retired() })}>
        {retired() ? t("data.restoreEntry") : t("data.retireEntry")}
      </Button>
      <Button variant="danger-ghost" size="sm" onClick={() => props.onChange(null)}>{t("data.remove")}</Button>
    </li>
  );
}

/** A list and its entries. */
function ListEditor(props: { list: CustomList; onChange: (list: CustomList) => void }) {
  const names = () => props.list.entries.map((entry) => entry.label_key.slice(props.list.label_key.length + 1));
  return (
    <div class="flex flex-col gap-2 rounded-token border border-line p-3" data-custom-list={props.list.custom_list_id}>
      <TextField label={t("data.labelKey")} hint={t("data.keyHint")} value={props.list.label_key} onInput={(label_key) => props.onChange({ ...props.list, label_key })} />
      <ul class="flex flex-wrap gap-2 text-sm">
        <For each={props.list.entries}>{(entry) => <EntryChip entry={entry} onChange={(next) => props.onChange(withEntry(props.list, entry, next))} />}</For>
      </ul>
      <AddByName
        label="data.newEntry"
        button="data.addEntry"
        onAdd={(name) => {
          const entry = idFrom(name, names(), "entry");
          props.onChange({ ...props.list, entries: [...props.list.entries, { list_entry_id: newUlid(), label_key: `${props.list.label_key}.${entry}` }] });
        }}
      />
    </div>
  );
}

/** One section of the panel. */
const Section = (props: { title: string; children: JSX.Element }) => (
  <Card title={props.title}>
    <div class="flex flex-col gap-3">{props.children}</div>
  </Card>
);

/** The panel: fields on existing objects, lists, object types, and the refusals of the data part. */
export function DataPanel(props: PartProps) {
  const data = (): PackageData => props.draft().data ?? {};
  const set = (change: Partial<PackageData>) => props.change((draft) => ({ ...draft, data: { ...(draft.data ?? {}), ...change } }));
  const lists = () => data().lists ?? [];
  const types = () => data().object_types ?? [];
  const setType = (index: number, next: ObjectType) => set({ object_types: types().map((old, at) => (at === index ? next : old)) });
  return (
    <div class="flex flex-col gap-4">
      <p class="text-sm text-ink-muted">{t("data.intro")}</p>
      <Section title={t("data.onObjects")}>
        <Fields fields={data().custom_fields ?? []} lists={lists()} object="DATA_OBJECT_ORDER" onChange={(custom_fields) => set({ custom_fields })} />
      </Section>
      <Section title={t("data.lists")}>
        <Index each={lists()}>
          {(list, index) => <ListEditor list={list()} onChange={(next) => set({ lists: lists().map((old, at) => (at === index ? next : old)) })} />}
        </Index>
        <AddByName label="data.newList" button="data.addList" onAdd={(name) => set({ lists: [...lists(), newList(newUlid(), idFrom(name, lists().map((list) => list.label_key.replace(/^list\./u, "")), "list"))] })} />
      </Section>
      <Section title={t("data.objectTypes")}>
        <Index each={types()}>
          {(type, index) => (
            <div class="flex flex-col gap-2" data-object-type={type().object_type_id}>
              <p class="font-medium text-ink">{type().label_key}</p>
              <TextField label={t("data.labelKey")} hint={t("data.keyHint")} value={type().label_key} onInput={(label_key) => setType(index, { ...type(), label_key })} />
              <Fields fields={type().fields} lists={lists()} onChange={(fields) => setType(index, { ...type(), fields })} />
            </div>
          )}
        </Index>
        <AddByName label="data.newObjectType" button="data.addObjectType" onAdd={(name) => set({ object_types: [...types(), newObjectType(newUlid(), idFrom(name, types().map((type) => type.label_key.replace(/^object\./u, "")), "object"))] })} />
      </Section>
      <RefusalList refusals={props.refusals} draft={props.draft()} />
    </div>
  );
}
