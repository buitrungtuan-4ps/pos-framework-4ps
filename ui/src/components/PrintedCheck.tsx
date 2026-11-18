// The check last printed for what is on screen (ADR-0218), as every till sees it: when it printed
// and which copy it was, or, once the order has changed since, that the paper no longer shows what
// is owed. Nothing until a check is printed. Read from the check read itself, so a check another
// till printed shows here on this till's next read.

import { Show } from "solid-js";

import type { CheckPrintedResponse } from "../api/schemas";
import { locale, t } from "../i18n";

// The printed instant as a clock time, in the reader's own language.
function printedAt(time: string): string {
  return new Date(Date.parse(time)).toLocaleTimeString(locale() === "vi" ? "vi-VN" : "en-GB", {
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function PrintedCheck(props: { readonly printed: CheckPrintedResponse | undefined; readonly class?: string }) {
  return (
    <Show when={props.printed}>
      {(printed) => (
        <p
          class={`text-sm ${props.class ?? ""}`}
          classList={{ "text-danger": printed().stale, "text-ink-muted": !printed().stale }}
          role="status"
          data-outcome={printed().stale ? "check-printed-stale" : "check-printed"}
        >
          {t(printed().stale ? "check.printed_stale" : "check.printed", {
            time: printedAt(printed().printed_time),
            copy: printed().copy,
          })}
        </p>
      )}
    </Show>
  );
}
