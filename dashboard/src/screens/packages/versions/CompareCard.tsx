// Two versions of a package, compared before one is pinned or a store is rolled back to it
// (ADR-0189 decisions 1 and 6): which screens, flows, reactions, time slots, custom fields, lists
// and object types the later one adds, removes or changes, and how many acts each offers over its
// tap budget (ADR-0210 decision 5). A published version never changes, so the comparison is of what
// the stores would run, part by part.

import { createSignal, For, Show } from "solid-js";

import type { PackageVersionRecord } from "../../../api/schemas";
import { Card, SelectField } from "../../../components/ui";
import { t, type MessageKey } from "../../../i18n";
import { type Change, diffVersions, PART_LABEL } from "../../../lib/package-diff";

const CHANGE_LABEL: Readonly<Record<Change, MessageKey>> = {
  added: "packageDiff.added",
  removed: "packageDiff.removed",
  changed: "packageDiff.changed",
};

/** Compares two of `versions`: by default the one before the newest, against the newest. */
export function CompareCard(props: { versions: readonly PackageVersionRecord[] }) {
  const id = (version: PackageVersionRecord | undefined) => version?.content.package_version_id ?? "";
  const [from, setFrom] = createSignal(id(props.versions[props.versions.length - 2]));
  const [to, setTo] = createSignal(id(props.versions[props.versions.length - 1]));
  const options = () =>
    props.versions.map((version) => ({ value: id(version), label: version.content.version_label }));
  const at = (versionId: string) => props.versions.find((version) => id(version) === versionId);
  const entries = () => {
    const earlier = at(from());
    const later = at(to());
    return earlier === undefined || later === undefined ? null : diffVersions(earlier.content, later.content);
  };
  return (
    <Card title={t("packageDiff.title")}>
      <div class="flex flex-col gap-3">
        <div class="grid gap-2 sm:grid-cols-2">
          <SelectField label={t("packageDiff.from")} value={from()} options={options()} onChange={setFrom} />
          <SelectField label={t("packageDiff.to")} value={to()} options={options()} onChange={setTo} />
        </div>
        <Show when={at(from()) !== undefined && at(to()) !== undefined}>
          <p class="text-sm text-ink-muted" data-outcome="tap-warning-counts">
            {t("packageDiff.tapWarnings", { from: at(from())?.tap_warning_count ?? 0, to: at(to())?.tap_warning_count ?? 0 })}
          </p>
        </Show>
        <Show when={entries()}>
          {(found) => (
            <Show when={found().length > 0} fallback={<p class="text-sm text-ink-muted">{t("packageDiff.same")}</p>}>
              <ul class="flex flex-col gap-1 text-sm" aria-label={t("packageDiff.title")}>
                <For each={found()}>
                  {(entry) => (
                    <li data-change={entry.change}>
                      {t(CHANGE_LABEL[entry.change], { part: t(PART_LABEL[entry.part]), id: entry.id })}
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          )}
        </Show>
      </div>
    </Card>
  );
}
