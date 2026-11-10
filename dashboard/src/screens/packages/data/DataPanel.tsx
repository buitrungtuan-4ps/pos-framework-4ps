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
  const ids = () => props.fields.map((field) => field.custom_field_id);
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
      <AddByName label="data.newField" button="data.addField" onAdd={(name) => props.onChange([...props.fields, newField(idFrom(name, ids(), "field"), props.object)])} />
    </div>
  );
}

/** A list and its entries. */
function ListEditor(props: { list: CustomList; onChange: (list: CustomList) => void }) {
  const ids = () => props.list.entries.map((entry) => entry.list_entry_id);
  return (
    <div class="flex flex-col gap-2 rounded-token border border-line p-3" data-custom-list={props.list.custom_list_id}>
      <TextField label={t("data.labelKey")} hint={t("data.keyHint")} value={props.list.label_key} onInput={(label_key) => props.onChange({ ...props.list, label_key })} />
      <ul class="flex flex-wrap gap-2 text-sm">
        <For each={props.list.entries}>{(entry) => <li class="rounded-token border border-line px-2">{entry.list_entry_id}</li>}</For>
      </ul>
      <AddByName
        label="data.newEntry"
        button="data.addEntry"
        onAdd={(name) => {
          const list_entry_id = idFrom(name, ids(), "entry");
          props.onChange({ ...props.list, entries: [...props.list.entries, { list_entry_id, label_key: `${props.list.label_key}.${list_entry_id}` }] });
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
        <AddByName label="data.newList" button="data.addList" onAdd={(name) => set({ lists: [...lists(), newList(idFrom(name, lists().map((list) => list.custom_list_id), "list"))] })} />
      </Section>
      <Section title={t("data.objectTypes")}>
        <Index each={types()}>
          {(type, index) => (
            <div class="flex flex-col gap-2" data-object-type={type().object_type_id}>
              <p class="font-medium text-ink">{type().object_type_id}</p>
              <TextField label={t("data.labelKey")} hint={t("data.keyHint")} value={type().label_key} onInput={(label_key) => setType(index, { ...type(), label_key })} />
              <Fields fields={type().fields} lists={lists()} onChange={(fields) => setType(index, { ...type(), fields })} />
            </div>
          )}
        </Index>
        <AddByName label="data.newObjectType" button="data.addObjectType" onAdd={(name) => set({ object_types: [...types(), newObjectType(idFrom(name, types().map((type) => type.object_type_id), "object"))] })} />
      </Section>
      <RefusalList refusals={props.refusals} draft={props.draft()} />
    </div>
  );
}
