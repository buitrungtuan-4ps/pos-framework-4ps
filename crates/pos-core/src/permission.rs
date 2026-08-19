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

/// The six permission groups `docs/pos-spec.md` §9 names.
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
    /// The tenant owner. Everything, by default.
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
    /// Record a paid-in or paid-out with a reason.
    RecordCashMovement {
        id: "cash.movement.record",
        group: CashAndShifts,
        risk: Medium,
        pin: false,
        default_roles: [Cashier, Supervisor, Manager, Owner],
        description: "Record a paid-in or paid-out with a reason",
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
mod tests {
    use super::{
        Grant, Permission, PermissionSet, Role, default_grants, render_matrix_markdown,
        render_snapshot, require,
    };
    use crate::error::DomainError;

    #[test]
    fn deny_by_default() {
        // The empty set grants nothing — the property every offline authorisation rests on.
        for permission in Permission::ALL {
            assert!(matches!(
                require(*permission, PermissionSet::EMPTY),
                Err(DomainError::PermissionDenied { .. })
            ));
        }
    }

    #[test]
    fn a_granted_permission_is_allowed_and_carries_its_pin_flag() {
        let granted = PermissionSet::EMPTY.with(Permission::VoidFiredLine);
        let grant = require(Permission::VoidFiredLine, granted).expect("granted");
        assert_eq!(
            grant,
            Grant { pin_required: true },
            "voiding a fired line always needs a PIN"
        );

        let low = PermissionSet::EMPTY.with(Permission::OpenShift);
        let grant = require(Permission::OpenShift, low).expect("granted");
        assert!(!grant.pin_required, "opening a shift does not");
    }

    #[test]
    fn granting_one_permission_does_not_grant_another() {
        let granted = PermissionSet::EMPTY.with(Permission::OpenShift);
        assert!(matches!(
            require(Permission::VoidBill, granted),
            Err(DomainError::PermissionDenied { permission }) if permission == "billing.bill.void"
        ));
    }

    #[test]
    fn the_owner_default_role_gets_everything() {
        // Not a rule the spec mandates, but a sane default and a guard that default_grants works:
        // every permission lists Owner, so the Owner seed is the full catalogue.
        let owner = default_grants(Role::Owner);
        for permission in Permission::ALL {
            assert!(
                owner.contains(*permission),
                "Owner should get {}",
                permission.meta().id
            );
        }
        assert_eq!(owner.len() as usize, Permission::ALL.len());
    }

    #[test]
    fn a_cook_does_not_get_money_permissions() {
        let cook = default_grants(Role::Cook);
        assert!(!cook.contains(Permission::VoidBill));
        assert!(!cook.contains(Permission::IssueRefund));
        assert!(!cook.contains(Permission::OpenDrawerNoSale));
        // But does get to record waste and 86 an item.
        assert!(cook.contains(Permission::RecordWaste));
        assert!(cook.contains(Permission::MarkItemUnavailable));
    }

    #[test]
    fn every_id_is_distinct_and_dotted() {
        // ids are the contract in the snapshot, so a duplicate or a malformed one is a real bug.
        let mut ids: Vec<&str> = Permission::ALL.iter().map(|p| p.meta().id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two permissions share an id");
        for permission in Permission::ALL {
            let id = permission.meta().id;
            let segments: Vec<&str> = id.split('.').collect();
            assert_eq!(segments.len(), 3, "{id} is not domain.resource.action");
            assert!(
                segments.iter().all(|s| {
                    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                }),
                "{id} is not lower snake_case"
            );
        }
    }

    #[test]
    fn high_risk_money_actions_require_a_pin() {
        // A weak but useful invariant: the money vectors the fraud section names must be
        // PIN-flagged, so nobody quietly ships one that is not.
        for permission in [
            Permission::VoidFiredLine,
            Permission::ApplyComp,
            Permission::VoidBill,
            Permission::IssueRefund,
            Permission::OpenDrawerNoSale,
            Permission::OverridePrice,
        ] {
            assert!(
                permission.meta().pin_required,
                "{} must need a PIN",
                permission.meta().id
            );
        }
    }

    #[test]
    fn the_snapshot_matches_the_committed_file() {
        let rendered = render_snapshot();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/snapshots/permissions.txt");
        if std::env::var_os("POS_UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&path, &rendered).expect("write permissions snapshot");
        }
        let committed = std::fs::read_to_string(&path).expect(
            "docs/snapshots/permissions.txt exists; regenerate with POS_UPDATE_SNAPSHOTS=1",
        );
        assert_eq!(
            rendered, committed,
            "regenerate with POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core"
        );
    }

    #[test]
    fn the_matrix_doc_matches_the_committed_file() {
        let rendered = render_matrix_markdown();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/permissions.md");
        if std::env::var_os("POS_UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&path, &rendered).expect("write permission matrix");
        }
        let committed = std::fs::read_to_string(&path)
            .expect("docs/permissions.md exists; regenerate with POS_UPDATE_SNAPSHOTS=1");
        assert_eq!(
            rendered, committed,
            "regenerate with POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core"
        );
    }
}
