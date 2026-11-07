// Where a subject held at stores stands (ADR-0206 decision 4): a personal custom value stays on the
// store that collected it, so an erasure is sent to each store by id and stays pending until the
// store confirms it.

import type { SubjectStores } from "../api/schemas";

/** A store's standing for one subject. */
export type StoreStanding = "masked" | "pending" | "holds";

/** The standing of one store row. */
export function storeStanding(store: SubjectStores["stores"][number]): StoreStanding {
    if (store.masked) {
        return "masked";
    }
    return store.erasure_pending ? "pending" : "holds";
}

/** How many stores still have to confirm an erasure. */
export function pendingCount(view: SubjectStores): number {
    return view.stores.filter((store) => storeStanding(store) === "pending").length;
}

/** Whether an erasure would reach a store that holds the subject and has not been asked yet. */
export function canEraseAtStores(view: SubjectStores): boolean {
    return view.stores.some((store) => storeStanding(store) === "holds");
}
