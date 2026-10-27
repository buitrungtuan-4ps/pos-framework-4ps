// Open wire tokens (`Open<E>` in `pos-proto`): the OpenAPI document types them as `string`, because
// a newer cloud may send one this console has never heard of, and the cloud keeps it as it arrived.
// The console keeps a `Known…` union of the tokens it can label; these two helpers are how a screen
// moves between the two without a cast, and without dropping or mislabelling a token it does not
// know. A form that loads one keeps it and sends it back on save.

import { t, type MessageKey } from "../i18n";

/** Whether `token` is one of `known`, narrowing it to that union. */
export function isKnown<Known extends string>(
  known: readonly Known[],
  token: string,
): token is Known {
  return (known as readonly string[]).includes(token);
}

/**
 * The label for `token`: its translated label when it is one of `labels`' keys, or the raw token as
 * the cloud sent it when this console does not know it — never a blank and never a wrong label.
 */
export function tokenLabel<Known extends string>(
  labels: Readonly<Record<Known, MessageKey>>,
  token: string,
): string {
  return isKnown(Object.keys(labels) as Known[], token) ? t(labels[token]) : token;
}
