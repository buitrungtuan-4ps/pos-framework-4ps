// The sales channels the console can label (ADR-0066, `pos-proto` `SalesChannel`). A channel on the
// wire is an open token (`lib/tokens.ts`): the full `wire_enum!` token, `SALES_CHANNEL_DINE_IN`, and
// possibly one a newer cloud added, which every screen shows raw and every form keeps.

import type { MessageKey } from "../i18n";
import { isKnown, tokenLabel } from "./tokens";

/** A sales channel this console knows. */
export type KnownSalesChannel =
  | "SALES_CHANNEL_DINE_IN"
  | "SALES_CHANNEL_TAKEAWAY"
  | "SALES_CHANNEL_DELIVERY"
  | "SALES_CHANNEL_QR"
  | "SALES_CHANNEL_API";

/** The channels an operator can price, in the order every editor lists them. */
export const SALES_CHANNELS: readonly KnownSalesChannel[] = [
  "SALES_CHANNEL_DINE_IN",
  "SALES_CHANNEL_TAKEAWAY",
  "SALES_CHANNEL_DELIVERY",
  "SALES_CHANNEL_QR",
  "SALES_CHANNEL_API",
];

/** The i18n key for each known channel's short label. */
export const CHANNEL_LABEL: Readonly<Record<KnownSalesChannel, MessageKey>> = {
  SALES_CHANNEL_DINE_IN: "channel.dineIn",
  SALES_CHANNEL_TAKEAWAY: "channel.takeaway",
  SALES_CHANNEL_DELIVERY: "channel.delivery",
  SALES_CHANNEL_QR: "channel.qr",
  SALES_CHANNEL_API: "channel.api",
};

/** Whether `channel` is one this console knows. */
export const isKnownChannel = (channel: string): channel is KnownSalesChannel =>
  isKnown(SALES_CHANNELS, channel);

/** A channel's label, or the raw token for one this console does not know. */
export const channelLabel = (channel: string): string => tokenLabel(CHANNEL_LABEL, channel);
