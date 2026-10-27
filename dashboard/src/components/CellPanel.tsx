// Which cell this cloud is (ADR-0179 C1): the `[cell]` section of cloud.toml, read-only.
//
// A small panel rather than a screen: there is nothing to do here, only something to know. An
// operator with consoles for two cells open needs to see which one this tab is, and a cell whose
// servers run outside its home country should say so where the operator looks — as a warning, never
// a refusal (ADR-0179). The fleet screen is where it sits, beside the system health, because
// both describe the deployment rather than a tenant.

import { For, Show } from "solid-js";

import { type CellInfo, getCell } from "../api/cell";
import { t, type MessageKey } from "../i18n";
import { createAdminResource, failureOf } from "../lib/resource";
import { Banner, Card, Skeleton, StatusBadge } from "./ui";

/** The rows the panel shows, in order, each a label and the field it reads. */
const ROWS: readonly { readonly key: MessageKey; readonly value: (cell: CellInfo) => string | null }[] =
  [
    { key: "cell.cellId", value: (cell) => cell.cell_id },
    { key: "cell.countryAlias", value: (cell) => cell.country_alias },
    { key: "cell.homeCountry", value: (cell) => cell.home_country },
    { key: "cell.runsIn", value: (cell) => cell.runs_in },
    { key: "cell.baseUrl", value: (cell) => cell.base_url },
  ];

export function CellPanel() {
  // The cell is fixed when the server starts, so one read is the whole story: no interval.
  const cell = createAdminResource(() => getCell());
  return (
    <Card
      title={t("cell.title")}
      actions={
        <Show when={cell.value()}>
          {(info) => (
            <StatusBadge
              tone="neutral"
              label={info().configured ? info().cell_id : t("cell.single")}
            />
          )}
        </Show>
      }
    >
      <Show when={failureOf(cell)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <Show when={cell.value()} fallback={<Skeleton label={t("common.loading")} rows={2} />}>
        {(info) => (
          <div class="flex flex-col gap-3">
            <Show when={!info().configured}>
              <p class="text-sm text-ink-muted">{t("cell.singleHint")}</p>
            </Show>
            <Show when={info().runs_abroad}>
              <p role="status" class="rounded-token border border-danger px-3 py-2 text-sm text-danger">
                {t("cell.runsAbroad", {
                  home: info().home_country ?? "",
                  runsIn: info().runs_in ?? "",
                })}
              </p>
            </Show>
            <dl class="grid grid-cols-1 gap-x-6 gap-y-2 text-sm sm:grid-cols-[max-content_1fr]">
              <For each={ROWS}>
                {(row) => (
                  <>
                    <dt class="text-ink-muted">{t(row.key)}</dt>
                    <dd class="break-all text-ink">{row.value(info()) ?? t("cell.notSet")}</dd>
                  </>
                )}
              </For>
            </dl>
          </div>
        )}
      </Show>
    </Card>
  );
}
