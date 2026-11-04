// The Custom data screen, at `/custom-data` (ADR-0194 decision 7): how many of a store's objects of
// one kind, or records of one object type, hold each value of one custom field, optionally among
// those whose filter field holds one value. Only non-personal fields are offered; the server refuses
// a personal one anyway, and the answer never names or counts a person.
//
// Labels come from the tenant's translation grid, in the console's language, then English, then the
// key itself (ADR-0043).

import { Show } from "solid-js";

import { EmptyState } from "../components/kit";
import { Banner, Card, PageHeader, Skeleton } from "../components/ui";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { AnswerCard } from "./custom-data/AnswerCard";
import { QuestionCard } from "./custom-data/QuestionCard";
import { useCustomData } from "./custom-data/state";

export function CustomData() {
  const state = useCustomData();
  onScopedContext("store", () => void state.load());
  const none = (
    <Card title={t("nav.customData")}>
      <EmptyState title={t("customData.none")} description={t("customData.noneHint")} />
    </Card>
  );
  return (
    <div>
      <PageHeader title={t("nav.customData")} description={t("customData.description")} />
      <RequireContext need="store">
        <Show when={state.error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show when={state.loaded()} fallback={<Skeleton label={t("common.loading")} rows={3} />}>
          <Show when={state.targets().length > 0} fallback={none}>
            <div class="flex flex-col gap-4">
              <QuestionCard state={state} />
              <Show when={state.summary()}>
                {(answer) => <AnswerCard state={state} answer={answer()} />}
              </Show>
            </div>
          </Show>
        </Show>
      </RequireContext>
    </div>
  );
}
