// The selected node's properties, each edited by its kind (ADR-0203 decision 2): a translation key
// typed (with the tenant's keys offered), a token chosen, a number within its bounds, a switch, or
// an act chosen with the parameters the package fixes. A property is always a literal, so nobody
// writes JSON. The keyboard twins of dragging (move up, move down, remove) sit here too.

import { For, Show } from "solid-js";

import { Button, CheckboxField, NumberField, SelectField, TextField } from "../../../components/ui";
import { t } from "../../../i18n";
import { ACTIONS, type ActionParam } from "../../../../../packages/package-renderer/src/actions";
import type { ActionBinding, ComponentProperty, ParamValue, PropertyValue, ScreenNode } from "../../../../../packages/package-renderer/src/components";
import { componentOf } from "../../../lib/screen-tree";
import { actionName, propertyName, tokenName } from "./names";

type InspectorProps = {
  readonly node: ScreenNode;
  /** The translation keys the tenant's grid holds, offered for a key. */
  readonly keys: readonly string[];
  /** The draft's screen ids, offered for an act's `screen_id`. */
  readonly screenIds: readonly string[];
  readonly isRoot: boolean;
  readonly onSet: (name: string, value: PropertyValue | undefined) => void;
  readonly onMove: (by: -1 | 1) => void;
  readonly onRemove: () => void;
};

/** A parameter a package may fix: never an amount or something a person enters (ADR-0191). */
function fixable(param: ActionParam): boolean {
  return param.kind !== "ACTION_PARAM_KIND_AMOUNT" && param.kind !== "ACTION_PARAM_KIND_ENTERED";
}

/** An act and the parameters the package fixes for it. */
function ActionEditor(props: {
  label: string;
  value: ActionBinding | undefined;
  supplies: readonly string[];
  screenIds: readonly string[];
  onChange: (value: ActionBinding | undefined) => void;
}) {
  const action = () => ACTIONS.find((entry) => entry.action_id === props.value?.action_id);
  const setParam = (name: string, value: ParamValue | undefined) => {
    const current = props.value;
    if (!current) {
      return;
    }
    const params = { ...(current.params ?? {}) };
    if (value === undefined || value === "") {
      delete params[name];
    } else {
      params[name] = value;
    }
    props.onChange({ action_id: current.action_id, ...(Object.keys(params).length > 0 ? { params } : {}) });
  };
  return (
    <div class="flex flex-col gap-2">
      <SelectField
        label={props.label}
        value={props.value?.action_id ?? ""}
        placeholder={t("designer.screens.noAction")}
        options={ACTIONS.map((entry) => ({ value: entry.action_id, label: actionName(entry.action_id) }))}
        onChange={(actionId) => props.onChange(actionId === "" ? undefined : { action_id: actionId })}
      />
      <For each={action()?.params ?? []}>
        {(param) => (
          <Show
            when={!props.supplies.includes(param.name) && fixable(param)}
            fallback={
              <p class="text-sm text-ink-muted">
                {t(props.supplies.includes(param.name) ? "designer.screens.paramSupplied" : "designer.screens.paramEntered", {
                  name: propertyName(param.name),
                })}
              </p>
            }
          >
            <Show
              when={param.name === "screen_id"}
              fallback={
                <Show
                  when={param.kind === "ACTION_PARAM_KIND_COUNT"}
                  fallback={
                    <TextField
                      label={propertyName(param.name)}
                      value={String(props.value?.params?.[param.name] ?? "")}
                      hint={param.kind === "ACTION_PARAM_KIND_ID_LIST" ? t("designer.screens.listHint") : undefined}
                      onInput={(text) =>
                        setParam(
                          param.name,
                          param.kind === "ACTION_PARAM_KIND_ID_LIST"
                            ? text.split(",").map((part) => part.trim()).filter((part) => part !== "")
                            : text.trim(),
                        )
                      }
                    />
                  }
                >
                  <NumberField
                    label={propertyName(param.name)}
                    value={typeof props.value?.params?.[param.name] === "number" ? (props.value.params[param.name] as number) : null}
                    onChange={(count) => setParam(param.name, count ?? undefined)}
                  />
                </Show>
              }
            >
              <SelectField
                label={propertyName(param.name)}
                value={String(props.value?.params?.[param.name] ?? "")}
                placeholder={t("designer.screens.pickScreen")}
                options={props.screenIds.map((id) => ({ value: id, label: id }))}
                onChange={(id) => setParam(param.name, id)}
              />
            </Show>
          </Show>
        )}
      </For>
    </div>
  );
}

/** One property's editor, by its kind. */
function PropertyEditor(props: { property: ComponentProperty } & Omit<InspectorProps, "onMove" | "onRemove" | "isRoot">) {
  const name = () => props.property.name;
  const value = () => props.node.properties?.[name()];
  const label = () => (props.property.required ? t("designer.screens.required", { name: propertyName(name()) }) : propertyName(name()));
  const supplies = () => componentOf(props.node.component_id)?.supplies ?? [];
  switch (props.property.kind) {
    case "PROPERTY_KIND_TRANSLATION_KEY":
      return (
        <TextField
          label={label()}
          value={typeof value() === "string" ? (value() as string) : ""}
          hint={t("designer.screens.keyHint")}
          suggestions={props.keys.map((key) => ({ value: key }))}
          onInput={(text) => props.onSet(name(), text.trim() === "" ? undefined : text.trim())}
        />
      );
    case "PROPERTY_KIND_TOKEN":
      return (
        <SelectField
          label={label()}
          value={typeof value() === "string" ? (value() as string) : ""}
          placeholder={t("designer.screens.defaultToken")}
          options={props.property.tokens.map((token) => ({ value: token, label: tokenName(token) }))}
          onChange={(token) => props.onSet(name(), token === "" ? undefined : token)}
        />
      );
    case "PROPERTY_KIND_INT":
      return (
        <NumberField
          label={label()}
          value={typeof value() === "number" ? (value() as number) : null}
          min={props.property.min}
          max={props.property.max}
          onChange={(number) =>
            props.onSet(name(), number === null ? undefined : Math.max(props.property.min, Math.min(props.property.max, number)))
          }
        />
      );
    case "PROPERTY_KIND_BOOL":
      return <CheckboxField label={label()} checked={value() === true} onChange={(on) => props.onSet(name(), on)} />;
    case "PROPERTY_KIND_ACTION":
      return (
        <ActionEditor
          label={label()}
          value={typeof value() === "object" ? (value() as ActionBinding) : undefined}
          supplies={supplies()}
          screenIds={props.screenIds}
          onChange={(binding) => props.onSet(name(), binding)}
        />
      );
    default:
      return null;
  }
}

/** The selected node: its properties and the moves a keyboard makes. */
export function Inspector(props: InspectorProps) {
  const properties = () => componentOf(props.node.component_id)?.properties ?? [];
  return (
    <div class="flex flex-col gap-3">
      <Show when={properties().length > 0} fallback={<p class="text-sm text-ink-muted">{t("designer.screens.noProperties")}</p>}>
        <For each={properties()}>{(property) => <PropertyEditor {...props} property={property} />}</For>
      </Show>
      <Show when={!props.isRoot}>
        <div class="flex flex-wrap gap-2">
          <Button size="sm" variant="secondary" onClick={() => props.onMove(-1)}>
            {t("designer.screens.moveUp")}
          </Button>
          <Button size="sm" variant="secondary" onClick={() => props.onMove(1)}>
            {t("designer.screens.moveDown")}
          </Button>
          <Button size="sm" variant="danger-ghost" onClick={() => props.onRemove()}>
            {t("designer.screens.remove")}
          </Button>
        </div>
      </Show>
    </div>
  );
}
