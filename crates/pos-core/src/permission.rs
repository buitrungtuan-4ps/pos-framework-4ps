// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The permission catalogue: a fixed set the framework owns, enforced through one gate.
//!
//! `docs/pos-spec.md` §9 fixes the shape. Permissions are a **fixed catalogue** declared in
//! `pos-core`; **roles are data**, edited in the cloud and synced to the edge as a
//! [`PermissionSet`], so offline authorisation behaves identically. Every check goes through the
//! one [`require`] function — ad-hoc role-string checks are banned — and the default is **deny**.
//!
//! # Adding a permission forces the decision that must not be forgotten
//!
//! §9 requires that adding a permission cannot be quiet: the compiler must force the template
//! update, the dashboard must show it, the audit log must tag it, and the documented matrix must
//! regenerate. Here that is one mechanism: a permission exists only as an entry in the
//! [`permissions!`] block, and that entry *requires* its metadata — id, group, risk, PIN flag,
//! description, and the default roles that receive it. There is no way to add a `Permission` variant
//! without deciding its default roles, because the variant and its metadata are one declaration.
//! The default matrix, the snapshot, and the generated `docs/permissions.md` all derive from that
//! single source, so none can drift from another.
//!
//! # Removing is forbidden; deprecate instead
//!
//! `docs/snapshots/permissions.txt` records every permission id, and `cargo xtask snapshot` refuses
//! to let an id disappear between the base branch and a pull request — the same removal gate that
//! protects the event catalogue. A retired permission keeps its id and is marked deprecated; a role
//! synced from an older cloud that still references it stays meaningful.

use crate::error::DomainError;

/// The permission groups `docs/pos-spec.md` §9 names.
///
/// The dashboard renders permissions grouped by these, so the grouping is data the framework owns
/// rather than a UI convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum PermissionGroup {
    /// Taking and editing orders.
    Sales,
    /// Bills, discounts, comps, refunds, payment.
    Billing,
    /// Cash drawer and shift lifecycle.
    CashAndShifts,
    /// What a store has taken, as the till shows it.
    Reports,
    /// Menu and inventory maintenance.
    MenuAndInventory,
    /// Store-level administration: devices, configuration, staff at the store.
    StoreAdministration,
    /// Administration that only exists in the cloud: tenants, cross-store staff.
    CloudAdministration,
}

impl PermissionGroup {
    /// The group's `UPPER_SNAKE_CASE` token, for the snapshot and the dashboard.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Sales => "SALES",
            Self::Billing => "BILLING",
            Self::CashAndShifts => "CASH_AND_SHIFTS",
            Self::Reports => "REPORTS",
            Self::MenuAndInventory => "MENU_AND_INVENTORY",
            Self::StoreAdministration => "STORE_ADMINISTRATION",
            Self::CloudAdministration => "CLOUD_ADMINISTRATION",
        }
    }
}

/// How much damage a misused permission can do — the axis the dashboard's outlier analysis and the
/// PIN policy both read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum RiskLevel {
    /// Routine, low-consequence.
    Low,
    /// Worth an audit trail.
    Medium,
    /// A fraud or money vector: voids, comps, refunds, opening the drawer with no sale.
    High,
}

impl RiskLevel {
    /// The level's token.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

/// The built-in default roles.
///
/// These seed the matrix a fresh store starts with. **Roles themselves are data** (§9): a real
/// deployment edits role definitions in the cloud and syncs them to the edge as a
/// [`PermissionSet`]. This enum is only the vocabulary of the defaults, so a permission can declare
/// which of them it grants to out of the box, deny by default for the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Role {
    /// The tenant owner. Everything by default, but a permission an ADR grants to no role (ADR-0214).
    Owner,
    /// A store manager.
    Manager,
    /// A shift supervisor: some manager powers, on the floor.
    Supervisor,
    /// A cashier at the till.
    Cashier,
    /// A server on the floor.
    Server,
    /// Kitchen staff.
    Cook,
}

impl Role {
    /// Every built-in role.
    pub const ALL: &'static [Self] = &[
        Self::Owner,
        Self::Manager,
        Self::Supervisor,
        Self::Cashier,
        Self::Server,
        Self::Cook,
    ];

    /// The role's token.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Owner => "OWNER",
            Self::Manager => "MANAGER",
            Self::Supervisor => "SUPERVISOR",
            Self::Cashier => "CASHIER",
            Self::Server => "SERVER",
            Self::Cook => "COOK",
        }
    }
}

/// Everything a permission declares.
///
/// Carried by every [`Permission`] via [`Permission::meta`], which is generated from the one
/// [`permissions!`] declaration so it cannot drift.
#[derive(Debug, Clone, Copy)]
pub struct PermissionMeta {
    /// The stable id, `domain.resource.action` (`docs/naming-and-api.md`).
    pub id: &'static str,
    /// Which group it belongs to.
    pub group: PermissionGroup,
    /// How dangerous misuse is.
    pub risk: RiskLevel,
    /// Whether it always requires a PIN on the spot, whatever the role.
    pub pin_required: bool,
    /// The built-in roles that receive it by default. Deny by default: a role not listed here does
    /// not get it until a cloud-defined role grants it.
    pub default_roles: &'static [Role],
    /// A one-line description for the dashboard.
    pub description: &'static str,
}

/// Declares the fixed permission catalogue.
///
/// One entry per permission, each carrying the full metadata §9 requires. The macro generates the
/// [`Permission`] enum, its `ALL`, and [`Permission::meta`] from the same block, so adding a
/// permission is one entry that updates all three at once and cannot omit the default roles.
macro_rules! permissions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident {
                id: $id:literal,
                group: $group:ident,
                risk: $risk:ident,
                pin: $pin:literal,
                default_roles: [ $($role:ident),* $(,)? ],
                description: $description:literal $(,)?
            }
        ),+ $(,)?
    ) => {
        /// A permission from the fixed catalogue.
        ///
        /// Fieldless and `Copy`, so a [`PermissionSet`] can hold it as a single bit.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Permission {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl Permission {
            /// Every permission, in declaration order.
            pub const ALL: &'static [Self] = &[ $(Self::$variant),+ ];

            /// The permission's declared metadata.
            #[must_use]
            pub const fn meta(self) -> PermissionMeta {
                match self {
                    $(
                        Self::$variant => PermissionMeta {
                            id: $id,
                            group: PermissionGroup::$group,
                            risk: RiskLevel::$risk,
                            pin_required: $pin,
                            default_roles: &[ $(Role::$role),* ],
                            description: $description,
                        },
                    )+
                }
            }
        }
    };
}

permissions! {
    // ---- Sales ----
    /// Void a line after it has been fired to the kitchen.
    VoidFiredLine {
        id: "sales.line.void_fired",
        group: Sales,
        risk: High,
        pin: true,
        default_roles: [Supervisor, Manager, Owner],
        description: "Void a line after it has fired; prints a void ticket and records waste",
    },
    /// Add an ad-hoc open item with a typed name and price.
    AddOpenItem {
        id: "sales.item.open",
        group: Sales,
        risk: Medium,
        pin: false,
        default_roles: [Cashier, Server, Supervisor, Manager, Owner],
        description: "Add an open item with a typed name and price; always audited",
    },
    /// Mark an item unavailable (86) or restore it.
    MarkItemUnavailable {
        id: "sales.item.mark_unavailable",
        group: Sales,
        risk: Low,
        pin: false,
        default_roles: [Cashier, Server, Supervisor, Manager, Owner, Cook],
        description: "86 an item or restore it",
    },
    /// Transfer an order to another table.
    TransferOrder {
        id: "sales.order.transfer",
        group: Sales,
        risk: Low,
        pin: false,
        default_roles: [Server, Supervisor, Manager, Owner],
        description: "Move an order to another table",
    },
    /// Release a guest's QR order to the kitchen, or refuse it
    /// ([ADR-0116](../../../docs/adr/0116-the-qr-hold-is-derived-and-it-gates-firing.md)).
    ///
    /// No PIN: the intended actor is a server standing at the table, and a PIN prompt on every
    /// guest order would be abandoned within a shift — `Medium` risk with a named actor in the
    /// event is the trade ADR-0116 records. The default roles are the ones who may open an order
    /// in the first place.
    ConfirmQrOrder {
        id: "sales.order.confirm_qr",
        group: Sales,
        risk: Medium,
        pin: false,
        default_roles: [Cashier, Server, Supervisor, Manager, Owner],
        description: "Confirm or reject a guest's QR order",
    },
    /// Seat a table, mark it cleaned, or release one seated by mistake (ADR-0158).
    ManageTables {
        id: "sales.table.manage",
        group: Sales,
        risk: Low,
        pin: false,
        default_roles: [Server, Cashier, Supervisor, Manager, Owner],
        description: "Seat, clean or release a table",
    },
    /// Add a line to an order, or change a line's quantity (ADR-0158).
    AddLine {
        id: "sales.line.add",
        group: Sales,
        risk: Low,
        pin: false,
        default_roles: [Server, Cashier, Supervisor, Manager, Owner],
        description: "Add a line to an order or change its quantity",
    },
    /// Fire lines to the kitchen: one line, a course, or the whole order (ADR-0158).
    FireLines {
        id: "sales.line.fire",
        group: Sales,
        risk: Low,
        pin: false,
        default_roles: [Server, Cashier, Supervisor, Manager, Owner],
        description: "Send lines to the kitchen",
    },
    /// Bump a ticket on the kitchen board once its dishes are done (ADR-0158).
    BumpTicket {
        id: "sales.ticket.bump",
        group: Sales,
        risk: Low,
        pin: false,
        default_roles: [Cook, Supervisor, Manager, Owner],
        description: "Mark a ticket done on the kitchen board",
    },

    // ---- Billing ----
    /// Apply a discount within the role's ceiling.
    ApplyDiscount {
        id: "billing.discount.apply",
        group: Billing,
        risk: Medium,
        pin: false,
        default_roles: [Cashier, Server, Supervisor, Manager, Owner],
        description: "Apply a discount up to the role's configured ceiling",
    },
    /// Exceed the role's discount ceiling.
    OverrideDiscountCeiling {
        id: "billing.discount.override_ceiling",
        group: Billing,
        risk: High,
        pin: true,
        default_roles: [Manager, Owner],
        description: "Apply a discount above the role's ceiling",
    },
    /// Comp an item (give it away). Distinct from a discount and from a void.
    ApplyComp {
        id: "billing.comp.apply",
        group: Billing,
        risk: High,
        pin: true,
        default_roles: [Supervisor, Manager, Owner],
        description: "Comp an item; inventory is still consumed and cost is recorded",
    },
    /// Override a line price.
    OverridePrice {
        id: "billing.price.override",
        group: Billing,
        risk: High,
        pin: true,
        default_roles: [Manager, Owner],
        description: "Override a line price up to the role's price-override ceiling",
    },
    /// Void a whole bill.
    VoidBill {
        id: "billing.bill.void",
        group: Billing,
        risk: High,
        pin: true,
        default_roles: [Manager, Owner],
        description: "Void a bill; requires a reason and prints a void slip",
    },
    /// Void a paid bill whole: not [`Self::VoidBill`], as forgiving money already taken is another
    /// act (ADR-0184 decision 4). Migration 0087 granted it on `billing.bill.void`'s terms.
    VoidSettledBill {
        id: "billing.bill.void_settled",
        group: Billing,
        risk: High,
        pin: true,
        default_roles: [Manager, Owner],
        description: "Void a bill that was paid, whole; requires a reason and prints a void slip",
    },
    /// Issue a refund against a settled bill.
    IssueRefund {
        id: "billing.refund.issue",
        group: Billing,
        risk: High,
        pin: true,
        default_roles: [Manager, Owner],
        description: "Refund a settled bill at the store that issued it",
    },
    /// Reprint a receipt, marked COPY and counted.
    ReprintReceipt {
        id: "billing.receipt.reprint",
        group: Billing,
        risk: Medium,
        pin: false,
        default_roles: [Cashier, Supervisor, Manager, Owner],
        description: "Reprint a receipt; the copy is marked and the reprint is counted",
    },
    /// Open the bill on an order, as when the guests ask for it (ADR-0158).
    OpenBill {
        id: "billing.bill.open",
        group: Billing,
        risk: Low,
        pin: false,
        default_roles: [Server, Cashier, Supervisor, Manager, Owner],
        description: "Open a bill when the guests ask for it",
    },
    /// Split a bill into parts, or merge bills back into one (ADR-0158).
    SplitBill {
        id: "billing.bill.split",
        group: Billing,
        risk: Low,
        pin: false,
        default_roles: [Server, Cashier, Supervisor, Manager, Owner],
        description: "Split a bill or merge bills",
    },
    /// Take a payment and settle a bill (ADR-0158).
    TakePayment {
        id: "billing.payment.take",
        group: Billing,
        risk: Low,
        pin: false,
        default_roles: [Cashier, Server, Supervisor, Manager, Owner],
        description: "Take a payment and settle a bill",
    },
    /// Waive a waivable fee on one bill, citing a reason
    /// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 5).
    ///
    /// Money forgiven, as a discount above the ceiling is, so it is PIN-flagged and goes by default
    /// to the roles that may exceed the ceiling; any other role can be granted it with approval
    /// (ADR-0158 decision 4).
    WaiveFee {
        id: "billing.fee.waive",
        group: Billing,
        risk: High,
        pin: true,
        default_roles: [Manager, Owner],
        description: "Waive a waivable fee on one bill; requires a reason",
    },

    // ---- Cash and shifts ----
    /// Open the cash drawer outside a sale.
    OpenDrawerNoSale {
        id: "cash.drawer.open_no_sale",
        group: CashAndShifts,
        risk: High,
        pin: true,
        default_roles: [Supervisor, Manager, Owner],
        description: "Open the drawer with no sale in progress; always logged",
    },
    /// Open a cash shift with a starting float.
    OpenShift {
        id: "cash.shift.open",
        group: CashAndShifts,
        risk: Low,
        pin: false,
        default_roles: [Cashier, Supervisor, Manager, Owner],
        description: "Open a shift with a starting float",
    },
    /// Close a cash shift (blind count).
    CloseShift {
        id: "cash.shift.close",
        group: CashAndShifts,
        risk: Medium,
        pin: false,
        default_roles: [Cashier, Supervisor, Manager, Owner],
        description: "Close a shift; the count is blind",
    },
    /// Start, count or close a drawer that is not the till's own, from any till: one, several or
    /// all at once ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions
    /// 3 and 13).
    ///
    /// `cash.shift.open` and `cash.shift.close` cover a till's own drawer, so a cashier keeps to
    /// their own unless a holder of this approves the act. Where a store keeps one drawer, every
    /// till's, nothing needs it. PIN-flagged with the default roles of `cash.drawer.open_no_sale`,
    /// and migration 0084 gave it once to every role that existed on the terms it grants that one.
    /// `Medium`, as closing a shift is, and not `High`: it starts, counts or closes a drawer's
    /// session, which the shift's own events record, and it moves no cash and opens no drawer.
    ManageOtherTill {
        id: "cash.shift.manage_other_till",
        group: CashAndShifts,
        risk: Medium,
        pin: true,
        default_roles: [Supervisor, Manager, Owner],
        description: "Start, count or close another till's drawer from any till, one or many at once",
    },
    /// Act, for one act, on a drawer assigned to someone else: take cash into it, pay in or out of
    /// it, open it without a sale, count it or close it
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 9).
    ///
    /// Where a store assigns each drawer, for its session, to the person who answers for it, only
    /// that person acts on it, and anyone else needs this, held directly or approved for the one act
    /// by a holder. Where drawers are shared, nothing needs it. PIN-flagged with the default roles of
    /// `cash.drawer.open_no_sale`, and migration 0086 gave it once to every role that existed on the
    /// terms it grants that one. `High`, as opening a drawer without a sale is, and not `Medium` as
    /// managing another till's drawer is: it reaches cash another person answers for, which is what
    /// the assignment is there to stop.
    OverrideDrawerAssignment {
        id: "cash.drawer.override_assignment",
        group: CashAndShifts,
        risk: High,
        pin: true,
        default_roles: [Supervisor, Manager, Owner],
        description: "Act once on a drawer assigned to someone else: take cash, pay in or out, open, count or close it",
    },
    /// Record a paid-in or paid-out with a reason.
    RecordCashMovement {
        id: "cash.movement.record",
        group: CashAndShifts,
        risk: Medium,
        pin: false,
        default_roles: [Cashier, Supervisor, Manager, Owner],
        description: "Record a paid-in or paid-out with a reason",
    },

    // ---- Reports ----
    /// See what the store has taken today, on the Today screen
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 2).
    ///
    /// The store's figure and nothing finer: the sum of the day's settled bills and how many there
    /// were, never by person or by till, because takings per person would be monitoring staff.
    /// Takings are confidential, so out of the box only the approvers who close shifts hold it: the
    /// supervisor, the manager and the owner. A cashier, or any other role, sees takings only where
    /// an owner grants it. Migration 0079 gave it once, on the same rule, to every role that existed
    /// and granted both `cash.shift.close` and a PIN-flagged permission directly, so an existing
    /// tenant and a new one agree on who holds it.
    ViewTakings {
        id: "reports.takings.view",
        group: Reports,
        risk: Medium,
        pin: false,
        default_roles: [Supervisor, Manager, Owner],
        description: "See what the store has taken today, as one figure for the store",
    },
    /// Read a shift's X at the till, with the cash its drawer should hold where the store counts
    /// blind ([ADR-0187](../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
    /// decision 4).
    ///
    /// An X resets nothing and moves no money, so no PIN. Anybody who sees takings reads an X; this
    /// is what adds the expected cash to it in a blind-close store, which would otherwise tell a
    /// cashier what their count should come to. Out of the box the manager and the owner hold it;
    /// migration 0088 gives it once to every role that grants `admin.config.edit` directly.
    ReadShiftX {
        id: "reports.shift.read_x",
        group: Reports,
        risk: Medium,
        pin: false,
        default_roles: [Manager, Owner],
        description: "Read a shift's X at the till, with the cash its drawer should hold",
    },

    // ---- Menu and inventory ----
    /// Perform a stocktake.
    PerformStocktake {
        id: "inventory.stocktake.perform",
        group: MenuAndInventory,
        risk: Medium,
        pin: false,
        default_roles: [Supervisor, Manager, Owner],
        description: "Record a physical count; the delta is against the projection at count time",
    },
    /// Record a stock receipt.
    RecordStockReceipt {
        id: "inventory.receipt.record",
        group: MenuAndInventory,
        risk: Low,
        pin: false,
        default_roles: [Supervisor, Manager, Owner],
        description: "Record goods received",
    },
    /// Record waste.
    RecordWaste {
        id: "inventory.waste.record",
        group: MenuAndInventory,
        risk: Low,
        pin: false,
        default_roles: [Cook, Supervisor, Manager, Owner],
        description: "Record spoiled or dropped stock",
    },
    /// Edit the menu.
    EditMenu {
        id: "menu.item.edit",
        group: MenuAndInventory,
        risk: Medium,
        pin: false,
        default_roles: [Manager, Owner],
        description: "Edit menu items, prices and recipes",
    },

    // ---- Store administration ----
    /// Approve or revoke a device.
    ManageDevices {
        id: "admin.device.manage",
        group: StoreAdministration,
        risk: High,
        pin: false,
        default_roles: [Manager, Owner],
        description: "Approve, rename or revoke a device",
    },
    /// Edit store configuration.
    EditStoreConfig {
        id: "admin.config.edit",
        group: StoreAdministration,
        risk: Medium,
        pin: false,
        default_roles: [Manager, Owner],
        description: "Edit store-level configuration",
    },
    /// Choose which of the package versions HQ allows the store runs
    /// ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 2).
    ///
    /// A store's people choose; they never edit. The choice is a write to cloud configuration, so
    /// no till or edge route checks it: until a store's own people can sign in to the console (a
    /// later ADR), the console's choose route is gated by `console.config.publish`, and this
    /// permission is the catalogue's record of who should hold it.
    ChoosePackage {
        id: "admin.package.choose",
        group: StoreAdministration,
        risk: Medium,
        pin: false,
        default_roles: [Manager, Owner],
        description: "Choose which allowed package version the store runs",
    },

    // ---- Custom data ----
    /// Enter and change custom records and custom field values
    /// ([ADR-0206](../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
    /// decision 7). A field on an existing object also needs that object's own permission.
    WriteDataRecord {
        id: "data.record.write",
        group: Sales,
        risk: Low,
        pin: false,
        default_roles: [Server, Cashier, Supervisor, Manager, Owner],
        description: "Enter and change custom records and custom field values",
    },
    /// Show a personal custom value again after it was saved, on a till or by subject (ADR-0206
    /// decisions 5 and 7). Entering one does not need it.
    ReadPersonalData {
        id: "data.personal.read",
        group: StoreAdministration,
        risk: High,
        pin: false,
        default_roles: [Manager, Owner],
        description: "Show a saved personal custom value again; every read is audited",
    },

    // ---- External lookups ----
    /// Run a lookup through the cloud gateway from the till
    /// ([ADR-0214](../../../docs/adr/0214-a-tills-external-lookup-is-one-catalogue-act.md)
    /// decision 2). No role holds it by default: a tenant grants it to the roles it chooses.
    RunLookup {
        id: "integrations.lookup.run",
        group: Sales,
        risk: Medium,
        pin: false,
        default_roles: [],
        description: "Run a lookup the tenant declared through the cloud gateway; nothing is kept",
    },

    // ---- Cloud administration ----
    /// Manage tenants, brands and cross-store settings.
    ManageTenant {
        id: "cloud.tenant.manage",
        group: CloudAdministration,
        risk: High,
        pin: false,
        default_roles: [Owner],
        description: "Manage tenants, brands and cross-store configuration",
    },
    /// Manage staff across stores.
    ManageStaff {
        id: "cloud.staff.manage",
        group: CloudAdministration,
        risk: High,
        pin: false,
        default_roles: [Owner],
        description: "Manage staff and role assignments across stores",
    },
}

// The bitset in `PermissionSet` holds one bit per permission by enum discriminant, so the catalogue
// cannot outgrow a u64 without a deliberate change here.
const _: () = assert!(
    Permission::ALL.len() <= 64,
    "PermissionSet is a u64 bitset; a 65th permission needs a wider representation"
);

/// A set of permissions, as a `u64` bitset keyed by enum discriminant.
///
/// `Copy` and cheap to test, because [`require`] runs on every gated action. At the edge this is
/// built from the role definition the cloud synced; the framework never consults a role string.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionSet(u64);

impl PermissionSet {
    /// The empty set — grants nothing. This is what "deny by default" is built on.
    pub const EMPTY: Self = Self(0);

    /// The bit for one permission.
    const fn bit(permission: Permission) -> u64 {
        1_u64 << (permission as u64)
    }

    /// Whether the set grants `permission`.
    #[must_use]
    pub const fn contains(self, permission: Permission) -> bool {
        self.0 & Self::bit(permission) != 0
    }

    /// Adds a permission.
    pub const fn insert(&mut self, permission: Permission) {
        self.0 |= Self::bit(permission);
    }

    /// Returns the set with `permission` added, for builder-style construction.
    #[must_use]
    pub const fn with(mut self, permission: Permission) -> Self {
        self.insert(permission);
        self
    }

    /// How many permissions the set grants.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// Whether the set grants nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl FromIterator<Permission> for PermissionSet {
    fn from_iter<I: IntoIterator<Item = Permission>>(iter: I) -> Self {
        let mut set = Self::EMPTY;
        for permission in iter {
            set.insert(permission);
        }
        set
    }
}

/// The default permissions a built-in role receives.
///
/// Derived from each permission's declared `default_roles`, so this matrix and the declarations are
/// one source of truth. A real deployment overrides it with cloud-defined roles; this is the seed.
#[must_use]
pub fn default_grants(role: Role) -> PermissionSet {
    Permission::ALL
        .iter()
        .copied()
        .filter(|permission| permission.meta().default_roles.contains(&role))
        .collect()
}

/// What a successful [`require`] returns: the action is allowed, and whether it still needs a PIN.
///
/// A PIN-flagged permission is *granted* to the role but must be re-authorised on the spot (§9), so
/// this is not a denial — it is an allow that carries an obligation the edge fulfils.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    /// Whether the edge must collect a PIN before performing the action.
    pub pin_required: bool,
}

/// The single authorisation gate. Every gated action passes through here.
///
/// Deny by default: a permission the set does not grant is refused, whatever role produced the set.
/// Ad-hoc role checks elsewhere are banned (§9), which is why this is the only function that reads a
/// [`PermissionSet`] against a [`Permission`].
///
/// # Errors
///
/// [`DomainError::PermissionDenied`] naming the permission id, when the set does not grant it.
pub fn require(permission: Permission, granted: PermissionSet) -> Result<Grant, DomainError> {
    if granted.contains(permission) {
        Ok(Grant {
            pin_required: permission.meta().pin_required,
        })
    } else {
        Err(DomainError::PermissionDenied {
            permission: permission.meta().id,
        })
    }
}

/// One fact per line, sorted, for `docs/snapshots/permissions.txt`.
///
/// The bare id line is the contract the removal gate protects; the tabbed lines are metadata that
/// may change (a permission may be re-grouped or have its default roles adjusted) and are treated as
/// mutable by `cargo xtask snapshot`.
#[must_use]
pub fn render_snapshot() -> String {
    let mut lines = Vec::new();
    for permission in Permission::ALL {
        let meta = permission.meta();
        lines.push(meta.id.to_owned());
        lines.push(format!("{}\tgroup={}", meta.id, meta.group.as_token()));
        lines.push(format!("{}\trisk={}", meta.id, meta.risk.as_token()));
        lines.push(format!("{}\tpin_required={}", meta.id, meta.pin_required));
        for role in meta.default_roles {
            lines.push(format!("{}\tdefault_role={}", meta.id, role.as_token()));
        }
    }
    lines.sort();
    let mut out =
        String::from("# Generated from crates/pos-core/src/permission.rs — do not edit.\n");
    out.push_str(
        "# One line per fact. A bare id is a contract; tabbed lines are mutable metadata.\n",
    );
    out.push_str(&lines.join("\n"));
    out.push('\n');
    out
}

/// The documented permission matrix as Markdown: every permission against every built-in role.
#[must_use]
pub fn render_matrix_markdown() -> String {
    let mut out = String::from("# Permission matrix\n\n");
    out.push_str(
        "Generated from `crates/pos-core/src/permission.rs`. Do not edit by hand — run \
         `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core`. A `✓` is a default grant; roles are data, \
         so a deployment overrides these in the cloud.\n\n",
    );

    out.push_str("| id | group | risk | PIN |");
    for role in Role::ALL {
        out.push(' ');
        out.push_str(role.as_token());
        out.push_str(" |");
    }
    out.push('\n');

    out.push_str("|---|---|---|---|");
    for _ in Role::ALL {
        out.push_str("---|");
    }
    out.push('\n');

    for permission in Permission::ALL {
        let meta = permission.meta();
        out.push_str("| `");
        out.push_str(meta.id);
        out.push_str("` | ");
        out.push_str(meta.group.as_token());
        out.push_str(" | ");
        out.push_str(meta.risk.as_token());
        out.push_str(" | ");
        out.push_str(if meta.pin_required { "yes" } else { "·" });
        out.push_str(" |");
        for role in Role::ALL {
            out.push(' ');
            out.push_str(if meta.default_roles.contains(role) {
                "✓"
            } else {
                "·"
            });
            out.push_str(" |");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests;
