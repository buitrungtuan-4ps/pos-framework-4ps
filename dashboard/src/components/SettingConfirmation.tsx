// A value that needs a recorded confirmation, on the Settings screen
// ([ADR-0169](../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)
// decision 4).
//
// A setting whose catalogue entry carries `confirm` names one value that records something about a
// person. Picking it draws a checkbox with the notice, from the console's translation of
// `confirm.notice`, and a line saying who and when are recorded and which scope the confirmation
// covers. Save stays disabled, and a line says why, until the box is ticked; the write then carries
// `notice_confirmed`. Nothing here names a setting: a register entry with a `confirm` and its
// notice's translations is all a new one needs.

import { type Accessor, createEffect, createSignal, on, type Setter, Show } from "solid-js";

import type { Json, SettingDefinition } from "../api/types";
import { t, tFromServer } from "../i18n";
import { CheckboxField } from "./ui";

/** Whether the notice is ticked for the value picked now: a new pick, or none, unticks it. */
export function createTick(picked: () => Json | undefined): [Accessor<boolean>, Setter<boolean>] {
  const [ticked, setTicked] = createSignal(false);
  createEffect(on(picked, () => setTicked(false), { defer: true }));
  return [ticked, setTicked];
}

/** Whether `value` is the one the setting writes only with a recorded confirmation. */
export function needsConfirming(setting: SettingDefinition, value: Json | undefined): boolean {
  return setting.confirm !== undefined && value === setting.confirm.value;
}

/**
 * The `noticeConfirmed` argument a write of `value` carries: `[true]` for the value that needs a
 * confirmation, once ticked, and nothing for any other.
 */
export function confirmation(
  setting: SettingDefinition,
  value: Json,
  ticked: boolean,
): readonly [] | readonly [true] {
  return ticked && needsConfirming(setting, value) ? [true] : [];
}

/** Whether a value picked to save still waits for its notice to be ticked. */
export function awaitingConfirmation(
  setting: SettingDefinition,
  picked: Json | undefined,
  ticked: boolean,
): boolean {
  return needsConfirming(setting, picked) && !ticked;
}

/**
 * The notice to tick before `picked` can be saved, and the line that says why Save waits: drawn only
 * for a changed value that needs a confirmation. `covers` names the scope being written.
 */
export function ConfirmNotice(props: {
  setting: SettingDefinition;
  picked: Json | undefined;
  changed: boolean;
  ticked: boolean;
  onTick: (ticked: boolean) => void;
  covers: string;
  disabled: boolean;
}) {
  const notice = () => props.setting.confirm?.notice ?? "";
  return (
    <Show when={props.changed && needsConfirming(props.setting, props.picked)}>
      <CheckboxField
        label={tFromServer(notice(), notice())}
        checked={props.ticked}
        onChange={(ticked) => props.onTick(ticked)}
        disabled={props.disabled}
        hint={t("settings.confirmCovers", { target: props.covers })}
      />
      <Show when={!props.ticked}>
        <p class="text-sm text-ink-muted">{t("settings.confirmFirst")}</p>
      </Show>
    </Show>
  );
}
