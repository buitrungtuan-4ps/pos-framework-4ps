// A till's device class on the Devices screen's Terminals card (ADR-0201 decision 3): which class
// of device the till bound to a terminal is, so it draws a package screen's layout for that class
// instead of reading one from its screen. Kept out of `../Devices.tsx`, which is at its size
// baseline; the card mounts the column, the action and the form this hands it.
//
// Saving is conditional on the version the terminal was read at, as its float is (ADR-0094), and
// the store hears it on the next publish. An empty choice is "read it from the screen".

import { createSignal, type JSX } from "solid-js";

import { api } from "../../api/client";
import type { DeviceProposalSummary } from "../../api/schemas";
import { type Column, FormPanel } from "../../components/kit";
import { toast } from "../../components/Toast";
import { Button, SelectField } from "../../components/ui";
import { t } from "../../i18n";
import { useEntityCrud } from "../../lib/entity-crud";
import { DEVICE_CLASSES } from "../../lib/screen-tree";
import { tenantId } from "../../state/session";
import { deviceName } from "../packages/screens/names";

/** What the Terminals card needs from the screen: its conditional write, and its re-read. */
export interface TerminalClassHost {
  readonly conditional: <T>(write: () => Promise<T>) => Promise<T>;
  readonly reload: () => Promise<void>;
}

/** A till's class, the screen's where nobody said. */
export function deviceClassLabel(token: string | null): string {
  return token === null ? t("devices.deviceClassScreen") : deviceName(token);
}

/** The column, the row's action and the form for a till's device class. */
export function createTerminalClass(host: TerminalClassHost) {
  const draft = useEntityCrud<DeviceProposalSummary>();
  const [choice, setChoice] = createSignal("");
  const save = () => {
    const terminal = draft.subject();
    if (!terminal) {
      return;
    }
    const chosen = choice() === "" ? null : choice();
    void draft
      .run(() => host.conditional(() => api.setTerminalDeviceClass(tenantId(), terminal.id, chosen, terminal.version)))
      .then((saved) => {
        if (saved) {
          toast.ok(t("devices.deviceClassSaved"));
          void host.reload();
        }
      });
  };
  const column: Column<DeviceProposalSummary> = {
    key: "deviceClass",
    header: t("devices.deviceClass"),
    cell: (row) => (
      <span class={row.device_class === null ? "text-ink-muted" : "text-ink"}>{deviceClassLabel(row.device_class)}</span>
    ),
  };
  const action = (row: DeviceProposalSummary): JSX.Element => (
    <Button
      variant="secondary"
      onClick={() => {
        setChoice(row.device_class ?? "");
        draft.edit(row);
      }}
    >
      {t("devices.chooseDeviceClass")}
    </Button>
  );
  const Panel = (): JSX.Element => (
    <FormPanel
      crud={draft}
      createTitle={t("devices.deviceClassTitle")}
      editTitle={t("devices.deviceClassTitle")}
      submitLabel={t("action.save")}
      onSubmit={save}
      as="modal"
    >
      <p class="text-sm text-ink-muted">{t("devices.deviceClassHint")}</p>
      <SelectField
        label={t("devices.deviceClass")}
        value={choice()}
        options={DEVICE_CLASSES.map((token) => ({ value: token, label: deviceName(token) }))}
        onChange={setChoice}
        placeholder={t("devices.deviceClassScreen")}
      />
    </FormPanel>
  );
  return { column, action, Panel };
}
