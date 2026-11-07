// A subject whose personal values stay on the stores that collected them (ADR-0206 decision 4):
// each store that holds it, whether an erasure is pending there, and the erase action while a
// store holds it unasked. Ids and standing only, never a value.

import { For, Show } from "solid-js";

import type { SubjectStores } from "../../api/schemas";
import { t } from "../../i18n";
import { Button, Card } from "../../components/ui";
import { canEraseAtStores, pendingCount, storeStanding } from "../../lib/subject-stores";

export function HeldAtStores(props: { held: SubjectStores; busy: boolean; onErase: () => void }) {
    return (
        <Card title={t("subjects.atStoresTitle")}>
            <div class="flex flex-col gap-4">
                <p class="text-sm text-ink-muted">{t("subjects.atStoresHelp")}</p>
                <Show when={pendingCount(props.held) > 0}>
                    <p class="text-sm font-medium text-ink">
                        {t("subjects.erasurePendingAt", {
                            count: pendingCount(props.held),
                        })}
                    </p>
                </Show>
                <table class="w-full text-left text-sm">
                    <thead>
                        <tr class="border-b border-line text-ink-muted">
                            <th class="py-2 pr-4 font-medium">{t("subjects.store")}</th>
                            <th class="py-2 font-medium">{t("subjects.status")}</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For each={props.held.stores}>
                            {(store) => (
                                <tr class="border-b border-line text-ink">
                                    <td class="break-all py-2 pr-4 font-mono text-xs">
                                        {store.store_id}
                                    </td>
                                    <td class="py-2">
                                        {t(`subjects.standing.${storeStanding(store)}`)}
                                    </td>
                                </tr>
                            )}
                        </For>
                    </tbody>
                </table>
                <Show when={canEraseAtStores(props.held)}>
                    <div>
                        <Button
                            variant="danger-ghost"
                            disabled={props.busy}
                            onClick={() => props.onErase()}
                        >
                            {t("subjects.erase")}
                        </Button>
                    </div>
                </Show>
            </div>
        </Card>
    );
}
