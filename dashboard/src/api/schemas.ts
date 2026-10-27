// The wire shapes, as the OpenAPI document declares them (ADR-0177 decision 3). Each name here is an
// alias of a schema in the generated `generated/admin.d.ts`, so a field the server adds, renames or
// stops sending changes the type the screens compile against. The hand-mirrored `types.ts` keeps only
// what the document cannot say; T4's `xtask hand-types` counts what is left there.

import type { components } from "./generated/admin";

type Schemas = components["schemas"];

export type ConfigVersion = Schemas["ConfigVersionView"];
export type ConfigNode = Schemas["ConfigNodeView"];
export type ConfigNodes = Schemas["ConfigNodesView"];
export type Enrolment = Schemas["Enrolment"];
export type ApiKeySummary = Schemas["ApiKeySummary"];
export type CreateApiKeyResponse = Schemas["CreateApiKeyResponse"];
export type PublishDevicesResponse = Schemas["DevicesPublished"];
export type DeviceProposalSummary = Schemas["DeviceProposalSummary"];
export type CreateTerminalResponse = Schemas["CreateTerminalResponse"];
export type RegisterWebhookResponse = Schemas["RegisterWebhookResponse"];
export type PublishedConfig = Schemas["PublishedConfig"];
export type DailyRollup = Schemas["DailyRollup"];
export type ItemMix = Schemas["ItemMix"];
export type DailyRevenue = Schemas["DailyRevenue"];
export type TaxComponentTotal = Schemas["TaxComponentTotal"];
export type FeeTotal = Schemas["FeeTotal"];
export type CashTotals = Schemas["CashTotals"];
export type DailyCash = Schemas["DailyCash"];
export type TranslationGrid = Schemas["TranslationGrid"];
export type TranslationImportRow = Schemas["ImportRow"];
export type TranslationImportReport = Schemas["ImportReport"];
