// The wire shapes, as the OpenAPI document declares them (ADR-0177 decision 3). Each name here is an
// alias of a schema in the generated `generated/edge.d.ts`, so a field the server adds, renames or
// stops sending changes the type the screens compile against. The hand-mirrored `types.ts` keeps only
// what the document cannot say; T4's `xtask hand-types` counts what is left there.

import type { components } from "./generated/edge";

type Schemas = components["schemas"];

export type LineRequest = Schemas["LineRequest"];
export type LineResponse = Schemas["LineResponse"];
export type OrderLineRequest = Schemas["OrderLineRequest"];
export type OpenedOrder = Schemas["OpenedOrderResponse"];
export type QuantityRequest = Schemas["QuantityRequest"];
export type FireRequest = Schemas["FireRequest"];
export type FloorGridPosition = Schemas["GridPosition"];
export type FloorTable = Schemas["FloorTable"];
export type FloorArea = Schemas["FloorArea"];
export type FloorPlan = Schemas["FloorPlan"];
export type KitchenStation = Schemas["KitchenStation"];
export type StationRoutingRule = Schemas["RoutingRule"];
export type StationPlan = Schemas["StationPlan"];
export type FloorResponse = Schemas["FloorResponse"];
export type ModifierGroup = Schemas["ModifierGroupResponse"];
export type CourseResponse = Schemas["CourseResponse"];
export type MenuItemResponse = Schemas["MenuItemResponse"];
export type CheckResponse = Schemas["CheckResponse"];
export type BillCheckResponse = Schemas["BillCheckResponse"];
export type SplitRequest = Schemas["SplitRequest"];
export type SplitResponse = Schemas["SplitResponse"];
export type MergeRequest = Schemas["MergeRequest"];
export type DiscountRequest = Schemas["DiscountRequest"];
export type DiscountResponse = Schemas["DiscountResponse"];
export type CounterLine = Schemas["CounterLineResponse"];
export type WaitingLine = Schemas["WaitingLineResponse"];
export type RejectReason = Schemas["RejectReasonResponse"];
export type WaitingOrder = Schemas["WaitingOrderResponse"];
export type WaitingResponse = Schemas["WaitingResponse"];
export type BumpRequest = Schemas["BumpRequest"];
export type ReasonCodeEntry = Schemas["ReasonCodeResponse"];
export type ReasonCodesResponse = Schemas["ReasonCodesResponse"];
export type VoidBillResponse = Schemas["VoidBillResponse"];
export type BumpResponse = Schemas["BumpResponse"];
export type PaymentRequest = Schemas["PaymentRequest"];
export type BuyerRequest = Schemas["BuyerRequest"];
export type LayoutButton = Schemas["ButtonResponse"];
export type LayoutSubcategory = Schemas["SubcategoryResponse"];
export type LayoutCategory = Schemas["CategoryResponse"];
export type LayoutResponse = Schemas["LayoutResponse"];

export type OpenShiftRequest = Schemas["OpenRequest"];
export type CountShiftRequest = Schemas["CountRequest"];
export type CloseShiftRequest = Schemas["CloseRequest"];
export type VarianceReason = Schemas["VarianceReason"];
export type BatchCloseRequest = Schemas["BatchCloseRequest"];
export type CashMovementRequest = Schemas["MovementRequest"];
export type OpenDrawerRequest = Schemas["OpenDrawerRequest"];
export type PairRequest = Schemas["PairRequest"];
export type PairAccepted = Schemas["PairAccepted"];
export type PairingState = Schemas["PairingState"];
export type RevokeRequest = Schemas["RevokeRequest"];
export type MintedCode = Schemas["MintedCode"];
export type ActivateRequest = Schemas["ActivateRequest"];
export type ActivateAccepted = Schemas["ActivateAccepted"];
export type ActivationStanding = Schemas["StandingResponse"];
export type ClaimStatus = Schemas["ClaimStatus"];
export type IntegrationEntry = Schemas["IntegrationResponse"];
export type PrinterEntry = Schemas["PrinterResponse"];
export type TestPrintResponse = Schemas["TestPrintResponse"];
export type PrintResponse = Schemas["PrintResponse"];
export type ReprintResponse = Schemas["ReprintResponse"];
