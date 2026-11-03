// Which variant of a package part the till shows (ADR-0189 decision 4): the till's half of the one
// rule `pos_core::package` implements for the edge. Both run the shared vectors in
// `crates/pos-proto/testdata/package_variants.json` (`unit/package-variant.test.ts`), so the two
// stay equal. `docs/architecture.md` §3.1 states the rule.
//
// The shapes below mirror `pos_proto::package` and are local to this module until the edge serves
// a package's content through a route of its OpenAPI document; then they become aliases in
// `api/schemas.ts` (ADR-0177).

/** The dimensions a variant states; each absent one matches everything. */
export interface VariantSelector {
    device_id?: string;
    device_class?: string;
    role_id?: string;
    sales_channel?: string;
    time_slot_id?: string;
}

/** One variant of a part. A variant with no selector is the part's base. */
export interface Variant<T> {
    selector?: VariantSelector;
    body: T;
}

/** What a variant is chosen against. */
export interface VariantContext {
    device_id?: string;
    device_class?: string;
    role_ids?: readonly string[];
    sales_channel?: string;
    time_slot_id?: string;
}

/** A weekly window: Monday is bit 0 of `days`; half-open; may wrap past midnight. */
export interface Schedule {
    days: number;
    start_minute: number;
    end_minute: number;
}

/** A named part of the week, in the package's order. */
export interface TimeSlot {
    time_slot_id: string;
    label_key: string;
    schedules: readonly Schedule[];
}

/** The local moment a slot is read at: 0 for Monday to 6 for Sunday, and minutes since midnight. */
export interface LocalMoment {
    weekday: number;
    minute_of_day: number;
}

// The tokens this release knows, as `pos_proto` declares them. A token from a newer release states
// its dimension and matches nothing, exactly as on the edge.
const DEVICE_CLASSES: ReadonlySet<string> = new Set([
    "DEVICE_CLASS_TERMINAL",
    "DEVICE_CLASS_TABLET",
    "DEVICE_CLASS_PHONE",
    "DEVICE_CLASS_KITCHEN_DISPLAY",
    "DEVICE_CLASS_KIOSK",
]);

const SALES_CHANNELS: ReadonlySet<string> = new Set([
    "SALES_CHANNEL_DINE_IN",
    "SALES_CHANNEL_TAKEAWAY",
    "SALES_CHANNEL_DELIVERY",
    "SALES_CHANNEL_QR",
    "SALES_CHANNEL_API",
]);

/** A token states its dimension unless it is absent or the enum's `*_UNSPECIFIED`. */
function stated(token: string | undefined): token is string {
    return token !== undefined && !token.endsWith("_UNSPECIFIED");
}

/** Whether a stated token is one this release knows and equals the context's. */
function knownAndEqual(
    known: ReadonlySet<string>,
    wanted: string,
    held: string | undefined,
): boolean {
    return known.has(wanted) && wanted === held;
}

/** The dimensions a selector states, in the tie-break's order, then the device named by id. */
function flags(selector: VariantSelector): boolean[] {
    const byId = selector.device_id !== undefined;
    return [
        byId || stated(selector.device_class),
        selector.role_id !== undefined,
        stated(selector.sales_channel),
        selector.time_slot_id !== undefined,
        byId,
    ];
}

/** How a candidate ranks: the dimensions it states, then one bit per tie-break step. */
function rank(selector: VariantSelector): [number, number] {
    const bits = flags(selector);
    const dimensions = bits.slice(0, 4).filter(Boolean).length;
    const order = bits.reduce((sum, bit) => sum * 2 + (bit ? 1 : 0), 0);
    return [dimensions, order];
}

function deviceMatches(
    selector: VariantSelector,
    context: VariantContext,
): boolean {
    if (
        selector.device_id !== undefined &&
        selector.device_id !== context.device_id
    )
        return false;
    return (
        !stated(selector.device_class) ||
        knownAndEqual(
            DEVICE_CLASSES,
            selector.device_class,
            context.device_class,
        )
    );
}

/** Whether every dimension `selector` states matches `context`. */
function matches(selector: VariantSelector, context: VariantContext): boolean {
    const role =
        selector.role_id === undefined ||
        (context.role_ids ?? []).includes(selector.role_id);
    const channel =
        !stated(selector.sales_channel) ||
        knownAndEqual(
            SALES_CHANNELS,
            selector.sales_channel,
            context.sales_channel,
        );
    const slot =
        selector.time_slot_id === undefined ||
        selector.time_slot_id === context.time_slot_id;
    return deviceMatches(selector, context) && role && channel && slot;
}

/**
 * The variant a device runs in `context`: the candidate stating the most dimensions; on a tie the
 * first of device, role, sales channel, time slot; then a device by id over one by class; then the
 * variant listed first. `undefined` only for variants with no base, which a publish refuses.
 */
export function resolveVariant<T>(
    variants: readonly Variant<T>[],
    context: VariantContext,
): T | undefined {
    let best: { rank: [number, number]; body: T } | undefined;
    for (const variant of variants) {
        const selector = variant.selector ?? {};
        if (!matches(selector, context)) continue;
        const candidate = rank(selector);
        const better =
            best === undefined ||
            candidate[0] > best.rank[0] ||
            (candidate[0] === best.rank[0] && candidate[1] > best.rank[1]);
        if (better) best = { rank: candidate, body: variant.body };
    }
    return best?.body;
}

function windowHolds(window: Schedule, at: LocalMoment): boolean {
    if ((window.days & (1 << at.weekday)) === 0) return false;
    const minute = at.minute_of_day;
    return window.start_minute <= window.end_minute
        ? minute >= window.start_minute && minute < window.end_minute
        : minute >= window.start_minute || minute < window.end_minute;
}

/** The store's current slot: the first of `timeSlots` one of whose windows holds `at`. */
export function currentTimeSlot(
    timeSlots: readonly TimeSlot[],
    at: LocalMoment,
): string | undefined {
    return timeSlots.find((slot) =>
        slot.schedules.some((window) => windowHolds(window, at)),
    )?.time_slot_id;
}
