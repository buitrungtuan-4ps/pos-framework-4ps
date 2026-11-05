// @generated from crates/pos-proto/src/screen.rs and screen/catalogue.rs — do not edit.
// Regenerate with `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot`.
//
// The component catalogue (ADR-0190, ADR-0203): every component a package's screen may
// name, with its properties, what it supplies to the acts it binds, the device classes it
// is drawn on, its fallback and the release that introduced it; and a screen's shape.

export type PropertyKind = "PROPERTY_KIND_UNSPECIFIED" | "PROPERTY_KIND_TRANSLATION_KEY" | "PROPERTY_KIND_TOKEN" | "PROPERTY_KIND_INT" | "PROPERTY_KIND_BOOL" | "PROPERTY_KIND_ACTION";

/** One property a component takes. */
export interface ComponentProperty {
    readonly name: string;
    readonly kind: PropertyKind;
    readonly required: boolean;
    readonly tokens: readonly string[];
    readonly min: number;
    readonly max: number;
}

/** One entry of the catalogue. */
export interface Component {
    readonly component_id: string;
    readonly properties: readonly ComponentProperty[];
    readonly holds_children: boolean;
    readonly supplies: readonly string[];
    readonly device_classes: readonly string[];
    readonly fallback_component: string | null;
    readonly since: string;
}

/** A fixed parameter of an act a property binds. */
export type ParamValue = number | string | readonly string[];

/** An action property: an act by its catalogue id, and the parameters the package fixes. */
export interface ActionBinding {
    readonly action_id: string;
    readonly params?: Readonly<Record<string, ParamValue>>;
}

/** A property's value, read by the kind the catalogue gives the property. */
export type PropertyValue = boolean | number | string | ActionBinding;

/** One node of a screen. */
export interface ScreenNode {
    readonly component_id: string;
    readonly properties?: Readonly<Record<string, PropertyValue>>;
    readonly children?: readonly ScreenNode[];
}

/** One screen of a package. */
export interface Screen {
    readonly screen_id: string;
    readonly root: ScreenNode;
}

/** The most nodes in one screen, and the deepest its tree goes. */
export const NODES_MAX = 400;
export const DEPTH_MAX = 12;

/** Every component, in the catalogue's order. */
export const COMPONENTS = [
    { component_id: "stack", properties: [{ name: "direction", kind: "PROPERTY_KIND_TOKEN", required: false, tokens: ["vertical", "horizontal"], min: 0, max: 0 }], holds_children: true, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "text", properties: [{ name: "text_key", kind: "PROPERTY_KIND_TRANSLATION_KEY", required: true, tokens: [], min: 0, max: 0 }, { name: "variant", kind: "PROPERTY_KIND_TOKEN", required: false, tokens: ["title", "body", "muted"], min: 0, max: 0 }], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "action_button", properties: [{ name: "label_key", kind: "PROPERTY_KIND_TRANSLATION_KEY", required: true, tokens: [], min: 0, max: 0 }, { name: "action", kind: "PROPERTY_KIND_ACTION", required: true, tokens: [], min: 0, max: 0 }, { name: "variant", kind: "PROPERTY_KIND_TOKEN", required: false, tokens: ["primary", "secondary", "danger"], min: 0, max: 0 }], holds_children: false, supplies: ["order_id", "table_id"], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "screen_frame", properties: [], holds_children: true, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "page_header", properties: [{ name: "title_key", kind: "PROPERTY_KIND_TRANSLATION_KEY", required: true, tokens: [], min: 0, max: 0 }], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "floor_alerts", properties: [], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "floor_plan", properties: [], holds_children: false, supplies: ["table_id"], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "order_layout", properties: [], holds_children: true, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "order_column", properties: [], holds_children: true, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "order_header", properties: [], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "order_notices", properties: [], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "order_move", properties: [], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "order_bill", properties: [], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "order_menu", properties: [], holds_children: true, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "menu_toolbar", properties: [], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "menu_fields", properties: [], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
    { component_id: "menu_buttons", properties: [], holds_children: false, supplies: [], device_classes: ["DEVICE_CLASS_TERMINAL", "DEVICE_CLASS_TABLET", "DEVICE_CLASS_PHONE", "DEVICE_CLASS_KITCHEN_DISPLAY", "DEVICE_CLASS_KIOSK"], fallback_component: null, since: "0.14.1" },
] as const satisfies readonly Component[];

/** The id of a component the catalogue holds. */
export type ComponentId = (typeof COMPONENTS)[number]["component_id"];
