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

export type Tenant = Schemas["Versioned_TenantRecord"];
export type Brand = Schemas["Versioned_BrandRecord"];
export type Store = Schemas["Versioned_StoreRecord"];
export type StoreGroup = Schemas["StoreGroupView"];
export type BatchResultRow = Schemas["BatchResultView"];
export type ConfigBatchReport = Schemas["BatchReport"];
export type Device = Schemas["Versioned_DeviceRecord"];
export type Money = Schemas["Money"];
export type TaxClass = Schemas["Versioned_TaxClass"];
export type TaxRate = Schemas["TaxRateView"];
export type TaxComponent = Schemas["TaxComponentView"];
export type ActivationCode = Schemas["IssueActivationResponse"];

export type ScheduledPublish = Schemas["ScheduledPublishView"];
export type ScheduledPublishCreated = Schemas["CampaignsScheduled"];
export type Release = Schemas["ReleaseView"];
export type ReleasePair = Schemas["ReleasePairView"];
export type ReleaseReport = Schemas["ReleaseReport"];
export type NodePreview = Schemas["NodePreview"];
export type Country = Schemas["CountryView"];
export type Quantity = Schemas["Quantity"];
export type Ingredient = Schemas["Versioned_PublishedIngredient"];
export type Supplier = Schemas["Versioned_PublishedSupplier"];

export type Employee = Schemas["Versioned_Employee"];
export type RoleTemplate = Schemas["Versioned_RoleTemplate"];
export type Assignment = Schemas["Assignment"];
export type PermissionsPublishResult = Schemas["PermissionsPublishResult"];
export type PermissionsPublishReport = Schemas["StoresPublished_PermissionsPublishResult"];
export type PermissionInfo = Schemas["PermissionInfo"];
export type MissingPermission = Schemas["MissingPermission"];
export type RoleReadiness = Schemas["RoleReadiness"];
export type PermissionsReadiness = Schemas["PermissionsReadiness"];
export type CreatedId = Schemas["CreatedId"];
export type CreatedAssignment = Schemas["AssignmentCreated"];
export type Area = Schemas["Versioned_Area"];
export type FloorTable = Schemas["Versioned_Table"];
export type Station = Schemas["Versioned_Station"];
export type RoutingRule = Schemas["FloorRoutingRule"];
export type TableQrToken = Schemas["TableQrEntry"];
export type TableQrSheet = Schemas["TableQrView"];
export type Alert = Schemas["AlertView"];

export type AdminInvite = Schemas["AdminInvite"];
export type AdminSessionView = Schemas["AdminSessionView"];
export type InviteAdminResponse = Schemas["InviteAdminResponse"];
export type RecoveryCodesResponse = Schemas["RecoveryCodesResponse"];
export type RecoveryCodesStatus = Schemas["RecoveryCodesStatus"];
export type FleetPrintAgent = Schemas["FleetPrintAgent"];
export type RegionAcknowledgement = Schemas["RegionAcknowledgementView"];
export type AdmittedDevice = Schemas["AdmittedDevice"];
export type StoreArchive = Schemas["ArchiveView"];
export type FleetStore = Schemas["FleetStoreView"];
export type StoreLease = Schemas["LeaseRead"];
export type HostedArtifact = Schemas["HostedArtifactResponse"];
export type HostedRelease = Schemas["ReleaseArtifacts"];
export type FetchedArtifact = Schemas["FetchedArtifactResponse"];
export type FetchedRelease = Schemas["ReleaseFetched"];
export type ReconcileRun = Schemas["ReconcileRunView"];
export type ChainFinding = Schemas["ChainFindingView"];
export type TaskHealthEntry = Schemas["TaskHealthView"];
export type TaskHealthReport = Schemas["TaskHealthReport"];
export type AuditEntry = Schemas["AuditEntryView"];
export type MediaSummary = Schemas["MediaSummaryView"];
export type SubjectMeta = Schemas["SubjectMetaView"];
export type SubjectExport = Schemas["SubjectExportView"];

export type WebhookSummary = Schemas["WebhookSummary"];
export type XzReport = Schemas["XzReport"];
export type ItemCategory = Schemas["Versioned_ItemCategory"];
export type ItemSubcategory = Schemas["Versioned_ItemSubcategory"];
export type DisplayCategory = Schemas["Versioned_CatalogDisplayCategory"];
export type DisplaySubcategory = Schemas["Versioned_CatalogDisplaySubcategory"];
export type GridPosition = Schemas["GridPosition"];
export type LayoutButton = Schemas["Versioned_LayoutButton"];
export type ModifierGroup = Schemas["Versioned_ModifierGroup"];
export type CatalogItem = Schemas["Versioned_CatalogItem"];
export type Menu = Schemas["Versioned_Menu"];
export type MenuSection = Schemas["Versioned_MenuSection"];
export type ChannelPrice = Schemas["ChannelPrice"];
export type MenuPlacement = Schemas["Versioned_MenuPlacement"];
export type AdminIdentity = Schemas["AdminUser"];

export type CapabilityFlag = Schemas["CapabilityFlagView"];
export type CapabilityPreset = Schemas["CapabilityPresetView"];
export type CapabilityRule = Schemas["CapabilityRuleView"];
export type CapabilityCatalogue = Schemas["CapabilityCatalogueView"];
export type ProviderField = Schemas["FieldView"];
export type Provider = Schemas["ProviderView"];
export type ProviderCatalogue = Schemas["CatalogueView"];
export type Connection = Schemas["Versioned_ConnectionView"];
export type SettingDefinition = Schemas["SettingCatalogueEntry"];
export type WrittenSetting = Schemas["StoredSettingValue"];
export type EffectiveSetting = Schemas["EffectiveSetting"];
export type SettingPublishResult = Schemas["SettingPublishResult"];
export type SettingPublishReport = Schemas["StoresPublished_SettingPublishResult"];
export type SettingPresetsReport = Schemas["PresetsApplied"];

export type FeeFault = Schemas["FeeFaultView"];
export type FeePublishResult = Schemas["FeePublishResult"];
export type FeePublishReport = Schemas["StoresPublished_FeePublishResult"];
export type SampleBill = Schemas["SampleBillView"];
