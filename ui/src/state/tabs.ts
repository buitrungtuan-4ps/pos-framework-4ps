// Bar tabs (ADR-0221): whether the store keeps them and lets staff label one, from the menu read,
// and the three writes the tabs screen sends through the edge-write path (ADR-0196 decision 1).
//
// A label is personal data: it is shown beside its tab's number and never kept by the till beyond
// the screen that lists it.

import { createSignal } from "solid-js";

import { api } from "../api/client";
import type { MenuResponse, OpenedOrder, Tab } from "../api/schemas";
import { editWrite } from "./edge-write";

const [tabsOn, setTabsOn] = createSignal(false);
const [labelsOn, setLabelsOn] = createSignal(false);

/** Applies the menu read's tab flags; an edge older than them keeps no tabs here. */
export function applyTabFlags(response: MenuResponse): void {
    setTabsOn(response.tabs_enabled === true);
    setLabelsOn(response.tab_guest_label === true);
}

/** Whether the store keeps bar tabs (`tabs_enabled`). */
export function tabsEnabled(): boolean {
    return tabsOn();
}

/** Whether staff may type a guest's label on a tab (`tabs.guest_label`, confirmed in the console). */
export function tabLabelsEnabled(): boolean {
    return labelsOn();
}

/** The open tabs a screen holds, as a getter and a setter. */
export interface TabList {
    readonly get: () => readonly Tab[];
    readonly set: (tabs: Tab[]) => void;
}

/** Opens a tab numbered for the day; answers the order to take its first round on. */
export function openTab(reread: () => void): Promise<OpenedOrder> {
    return editWrite("sales.tab.open", { send: () => api.openTab(), reread });
}

/** Writes `label` on a tab at once, or clears it with an empty one; a refusal puts the old one back. */
export async function labelTab(
    list: TabList,
    orderId: string,
    label: string,
    reread: () => void,
): Promise<void> {
    const text = label.trim();
    await editWrite("sales.tab.label", {
        paint: () => {
            const before = list.get();
            list.set(
                before.map((tab) => {
                    if (tab.order_id !== orderId) {
                        return tab;
                    }
                    const { label: _old, ...rest } = tab;
                    return text === "" ? rest : { ...rest, label: text };
                }),
            );
            return () => list.set([...before]);
        },
        send: () => api.labelTab(orderId, text),
        reread,
    });
}

/** Closes an empty tab, citing the store's reason where it lists one; it leaves the list at once. */
export async function closeEmptyTab(
    list: TabList,
    orderId: string,
    reasonCodeId: string | undefined,
    reread: () => void,
): Promise<void> {
    await editWrite("sales.order.close", {
        paint: () => {
            const before = list.get();
            list.set(before.filter((tab) => tab.order_id !== orderId));
            return () => list.set([...before]);
        },
        send: () => api.closeOrder(orderId, reasonCodeId),
        reread,
    });
}
