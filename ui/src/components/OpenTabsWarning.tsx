// The shift close's warning while bar tabs are open (ADR-0221 decision 6): which tabs, by number.
// It blocks nothing: a tab open at the close stays open into the next shift, and the next day's.
// Read once when the counted drawer is shown, where the store keeps tabs.

import { Show, createSignal, onMount } from "solid-js";

import { api } from "../api/client";
import { t } from "../i18n";
import { tabsEnabled } from "../state/tabs";

export function OpenTabsWarning() {
    const [numbers, setNumbers] = createSignal<number[]>([]);
    onMount(() => {
        if (tabsEnabled()) {
            void api
                .openTabs()
                .then((tabs) => setNumbers(tabs.map((tab) => tab.tab_number)))
                .catch(() => setNumbers([]));
        }
    });
    return (
        <Show when={numbers().length > 0}>
            <p
                class="mt-3 rounded-token border border-awaiting px-3 py-2 text-ink"
                role="status"
                data-outcome="open-tabs"
            >
                {t("shift.open_tabs", {
                    count: numbers().length,
                    numbers: numbers().join(", "),
                })}
            </p>
        </Show>
    );
}
