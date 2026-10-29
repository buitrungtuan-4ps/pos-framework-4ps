// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What [`session_from_config`](super::session_from_config) does with each node of a synced config
//! document: one function per group of nodes, which it calls in turn.

use super::{
    Capability, CapabilityContext, CurrencyCode, CutoffHour, DeviceOtaConfig, EdgeSession,
    FleetUpdateConfig, FloorPlan, LayoutBook, LeaseConfig, LocaleSettings, MenuBook,
    PublishedBackup, PublishedCampaigns, PublishedChannels, PublishedCounter, PublishedDevices,
    PublishedFees, PublishedIntegrations, PublishedInventory, PublishedLocale,
    PublishedNumberFormat, PublishedPermissions, PublishedPrinting, PublishedQr,
    PublishedReasonCodes, PublishedRetention, PublishedSession, PublishedShift, PublishedTender,
    PublishedTenderKeys, SalesChannel, StationPlan, StoreProfile, StoreTimeZone, TaxRateTable,
    accepted_tender, campaigns_from_published, enabled_channels, inventory_from_published,
    localized_book, roster_from,
};

/// The store's display language from the `locale` node, and the `menu` node in it (ADR-0066,
/// ADR-0074).
pub(super) fn apply_menu(
    session: &mut EdgeSession,
    document: &serde_json::Value,
    channel: SalesChannel,
) {
    // The store's display language from the `locale` node (ADR-0074), if it set one — the locale it
    // renders item names in. Read here so the `menu` branch below can resolve each entry's per-locale
    // name once, at install; a store that sets no language shows each item's default name.
    let display_language = document
        .get("locale")
        .and_then(|locale| locale.get("display_language"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|language| !language.is_empty())
        .map(str::to_owned);
    // Parse the node via its JSON text, not `from_value`: some wire types (e.g. `CurrencyCode`)
    // deserialize from a *borrowed* `&str`, which `from_str` supports but `from_value` cannot.
    if let Some(book) = document
        .get("menu")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<MenuBook>(&text).ok())
    {
        // Resolve each entry's display name to the store's language once, here (ADR-0074): the priced
        // line and receipt then read in the store's language, and an item with no translation for it
        // keeps its default name (never-blank).
        let book = match display_language.as_deref() {
            Some(language) => localized_book(&book, language),
            None => book,
        };
        // The store's own channel's catalogue, and the whole book beside it: an order on another
        // channel is priced from that channel's prices (`EdgeSession::menu_for`).
        session.menu = book.catalog_for(channel).clone();
        session.menu_book = book;
    }
    // Kept as well as applied: a compiled menu can be resolved once here, but a list the binary
    // ships — ADR-0116's refusal reasons, from `PublishedReasonCodes::framework_default` — has no
    // apply step to be resolved in, so the read needs the language itself (ADR-0074).
    session.display_language.clone_from(&display_language);
}

/// The `permissions` node: the staff roster (ADR-0070).
pub(super) fn apply_permissions(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `permissions` node the people publish writes (ADR-0070) becomes the staff roster the edge
    // authorises sign-ins against, replacing any local roster, when it carries a `staff` list.
    let permissions_node = document.get(PublishedPermissions::NODE);
    if let Some(published) = permissions_node
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedPermissions>(&text).ok())
    {
        // The rollout switch rides the node (ADR-0158): the cloud writes it on the Tenant layer and
        // the merge puts it beside the staff. A node without it keeps the store-wide set.
        session.permissions_enforced = published.enforced;
        // A node with no `staff` list at all is the switch alone: the Tenant layer's setting with no
        // people node beside it, which is a store the people publish has not reached, or one whose
        // people node a rollback took away. It sets the switch and keeps the roster, because an
        // absent list says nothing about who works here. A `staff` list that is present but empty
        // is a people publish that assigned nobody, and it still empties the roster.
        if permissions_node.is_some_and(|node| node.get("staff").is_some()) {
            session.staff = roster_from(published.staff, session.currency);
        }
    }
}

/// The capability flags (ADR-0071), and which of them the document sets (ADR-0160).
pub(super) fn apply_capabilities(session: &mut EdgeSession, document: &serde_json::Value) {
    // The capability flags the config document carries (ADR-0071): top-level booleans keyed by each
    // capability's key (`tables_enabled`, `pay_first_enabled`, …), read the same way the cloud
    // validator does via the shared `CapabilityContext::from_flags` (§10). Applying them was a silent
    // no-op before M8 — the store now turns table service, KDS, pay-first, etc. on or off from the
    // published profile. Gate on the document carrying at least one known flag key, so a publish that
    // names none leaves the base profile unchanged (the never-blank contract the branches above keep);
    // once it names any, the profile is authoritative and an unnamed flag falls to its declared
    // default.
    if Capability::ALL
        .iter()
        .any(|capability| document.get(capability.meta().key).is_some())
    {
        session.capabilities = CapabilityContext::from_flags(|key| {
            document.get(key).and_then(serde_json::Value::as_bool)
        });
    }
    // And which of the flags this document sets, from the document whole (ADR-0160 decision 5): the
    // profile above reads an unset flag as its default, which cannot tell "off" from "never chosen".
    session.published_flags = Capability::ALL
        .iter()
        .filter_map(|capability| {
            document
                .get(capability.meta().key)
                .and_then(serde_json::Value::as_bool)
                .map(|on| (*capability, on))
        })
        .collect();
}

/// The `floor`, `stations` and `layout` nodes: the floor plan, the kitchen and the till's buttons
/// (ADR-0066, ADR-0072).
pub(super) fn apply_floor(
    session: &mut EdgeSession,
    document: &serde_json::Value,
    channel: SalesChannel,
) {
    // The `floor` and `stations` nodes the floor publish writes (ADR-0072): the store's areas/tables
    // and its kitchen stations + item→station routing. Parsed via JSON text like the `menu` node; an
    // absent or unparseable node leaves that plan as the base has it — a bad publish never blanks a
    // trading store's floor or kitchen.
    if let Some(floor) = document
        .get("floor")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<FloorPlan>(&text).ok())
    {
        session.floor = floor;
    }
    if let Some(stations) = document
        .get("stations")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<StationPlan>(&text).ok())
    {
        session.stations = stations;
    }
    // The `layout` node the catalog publish writes beside `menu` (ADR-0066): how the till groups and
    // orders its buttons, resolved here for the store's channel exactly as the price book is. Until
    // this branch the node was authored, validated, published — and read by nobody (C4). Absent or
    // unparseable leaves the plan as the base has it: a bad publish never blanks a trading till's
    // buttons, and an empty plan means the till draws the flat price book instead.
    if let Some(book) = document
        .get("layout")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<LayoutBook>(&text).ok())
    {
        session.layout = book.plan_for(channel).clone();
    }
}

/// The `devices`, `integrations` and `store_profile` nodes: what the store addresses, who serves
/// it, and who it legally is (ADR-0100, ADR-0153, ADR-0106).
pub(super) fn apply_store_identity(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `devices` node the device publish writes (ADR-0100): the printers and kitchen displays this
    // store may address. Absent leaves the session's set as the base has it — which for a box that
    // has never synced means empty, and empty means "print nothing", not "print blind".
    if let Some(devices) = document
        .get("devices")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedDevices>(&text).ok())
    {
        session.devices = devices;
    }
    // The `integrations` node (ADR-0153): the vendor connections that serve this store. Absent
    // leaves the session's as the base has it — empty on a box that never synced, which keeps every
    // family on its offline path; unparseable does the same, so a bad publish never switches a
    // store's e-invoicing or card terminal off mid-service.
    if let Some(integrations) = document
        .get("integrations")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedIntegrations>(&text).ok())
    {
        session.integrations = integrations;
    }
    // The `store_profile` node (ADR-0106): who this store legally is, as the receipt prints it. An
    // absent or unparseable node leaves the previous profile in place, which is the never-blank rule
    // applied where it matters most — a store mid-service must not start printing receipts with no
    // name because one publish was malformed.
    if let Some(profile) = document
        .get("store_profile")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<StoreProfile>(&text).ok())
    {
        session.profile = profile;
    }
}

/// The `shift`, `session`, `printing` and `qr` nodes of settings (ADR-0160).
pub(super) fn apply_settings(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `shift` node (ADR-0160): how the store runs its shifts. Every field is a setting, and a
    // setting the document does not carry means its default (ADR-0160 decision 1) — so, unlike the
    // nodes above, an absent node puts the defaults back rather than keeping the last value: the
    // cloud sends the whole document, and a node it no longer sends is a setting somebody cleared.
    // An unparseable node keeps the last value, as every node here does.
    match document.get(PublishedShift::NODE) {
        None => session.shift = PublishedShift::default(),
        Some(value) => {
            if let Some(shift) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<PublishedShift>(&text).ok())
            {
                session.shift = shift;
            }
        }
    }
    // The `session` node (ADR-0160): how long a sign-in lasts on an idle device and how many wrong
    // PINs lock a person out. A node of settings like `shift`, so absent means the defaults and an
    // unparseable one keeps the last value — a broken publish must not relax a lockout mid-service.
    // The sign-in window and the lockout read it from the live session, so it applies from the next
    // request, with no restart ([`crate::auth::Sessions`]).
    match document.get(PublishedSession::NODE) {
        None => session.session_settings = PublishedSession::default(),
        Some(value) => {
            if let Some(settings) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<PublishedSession>(&text).ok())
            {
                session.session_settings = settings;
            }
        }
    }
    // The `printing` node (ADR-0160), read as the `shift` node is: absent puts the defaults back,
    // unparseable keeps the last.
    match document.get(PublishedPrinting::NODE) {
        None => session.printing = PublishedPrinting::default(),
        Some(value) => {
            if let Some(printing) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<PublishedPrinting>(&text).ok())
            {
                session.printing = printing;
            }
        }
    }
    // The settings on the `qr` node (ADR-0160 item 2), read as the `shift` node is: absent puts the
    // defaults back, unparseable keeps the last. The node's guardrails are read below, field by
    // field, as they always were.
    match document.get(PublishedQr::NODE) {
        None => session.qr = PublishedQr::default(),
        Some(value) if value.is_object() => {
            if let Some(qr) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<PublishedQr>(&text).ok())
            {
                session.qr = qr;
            }
        }
        // A node that is not an object keeps the last too, as it carries no guardrail either
        // ([`PublishedQr::guardrails`]): the whole node is read only as an object, where serde alone
        // would read an array positionally into the node's fields.
        Some(_) => {}
    }
}

/// The `tax`, `campaigns`, `inventory` and `reason_codes` nodes: what a line costs and why it
/// changes.
pub(super) fn apply_pricing(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `tax` node the tax publish writes (ADR-0074, Track M4): the per-(tax class × channel) rate
    // table the edge reprices and bills against. Until M4 this was only ever the hardcoded bootstrap
    // default; a store now bills the authored rates. Absent or unparseable leaves the base table
    // untouched — a bad publish never blanks a trading store's tax to zero (and `rate_for` still
    // refuses an unpriced class rather than charging no tax).
    if let Some(tax_rates) = document
        .get("tax")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<TaxRateTable>(&text).ok())
    {
        // A row that names a component with something other than a token is charged at its rate
        // and gives no breakdown (ADR-0168 decision 2): the money never depends on the components.
        // Said once, where the table is applied, naming the class and nobody else.
        for row in tax_rates.rows_with_untokened_components() {
            tracing::warn!(
                tax_class_id = %row.tax_class_id,
                sales_channel = row.sales_channel.as_wire(),
                "a tax row names a component that is not a token; it is charged at its rate with \
                 no breakdown"
            );
        }
        session.tax_rates = tax_rates;
    }
    // The `campaigns` node the campaign publish writes (ADR-0077, Track M3): the store's authored
    // promotions, converted to the runtime `Campaign`s the pricing engine (`pos_core::campaign`)
    // evaluates. Absent or unparseable leaves the base list untouched — a bad publish never drops a
    // trading store's promotions. (Applying them to a live bill — building the eval context, timing,
    // and voucher redemption — is the flagged follow-up; this delivers the campaigns to the session.)
    if let Some(published) = document
        .get("campaigns")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedCampaigns>(&text).ok())
    {
        session.campaigns = campaigns_from_published(&published);
    }
    // The `inventory` node the inventory publish writes (ADR-0079, Track M6): the store's ingredients,
    // per-item recipes (bill of materials), and auto-86 thresholds, converted to the runtime RecipeBook
    // the fire path consumes and a per-item threshold map. Absent or unparseable leaves the base book
    // untouched — a bad publish never blanks a trading store's recipes. This kills the empty-book
    // bootstrap: a fired line now consumes its ingredients (`decide_line` reads `session.recipes`).
    // Deriving live item availability (auto-86 on the menu) needs an on-hand stock projection, which
    // arrives with the flagged goods-in/stocktake follow-up; the thresholds are delivered here so that
    // slice has them, and `EdgeSession::item_sellable` is the pure decision it will drive.
    if let Some(published) = document
        .get("inventory")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedInventory>(&text).ok())
    {
        let (book, thresholds) = inventory_from_published(&published);
        session.recipes = book;
        session.recipe_thresholds = thresholds;
    }
    // The `reason_codes` node the reason-code publish writes
    // ([ADR-0115](../../../../docs/adr/0115-reason-codes-are-a-managed-list.md), roadmap B2.2): the
    // managed list a void, a discount, a comp, a refund, an out-of-sale drawer opening, a staff
    // rejection, either cash movement and either stock correction must cite. `docs/pos-spec.md` §11
    // item 2 makes these reasons a fraud control, and until this node arrived a store could only
    // ever cite the framework set compiled into the binary.
    //
    // A present node **replaces** the framework default wholesale rather than merging with it, which
    // is what lets an operator remove a framework reason they judge wrong for their business —
    // merging would quietly resurrect it. Absent or unparseable leaves the framework set in place, so
    // a store that has never published one can still void a mis-keyed line on its first day, during a
    // cloud outage, before anyone has opened the console (ADR-0001).
    //
    // The cloud refuses to publish an empty list for the same reason: with the replace rule, an empty
    // node is not "no opinion", it is taking every reason away from a trading store.
    if let Some(published) = document
        .get("reason_codes")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedReasonCodes>(&text).ok())
        .filter(|published| !published.is_empty())
    {
        session.reason_codes = published;
    }
}

/// The `fees`, `channels` and `tender` nodes: what a bill may carry and how it is paid
/// (ADR-0159).
pub(super) fn apply_bill_rules(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `fees` node ([ADR-0159](../../../../docs/adr/0159-a-fee-is-configuration.md) decision 1):
    // every fee rule a bill may apply. Absent means no rule, as for a node of settings: the cloud
    // sends the whole document, so a node it no longer sends is fees somebody removed, and an empty
    // list is the same answer. An unparseable node keeps the rules the store had, as every node
    // here does, so a bad publish never changes what a trading store charges. A bill freezes the
    // rules in force when it opens, so neither reaches a bill already on a table.
    match document.get(PublishedFees::NODE) {
        None => session.fees = PublishedFees::default(),
        Some(value) => {
            if let Some(fees) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<PublishedFees>(&text).ok())
            {
                session.fees = fees;
            }
        }
    }
    // The `channels` node the channels publish writes (ADR-0080, Track M7): the sales channels a store
    // accepts. A present node is authoritative — the edge refuses an order on a channel it does not
    // list; an absent node leaves `None` (no restriction), so a store that never published one trades
    // on every channel exactly as before M7.
    if let Some(published) = document
        .get("channels")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedChannels>(&text).ok())
    {
        session.enabled_channels = Some(enabled_channels(&published));
    }
    // The `tender` node the tender publish writes (ADR-0080, Track M7): the payment methods a store
    // accepts. Same opt-in rule — a present node gates settlement, an absent one accepts any known
    // method as before.
    if let Some(published) = document
        .get("tender")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedTender>(&text).ok())
    {
        session.accepted_tender = Some(accepted_tender(&published));
    }
}

/// The `tender_keys`, `counter` and `backup` nodes of settings (ADR-0160).
pub(super) fn apply_till_settings(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `tender_keys` node (ADR-0160 decision 2): the keys the pay screen offers. A node of
    // settings like `shift`, so absent puts the defaults back and a node that is not an object keeps
    // the last; a field it carries that nobody can read is that field's default
    // ([`PublishedTenderKeys`]).
    match document.get(PublishedTenderKeys::NODE) {
        None => session.tender_keys = PublishedTenderKeys::default(),
        Some(value) => {
            if let Some(keys) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<PublishedTenderKeys>(&text).ok())
            {
                session.tender_keys = keys;
            }
        }
    }
    // The `counter` node (ADR-0160 decision 2): the channel the counter opens a walk-in on when the
    // till names none. A node of settings like `tender_keys`, so absent puts takeaway back and a node
    // that is not an object keeps the last; a value that is not a token is takeaway
    // ([`PublishedCounter`]).
    match document.get(PublishedCounter::NODE) {
        None => session.counter = PublishedCounter::default(),
        Some(value) => {
            if let Some(counter) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<PublishedCounter>(&text).ok())
            {
                session.counter = counter;
            }
        }
    }
    // The `backup` node (ADR-0160 decision 6): how often the store archives its database. A node
    // of settings like `counter`, so absent leaves the box on its own interval and an unparseable
    // one keeps the last. The archive loop reads it as it schedules each archive
    // ([`crate::backup_client`]), so it applies from the next archive.
    match document.get(PublishedBackup::NODE) {
        None => session.backup = PublishedBackup::default(),
        Some(value) => {
            if let Some(backup) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<PublishedBackup>(&text).ok())
            {
                session.backup = backup;
            }
        }
    }
}

/// The `qr` node's guardrails and the retention node.
pub(super) fn apply_qr_and_retention(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `qr` guardrail node (P11b, authored via ADR-0080, Track M7): the edge reads only
    // `staff_confirmation_required` — whether a table-bearing QR order waits for staff before the
    // kitchen sees it. The cloud reads the same node for its own guardrails (hours, rate). Whether
    // the store takes QR orders at all is not on this node: it is the `qr_ordering_enabled` flag,
    // read with the capabilities above (ADR-0160 decision 5), with `qr.enabled` below to tell it
    // from the flag as it was before the switch. An absent field leaves the base value (default
    // `true`, ADR-0057), so a store that never published the node still holds guest orders for
    // staff. Read field by field, apart from the node's setting above
    // ([`PublishedQr::guardrails`]), so one malformed field takes no other down.
    let guardrails = document
        .get(PublishedQr::NODE)
        .and_then(PublishedQr::guardrails);
    if let Some(required) = guardrails
        .as_ref()
        .and_then(|qr| qr.staff_confirmation_required)
    {
        session.qr_staff_confirmation_required = required;
    }
    // `qr.enabled` as this document sets it, read only beside QR ordering's switch: a cloud that
    // knows the switch publishes it equal to the switch, so a `false` switch beside a `qr.enabled`
    // that is not `false` is the flag from before the switch (`EdgeSession::qr_ordering_switched_off`).
    session.qr_enabled_published = guardrails.and_then(|qr| qr.enabled);
    // The `retention` node (ADR-0145): how many days this store keeps a synced event. Outside the
    // range, or absent, leaves the value it had, so a malformed publish never shortens a log
    // ([`PublishedRetention::event_log_days`]).
    if let Some(days) = document
        .get(PublishedRetention::NODE)
        .and_then(PublishedRetention::read)
        .and_then(|retention| retention.event_log_days())
    {
        session.event_log_days = days;
    }
}

/// The `locale` node and its settings (ADR-0074, ADR-0160).
pub(super) fn apply_locale(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `locale` node the locale publish writes (ADR-0074, Track M4): the store's currency, timezone,
    // and business-date cutoff. Until M4 these were hardcoded to VND/UTC/04:00 in the edge bootstrap.
    // Each field applies only if it parses, so a malformed timezone leaves the running clock alone
    // rather than resetting the store's business date to UTC (the never-blank rule, per field).
    // Read through the country's view of the node ([`PublishedLocale`]), whose required fields keep a
    // node carrying only settings out of this branch.
    if let Some(locale) = document
        .get("locale")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedLocale>(&text).ok())
    {
        if let Ok(currency) = CurrencyCode::parse(&locale.currency_code) {
            session.currency = currency;
        }
        // Applied only when stated, so an older publish leaves the session's figure alone rather
        // than silently making every price a whole unit.
        if let Some(exponent) = locale.currency_exponent {
            session.currency_exponent = exponent;
        }

        if let Ok(timezone) = StoreTimeZone::from_iana_name(&locale.timezone) {
            session.timezone = timezone;
        }
        if let Ok(cutoff) = CutoffHour::new(locale.cutoff_hour) {
            session.cutoff = cutoff;
        }
        // A bool cannot fail to parse, so unlike its siblings this one applies unconditionally —
        // which is also what makes turning the posture back off a publish rather than a release.
        session.prices_include_tax = locale.prices_include_tax;
        // Likewise: both have a `serde(default)` that *is* the previous behaviour, so applying them
        // unconditionally is what lets a store turn cash rounding off, or clear its quick-cash keys,
        // by publishing — rather than by waiting for a build (ADR-0105). A non-positive increment is
        // dropped rather than obeyed: `round_to_increment` needs a non-zero step, and a negative one
        // is a typo nobody means.
        session.cash_rounding_increment = locale
            .cash_rounding_increment
            .filter(|increment| *increment > 0);
        session
            .cash_denominations
            .clone_from(&locale.cash_denominations);
        // Absent, or zero, leaves what the store already had: a published zero would mean "scrub a
        // buyer's record the instant the invoice is printed", which is never what an operator means
        // and would destroy the document's own evidence (ADR-0107).
        if let Some(days) = locale.default_retention_days.filter(|days| *days > 0) {
            session.retention_days = days;
        }
        // Applied only when the publish both states one and states a usable one, so a cloud that
        // predates the field — or one that published a separator no formatter can place — leaves the
        // store writing numbers the way it already did rather than blank.
        if let Some(format) = locale
            .number_format
            .as_ref()
            .and_then(PublishedNumberFormat::validate)
        {
            session.number_format = format;
        }
        // Applied as stated, absent included: a locale that names no country language leaves a
        // country receipt nothing to follow but the display language, whatever an earlier one said.
        session.country_language = locale
            .country_language
            .map(|language| language.trim().to_owned())
            .filter(|language| !language.is_empty());
    }
    // The settings on the `locale` node (ADR-0160), read as the `qr` node's are: absent puts the
    // default back, unparseable keeps the last. Read apart from the country's fields above, so a
    // store whose locale the cloud has not published yet still rounds as its settings say.
    match document.get(LocaleSettings::NODE) {
        None => session.tax_rounding = LocaleSettings::default().tax_rounding(),
        Some(value) => {
            if let Some(settings) = serde_json::to_string(value)
                .ok()
                .and_then(|text| serde_json::from_str::<LocaleSettings>(&text).ok())
            {
                session.tax_rounding = settings.tax_rounding();
            }
        }
    }
}

/// The `fleet_update`, `device_ota` and `lease` nodes: how the store updates, and which box
/// holds its lease.
pub(super) fn apply_updates(session: &mut EdgeSession, document: &serde_json::Value) {
    // The `fleet_update` node the OTA publish writes (ADR-0048): the rollout every device weighs
    // itself against, and the signing keys revocation has retired. The cloud has published this node
    // since P9e-3 and nothing here read it, so the rollout decision had no rollout to decide about.
    //
    // The parse is two-stage — serde into the wire form, then `validate` into the domain form — and
    // both stages are recoverable in the same way: an absent, unparseable, or invalid node leaves the
    // previous rollout untouched. That is the right failure for this node specifically. A device that
    // keeps yesterday's rollout installs an update it was already eligible for; a device that lost it
    // would stop being eligible for anything, which turns one bad publish into a fleet that no longer
    // takes security fixes. Halting a rollout is `halted` inside the node, never a deletion, so
    // stopping the fleet does not depend on a node vanishing.
    //
    // A violation reaching here means the cloud published something its own validator would refuse
    // (the same `FleetUpdateConfig::validate` runs on both sides), so it is logged rather than
    // swallowed. Config carries no personal data, so the violations themselves are safe to log.
    if let Some(config) = document
        .get("fleet_update")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<FleetUpdateConfig>(&text).ok())
    {
        match config.validate() {
            Ok(rollout) => session.fleet_update = Some(rollout),
            Err(violations) => tracing::warn!(
                violations = %violations.join("; "),
                "the published fleet_update node did not validate; keeping the previous rollout"
            ),
        }
    }
    // The `device_ota` node (ADR-0048): where the cloud has placed *this* device — its ring and its
    // stable canary bucket. Stable is the point: the bucket fixes the device's position in the canary
    // ramp, so a device does not cross the rollout threshold and back again as the percentage moves,
    // and the cloud owning it is what makes it survive a reboot.
    //
    // Same never-blank rule as every node above, and here it is what keeps a device *placed*: losing
    // the placement would leave it eligible for nothing at all. A device the cloud has never placed
    // stays `None`, which is the safe end of that trade rather than an accident — see the field's own
    // doc for why no default ring is invented.
    if let Some(config) = document
        .get("device_ota")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<DeviceOtaConfig>(&text).ok())
    {
        match config.validate() {
            Ok(assignment) => session.device_ota = Some(assignment),
            Err(violations) => tracing::warn!(
                violations = %violations.join("; "),
                "the published device_ota node did not validate; keeping the previous placement"
            ),
        }
    }
    // The `lease` node (ADR-0108): the store's *authoritative* lease generation — the number this
    // box's own held generation is weighed against to decide whether it is still the store. Derived
    // by the cloud from its `store_lease` row, never authored by anybody.
    //
    // Same never-blank rule, and it is load-bearing in a way the others are not: a malformed or
    // absent node must not un-supersede a machine, so an unparseable value keeps the last good
    // generation rather than clearing it back to "no lease issued". A store the cloud has never
    // issued a lease to stays `None`, which reads as active — the fleet's behaviour before this node
    // existed, preserved on purpose.
    if let Some(config) = document
        .get("lease")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<LeaseConfig>(&text).ok())
    {
        session.lease_generation = Some(config.generation());
    } else if document.get("lease").is_some() {
        tracing::warn!(
            "the published lease node did not parse; keeping the previous lease generation"
        );
    }
}
