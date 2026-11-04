// The kitchen's and the guest queue's edge writes, painted first (ADR-0196 decision 1,
// `./edge-write`): bumping a ticket, and confirming or refusing a guest's QR order. Each shows at
// once; the edge's answer makes it so, a refusal puts it back, and no answer puts it back and reads
// its subject again (the open orders, the guest queue). None is ever sent again by the till.

import { createSignal } from "solid-js";

import { api } from "../api/client";
import type { WaitingOrder } from "../api/schemas";
import { errorMessage } from "../lib/errors";
import { editWrite } from "./edge-write";
import { loadLiveOrders, state, updateStore } from "./store";

// A station marks a ticket's lines prepared. The edge records the durable `kitchen.ticket.bumped`
// event and fans it out, so every KDS folds the same prepared set; the ticket leaves this board on
// the tap. `stationId` is the station the ticket was fired to, where the board knows it; the store's
// default station otherwise, which is what every bump sent before lines carried one.
export async function bump(orderId: string, orderLineIds: readonly string[], stationId?: string): Promise<void> {
  // Only the lines this tap marks: one another station bumped already stays bumped on an undo.
  const marked = orderLineIds.filter((id) => state.bumped[id] !== true);
  await editWrite("sales.ticket.bump", {
    paint: () => {
      updateStore((draft) => {
        for (const id of marked) {
          draft.bumped[id] = true;
        }
      });
      return () =>
        updateStore((draft) => {
          for (const id of marked) {
            delete draft.bumped[id];
          }
        });
    },
    send: () =>
      api.bumpTicket({
        order_id: orderId,
        station_id: stationId ?? state.defaultStation,
        order_line_ids: [...orderLineIds],
      }),
    reread: () => void loadLiveOrders(),
  });
}

/** A board's bump, and the sentence of the last one the edge refused or did not answer. */
export function createBumper(): {
  readonly refused: () => string | null;
  readonly bump: (orderId: string, orderLineIds: readonly string[], stationId?: string) => void;
} {
  const [refused, setRefused] = createSignal<string | null>(null);
  return {
    refused,
    bump: (orderId, orderLineIds, stationId) => {
      setRefused(null);
      bump(orderId, orderLineIds, stationId).catch((caught: unknown) => setRefused(errorMessage(caught)));
    },
  };
}

/** The guest queue a screen holds, as a getter and a setter of its list. */
export interface WaitingList {
  readonly get: () => readonly WaitingOrder[] | null;
  readonly set: (orders: WaitingOrder[]) => void;
}

/** Takes `orderId` off `list` at once; hands back how to put it back where it stood. */
export function takeOff(list: WaitingList, orderId: string): () => void {
  const held = list.get() ?? [];
  const at = held.findIndex((order) => order.order_id === orderId);
  if (at < 0) {
    return () => undefined;
  }
  const order = held[at] as WaitingOrder;
  list.set(held.filter((waiting) => waiting.order_id !== orderId));
  return () => {
    const now = list.get() ?? [];
    if (now.some((waiting) => waiting.order_id === orderId)) {
      return;
    }
    list.set([...now.slice(0, at), order, ...now.slice(at)]);
  };
}

// A guest's QR order confirmed by a member of staff (ADR-0116): it leaves the queue on the tap, and
// the kitchen gets it once the edge commits.
export async function confirmGuestOrder(list: WaitingList, orderId: string, reread: () => void): Promise<void> {
  await editWrite("sales.order.confirm", {
    paint: () => takeOff(list, orderId),
    send: () => api.confirmOrder(orderId),
    reread,
  });
}

// A guest's QR order refused, citing a reason from the store's managed list (ADR-0115).
export async function rejectGuestOrder(
  list: WaitingList,
  orderId: string,
  reasonCodeId: string,
  reread: () => void,
): Promise<void> {
  await editWrite("sales.order.reject", {
    paint: () => takeOff(list, orderId),
    send: () => api.rejectOrder(orderId, reasonCodeId),
    reread,
  });
}
