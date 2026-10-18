// Whose a drawer is, where the store assigns drawers
// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 9): the
// signed-in person's own, or another person's, never by name. On a drawer that is another
// person's, and where `approver` asks, the override approver's code and PIN beneath, which the acts
// on it then carry (`state/drawer-access.ts`). `own` is this till's own drawer while its shift is
// open or counted.

import { Show, createSignal, onCleanup } from "solid-js";

import { t } from "../i18n";
import {
  drawersAssigned,
  forgetOverride,
  overrideAsks,
  overrideCode,
  overridePin,
  setOverrideCode,
  setOverridePin,
  somebodyElses,
} from "../state/drawer-access";
import { state } from "../state/store";
import { ApproverFields } from "./ApproverFields";
import { CodePad } from "./CodePad";

export function DrawerOwner(props: { assignee?: string; own?: boolean; approver?: boolean }) {
  const assignee = () => {
    if (props.own !== true) {
      return props.assignee;
    }
    const shift = state.shift;
    return shift !== null && shift.state !== "SHIFT_STATE_CLOSED" ? shift.assignee : undefined;
  };
  onCleanup(() => {
    if (props.approver === true) {
      forgetOverride();
    }
  });
  return (
    <Show when={drawersAssigned() && assignee() !== undefined}>
      <span class="mt-1 block text-sm" data-drawer-owner={somebodyElses(assignee()) ? "other" : "own"}>
        {somebodyElses(assignee()) ? t("shift.drawer_owner_other") : t("shift.drawer_owner_own")}
      </span>
      <Show when={props.approver === true && overrideAsks(assignee())}>
        <p class="mt-1 text-sm text-ink-muted">{t("shift.override_hint")}</p>
        <ApproverFields
          id="override-approver"
          code={overrideCode()}
          pin={overridePin()}
          onCode={setOverrideCode}
          onPin={setOverridePin}
          codeLabel={t("shift.approver_code")}
          pinLabel={t("shift.approver_pin")}
        />
      </Show>
    </Show>
  );
}

// The staff code of the person a drawer started for another till is for, where the store assigns
// drawers: typed, as an approver's code is, because the till keeps no names to pick from.
export function AssigneeField(props: { value: string; onChange: (next: string) => void }) {
  const [filling, setFilling] = createSignal(false);
  return (
    <Show when={drawersAssigned()}>
      <label class="mt-2 block text-sm" for="drawer-assignee">
        {t("shift.assignee_code")}
      </label>
      <input
        id="drawer-assignee"
        type="text"
        autocomplete="off"
        class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
        value={props.value}
        onFocus={() => setFilling(true)}
        onInput={(event) => props.onChange(event.currentTarget.value)}
      />
      <Show when={filling()}>
        <CodePad
          id="drawer-assignee-pad"
          value={props.value}
          onChange={props.onChange}
          mode="text"
          label={t("approver.pad_code")}
        />
      </Show>
    </Show>
  );
}
