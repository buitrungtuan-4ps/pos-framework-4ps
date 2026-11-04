// Letting each store choose its package version
// ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 2): the
// versions a store may choose from, and the one it runs until it chooses. Saved as the tenant's pin,
// in the order `lib/package-pin.ts` gives, which republishes every store.

import { createEffect, createSignal, For, Show } from "solid-js";

import type { PackageVersionRecord } from "../../api/schemas";
import { t } from "../../i18n";
import { choiceGap, type PackagePin, type PinRequest } from "../../lib/package-pin";
import { Banner, Button, Card, SelectField } from "../../components/ui";

export function ChoiceCard(props: {
  versions: readonly PackageVersionRecord[];
  current: PackagePin;
  busy: boolean;
  onSave: (request: PinRequest) => void;
}) {
  const [allowed, setAllowed] = createSignal<readonly string[]>([]);
  const [fallback, setFallback] = createSignal("");
  const [gap, setGap] = createSignal("");
  // Starts from the pin as it stands, so a store's choices are edited rather than retyped.
  createEffect(() => {
    if (props.current.mode === "PACKAGE_MODE_CHOOSE") {
      setAllowed(props.current.allowedVersionIds);
      setFallback(props.current.defaultVersionId ?? "");
    }
  });

  const allowVersion = (versionId: string, on: boolean) => {
    const next = on
      ? [...allowed(), versionId]
      : allowed().filter((allowedId) => allowedId !== versionId);
    setAllowed(next);
    // The first version allowed is the default until the operator says otherwise, so the common
    // case — a choice with one obvious starting point — needs no second control.
    if (!next.includes(fallback())) {
      setFallback(next[0] ?? "");
    }
  };

  const saveChoice = () => {
    const refused = choiceGap(allowed(), fallback());
    setGap(refused === null ? "" : t(refused));
    if (refused === null) {
      props.onSave({
        mode: "PACKAGE_MODE_CHOOSE",
        allowedVersionIds: allowed(),
        defaultVersionId: fallback(),
      });
    }
  };

  const labelOf = (versionId: string) =>
    props.versions.find((v) => v.content.package_version_id === versionId)?.content
      .version_label ?? versionId;

  return (
    <Card title={t("packages.choiceTitle")}>
      <p class="mb-3 text-sm text-ink-muted">{t("packages.choiceHint")}</p>
      <Show when={gap()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <div class="flex flex-wrap gap-2" role="group" aria-label={t("packages.choiceAllowed")}>
        <For each={props.versions}>
          {(version) => {
            const on = () => allowed().includes(version.content.package_version_id);
            return (
              <Button
                variant={on() ? "primary" : "secondary"}
                aria-pressed={on()}
                onClick={() => allowVersion(version.content.package_version_id, !on())}
                data-step="allowVersion"
              >
                {version.content.version_label}
              </Button>
            );
          }}
        </For>
      </div>
      <div class="mt-3 flex flex-wrap items-end gap-3">
        <SelectField
          label={t("packages.choiceDefaultLabel")}
          value={fallback()}
          options={allowed().map((versionId) => ({ value: versionId, label: labelOf(versionId) }))}
          onChange={setFallback}
        />
        <Button disabled={props.busy} onClick={saveChoice} data-step="saveChoice">
          {t("packages.choiceSave")}
        </Button>
      </div>
      <Show when={props.current.mode === "PACKAGE_MODE_CHOOSE"}>
        <p class="mt-3 text-sm text-ink" data-outcome="package-choosing">
          {t("packages.choiceSaved", { count: props.current.allowedVersionIds.length })}
        </p>
      </Show>
    </Card>
  );
}
