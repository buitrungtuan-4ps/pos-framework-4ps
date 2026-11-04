// The server's answer: one row per value, with how many objects hold it. No person is named or
// counted (ADR-0194 decision 7).

import { Show } from "solid-js";

import type { ValueCount, ValueSummary } from "../../api/schemas";
import { type Column, DataTable, EmptyState } from "../../components/kit";
import { Card } from "../../components/ui";
import { t } from "../../i18n";
import { valueText } from "../../lib/custom-data";
import type { CustomDataState } from "./state";

export function AnswerCard(props: { state: CustomDataState; answer: ValueSummary }) {
  const state = props.state;
  const text = (row: ValueCount) =>
    valueText(row.value, {
      lists: state.lists(),
      fields: state.target()?.fields ?? [],
      label: state.label,
      yes: t("customData.yes"),
      no: t("customData.no"),
    });
  const columns: readonly Column<ValueCount>[] = [
    { key: "value", header: t("customData.value"), cell: text },
    {
      key: "objects",
      header: t("customData.objects"),
      cell: (row) => String(row.objects),
      sortValue: (row) => row.objects,
    },
  ];
  return (
    <Card title={t("customData.answer")}>
      <Show when={props.answer.truncated}>
        <p class="mb-2 text-sm text-ink-muted" role="status">
          {t("customData.truncated")}
        </p>
      </Show>
      <DataTable
        columns={columns}
        rows={props.answer.groups}
        pageSize={50}
        empty={<EmptyState title={t("customData.empty")} description={t("customData.emptyHint")} />}
      />
    </Card>
  );
}
