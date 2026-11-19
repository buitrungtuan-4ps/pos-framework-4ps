// A screen that sells listens for a barcode scanner (ADR-0222 decisions 2 and 7): where the store
// sells by barcode, digits arriving within its scan window and ending in Enter are looked up in the
// till's code index and the item is added as if its button were tapped. A code the menu does not
// carry says so. Not while a field has focus: there the scanner is typing into the field. Draws
// nothing but that sentence.

import { Show, createSignal, onCleanup, onMount } from "solid-js";

import type { MenuItemResponse } from "../api/schemas";
import { t } from "../i18n";
import { resolveCode, scansListened, scanWindowMs } from "../state/codes";
import { createScanReader } from "../state/scan-reader";
import { state } from "../state/store";

function typingInAField(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName));
}

export function ScanListener(props: {
  readonly disabled: boolean;
  readonly onItem: (item: MenuItemResponse) => void;
  /** Takes a code the till's index does not hold, in place of saying it is unknown. */
  readonly onUnknown?: (code: string) => void;
}) {
  const [unknown, setUnknown] = createSignal<string | null>(null);
  const reader = createScanReader(scanWindowMs);
  const onKey = (event: KeyboardEvent) => {
    if (!scansListened() || props.disabled || typingInAField(event.target)) {
      return;
    }
    const code = reader.key(event.key, event.timeStamp);
    if (code === null) {
      return;
    }
    event.preventDefault();
    const itemId = resolveCode(code);
    const item = state.menu.find((entry) => entry.menu_item_id === itemId);
    if (item === undefined && props.onUnknown !== undefined) {
      props.onUnknown(code);
      return;
    }
    setUnknown(item === undefined ? code : null);
    if (item !== undefined) {
      props.onItem(item);
    }
  };
  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));
  return (
    <Show when={unknown()}>
      {(code) => (
        <p class="mb-2 text-sm text-danger" role="status" data-outcome="scan-unknown">
          {t("order.scan_unknown", { code: code() })}
        </p>
      )}
    </Show>
  );
}
