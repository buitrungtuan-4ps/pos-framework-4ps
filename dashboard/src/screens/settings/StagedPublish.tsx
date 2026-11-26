// Stage a change to one group of settings, then publish it to one place (ADR-0246 decision 3): a
// store, a store group, a brand or every store, rather than wherever each setting happens to be
// written today. Each value goes through the ordinary setting write when published, so it is checked
// against the register, reaches only the stores that place takes in, and is audited.

import { createMemo, createSignal, For, Show } from "solid-js";

import { api } from "../../api/client";
import type { SettingDefinition } from "../../api/schemas";
import type { Json, SettingScope } from "../../api/types";
import { t, tFromServer } from "../../i18n";
import { apiMessage } from "../../lib/errors";
import { createAdminResource } from "../../lib/resource";
import { type Staged, groupSettings, groupsOf, stageableValues, withStaged, writesFor } from "../../lib/staged-publish";
import { tenantId } from "../../state/session";
import { Banner, Button, Card, NumberField, SelectField } from "../../components/ui";
import { toast } from "../../components/Toast";

const UNSET = "";

function titleOf(key: string): string {
  return tFromServer(`settings.item.${key}.title`, key);
}

/** The field a staged value is typed in, by the setting's kind. */
function StageField(props: { setting: SettingDefinition; value: Json | undefined; onStage: (value: Json | undefined) => void }) {
  const label = titleOf(props.setting.setting_key);
  if (props.setting.kind === "SETTING_KIND_INT") {
    return (
      <NumberField
        label={label}
        value={typeof props.value === "number" ? props.value : null}
        min={props.setting.min}
        max={props.setting.max}
        onChange={(value) => props.onStage(value ?? undefined)}
      />
    );
  }
  const options =
    props.setting.kind === "SETTING_KIND_BOOL"
      ? [
          { value: "true", label: t("settings.on") },
          { value: "false", label: t("settings.off") },
        ]
      : stageableValues(props.setting).map((value) => ({ value, label: tFromServer(`settings.value.${value}`, value) }));
  const shown = props.value === undefined ? UNSET : String(props.value);
  const read = (value: string): Json | undefined =>
    value === UNSET ? undefined : props.setting.kind === "SETTING_KIND_BOOL" ? value === "true" : value;
  return (
    <SelectField
      label={label}
      value={shown}
      options={[{ value: UNSET, label: t("stagedPublish.unchanged") }, ...options]}
      onChange={(value) => props.onStage(read(value))}
    />
  );
}

/** Writes each staged value at `place` (`scope:id`); answers what was refused or skipped, in words. */
async function publishStaged(
  catalogue: readonly SettingDefinition[],
  staged: ReadonlyMap<string, Json>,
  place: string,
  places: ReadonlyMap<string, string>,
): Promise<string[]> {
  const [scope, scopeId] = place.split(":");
  if (!scope || !scopeId) return [];
  const list: Staged[] = [...staged].map(([key, value]) => ({ key, value }));
  const { writes, skipped } = writesFor(catalogue, list, { scope: scope as SettingScope, scopeId });
  const failed = skipped.map((key) => t("stagedPublish.notAtScope", { setting: titleOf(key) }));
  let written = 0;
  for (const write of writes) {
    try {
      await api.putSetting(tenantId(), write.key, scope as SettingScope, scopeId, write.value);
      written += 1;
    } catch (caught) {
      failed.push(`${titleOf(write.key)}: ${apiMessage(caught)}`);
    }
  }
  toast.ok(t("stagedPublish.published", { count: written, where: places.get(place) ?? scopeId }));
  return failed;
}

/** The card: a group, its staged values, a place, and Publish. `places` maps `scope:id` to a name. */
export function StagedPublish(props: { places: ReadonlyMap<string, string> }) {
  const catalogue = createAdminResource(async () => api.settingsCatalogue(), { scope: "tenant" });
  const [group, setGroup] = createSignal("");
  const [staged, setStaged] = createSignal<ReadonlyMap<string, Json>>(new Map());
  const [place, setPlace] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [refused, setRefused] = createSignal<string[]>([]);
  const settings = createMemo(() => groupSettings(catalogue.value() ?? [], group()));
  const stage = (key: string, value: Json | undefined) => setStaged(withStaged(staged(), key, value));
  const publish = async () => {
    setBusy(true);
    const failed = await publishStaged(catalogue.value() ?? [], staged(), place(), props.places);
    setBusy(false);
    setRefused(failed);
    if (failed.length === 0) setStaged(new Map());
  };
  return (
    <Card title={t("stagedPublish.title")}>
      <div class="grid max-w-xl gap-3" data-testid="staged-publish">
        <p class="text-sm text-ink-muted">{t("stagedPublish.hint")}</p>
        <SelectField
          label={t("stagedPublish.group")}
          value={group()}
          options={[{ value: "", label: t("stagedPublish.chooseGroup") }, ...groupsOf(catalogue.value() ?? []).map((node) => ({ value: node, label: node }))]}
          onChange={(value) => (setGroup(value), setStaged(new Map()))}
        />
        <For each={settings()}>
          {(setting) => <StageField setting={setting} value={staged().get(setting.setting_key)} onStage={(value) => stage(setting.setting_key, value)} />}
        </For>
        <SelectField
          label={t("stagedPublish.where")}
          value={place()}
          options={[{ value: "", label: t("stagedPublish.chooseWhere") }, ...[...props.places].map(([value, label]) => ({ value, label }))]}
          onChange={setPlace}
        />
        <Show when={refused().length > 0}>
          <Banner tone="danger" message={t("settingsHistory.refused", { list: refused().join("; ") })} />
        </Show>
        <div>
          <Button onClick={() => void publish()} disabled={busy() || staged().size === 0 || place() === ""}>
            {t("stagedPublish.publish", { count: staged().size })}
          </Button>
        </div>
      </div>
    </Card>
  );
}
