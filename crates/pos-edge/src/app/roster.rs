// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Who this edge is and who may act on it: the store's identity, and the staff roster a sign-in and
//! an approval are checked against.

use std::collections::BTreeMap;

use pos_core::permission::{Permission, PermissionSet};
use pos_proto::ids::{BrandId, EmployeeId, StoreId, TenantId};
use pos_proto::money::Money;
use pos_proto::ulid::Ulid;

/// Which tenant, brand and store this edge is — the envelope context every event carries. All three
/// are identifiers, not PII.
///
/// Only the store id is the box's own knowledge. The tenant and brand are
/// [`UNASSIGNED`](Self::UNASSIGNED): a box has no way to learn them (activation hands it a device id
/// and a credential, [ADR-0003](../../../docs/adr/0003-cattle-not-pets.md)), and the cloud stamps
/// both from its store registry as it ingests
/// ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)). This doc used to say
/// activation supplied them; it never did.
#[derive(Debug, Clone, Copy)]
pub struct StoreIdentity {
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Owning brand.
    pub brand_id: BrandId,
    /// This store.
    pub store_id: StoreId,
}

impl StoreIdentity {
    /// The tenant and brand a store puts on its own events: **neither**.
    ///
    /// The nil ULID, and deliberately so ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    /// A box does not know which tenant owns it and has no way to find out — activation hands it a
    /// device id and a credential, not an org chart — so the honest value is one that reads as
    /// "nobody". Until that ADR this was `ULID(1)`, which reads as a tenant that might exist, and
    /// every store in the fleet stamped it: the column row-level isolation is defined on held one
    /// constant, fleet-wide.
    ///
    /// The cloud overwrites both from its store registry as it ingests, so the *stored* log is
    /// correct whatever a box asserts. This value is what the log holds only in a store's own SQLite,
    /// where there is exactly one tenant and the question does not arise.
    pub const UNASSIGNED: Ulid = Ulid::from_u128(0);

    /// The identity for a store: its own id, and a tenant and brand it does not claim to know
    /// ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    #[must_use]
    pub fn for_store(store_id: StoreId) -> Self {
        Self {
            tenant_id: TenantId::new(Self::UNASSIGNED),
            brand_id: BrandId::new(Self::UNASSIGNED),
            store_id,
        }
    }
}

/// One staff member as the store authorises them ([ADR-0070](../../../docs/adr/0070-people-and-access.md)):
/// the permission set their assigned role grants, and the Argon2id PIN hash the edge verifies against
/// offline ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)). Both arrive in the store's
/// published `permissions` config node — the store never invents them.
#[derive(Debug, Clone)]
pub struct StaffAuth {
    /// The employee this badge code signs in as — the identity a command runs under once they sign in
    /// (S0b, [ADR-0084](../../../docs/adr/0084-device-authentication.md)). `None` if the published node
    /// carried no (or a malformed) id, in which case the member cannot sign in — the edge never invents
    /// an identity to act as.
    pub employee_id: Option<EmployeeId>,
    /// What the person's roles grant them **directly** (§9): they act on these alone.
    pub permissions: PermissionSet,
    /// What their roles grant only **with approval**: each act needs another person, who holds the
    /// permission directly, to approve it with their code and PIN
    /// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
    /// decision 4). Read only where the store enforces each person's own set
    /// ([`EdgeSession::permissions_enforced`]); never overlaps [`Self::permissions`].
    pub permissions_with_approval: PermissionSet,
    /// How much they may discount before it needs a manager, from their role's configured ceiling
    /// (ADR-0070's `permissions` node).
    ///
    /// `None` is "no ceiling configured", which `decide_bill` reads as **zero** — every discount
    /// then needs `billing.discount.override_ceiling` and a manager's PIN. That is what every store
    /// carries today, and the day a tenant sets one a server's small discount goes through with no
    /// code change.
    pub discount_ceiling: Option<Money>,
    /// The Argon2id PHC hash of their PIN, or `None` if none is set (they cannot sign in until one is).
    pub pin_phc: Option<String>,
}

/// The store's staff, keyed by the badge `code` a person types, as published from the cloud
/// ([ADR-0070](../../../docs/adr/0070-people-and-access.md)). This is the roster the edge authorises
/// against — replacing any local, out-of-band staff list with the set the console published.
#[derive(Debug, Clone, Default)]
pub struct StaffRoster {
    by_code: BTreeMap<String, StaffAuth>,
}

impl StaffRoster {
    /// An empty roster — the bootstrap default until the cloud publishes the `permissions` node.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds (or replaces) a staff member under their `code`.
    pub fn insert(&mut self, code: impl Into<String>, auth: StaffAuth) {
        self.by_code.insert(code.into(), auth);
    }

    /// The staff member under `code`, if any.
    #[must_use]
    pub fn get(&self, code: &str) -> Option<&StaffAuth> {
        self.by_code.get(code)
    }

    /// What a signed-in person may do, looked up by the identity their session carries.
    ///
    /// The roster is keyed by the badge *code* a person types, because that is what sign-in has in
    /// hand. Everything after sign-in has an [`EmployeeId`] instead — it is what an
    /// [`Actor`](pos_core::decision::Actor) carries — so a route that gates on a permission without
    /// re-authorising needs this direction. A linear scan over a store's staff is the right cost: a
    /// roster is tens of people, and the alternative is a second index to keep in step with the
    /// published node.
    ///
    /// `None` for an id no published member holds, which is the safe answer: an identity the roster
    /// does not know grants nothing.
    #[must_use]
    pub fn permissions_for(&self, employee_id: EmployeeId) -> Option<PermissionSet> {
        self.member(employee_id).map(|auth| auth.permissions)
    }

    /// Whether the person's own roles grant `permission` directly, whatever the store's
    /// enforcement switch says.
    ///
    /// For the acts a store decides from the person's own role in every store: managing devices
    /// (ADR-0118, ADR-0112), seeing the day's takings (ADR-0160), and seeing what another till's
    /// drawer should hold (ADR-0167). Read from the roster the cloud
    /// published, never from anything a request carried: the store authorises against the set the
    /// console published and invents nothing (ADR-0070). `false` for an identity the roster does not
    /// know.
    #[must_use]
    pub fn grants(&self, employee_id: EmployeeId, permission: Permission) -> bool {
        self.permissions_for(employee_id)
            .is_some_and(|granted| granted.contains(permission))
    }

    /// What a signed-in person may do only with another person's approval
    /// ([`StaffAuth::permissions_with_approval`]). Empty for an id no published member holds, which
    /// grants nothing either way.
    #[must_use]
    pub fn permissions_with_approval_for(&self, employee_id: EmployeeId) -> PermissionSet {
        self.member(employee_id)
            .map_or(PermissionSet::EMPTY, |auth| auth.permissions_with_approval)
    }

    /// What a signed-in person may discount before it needs a manager, or `None` when their role
    /// configures no ceiling — which the domain reads as zero.
    ///
    /// The two `None`s here mean the same thing on purpose: a person the roster does not know and a
    /// person whose role sets no ceiling both discount nothing without a manager. An identity that
    /// grants no permissions cannot be handed an allowance.
    #[must_use]
    pub fn discount_ceiling_for(&self, employee_id: EmployeeId) -> Option<Money> {
        self.member(employee_id)
            .and_then(|auth| auth.discount_ceiling)
    }

    /// The published member acting as `employee_id`, if the roster knows one.
    ///
    /// A linear scan over a store's staff is the right cost: a roster is tens of people, and the
    /// alternative is a second index to keep in step with the published node.
    fn member(&self, employee_id: EmployeeId) -> Option<&StaffAuth> {
        self.by_code
            .values()
            .find(|auth| auth.employee_id == Some(employee_id))
    }

    /// How many staff the roster holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_code.len()
    }

    /// Whether the roster is empty (no staff published yet).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_code.is_empty()
    }

    /// Whether anyone can sign in: at least one member has the id and the PIN a sign-in needs.
    ///
    /// `false` until the console has published someone with a PIN for this store, and on a box that
    /// has not been activated and so has received no roster at all. The sign-in screen says so
    /// rather than refusing every code as a wrong one, which is all a refusal can say.
    #[must_use]
    pub fn sign_in_ready(&self) -> bool {
        self.by_code
            .values()
            .any(|auth| auth.employee_id.is_some() && auth.pin_phc.is_some())
    }

    /// Authorises a sign-in: verifies `pin` against the published Argon2id hash for `code`, returning
    /// the granted [`PermissionSet`] on success. `None` for an unknown code, a member with no PIN set,
    /// or a wrong PIN — so a bad sign-in never yields any permissions. The rate-limit that turns
    /// repeated failures into a lockout is [`crate::auth::Lockout`] (ADR-0030); this is the pure
    /// verify against the published set.
    #[must_use]
    pub fn authorise(&self, code: &str, pin: &str) -> Option<PermissionSet> {
        let auth = self.by_code.get(code)?;
        let phc = auth.pin_phc.as_deref()?;
        crate::auth::verify_pin(phc, pin).then_some(auth.permissions)
    }

    /// The approver `code` names for a **step-up** on one act: who they are, the Argon2id hash to
    /// verify their PIN against, and what their role grants.
    ///
    /// [`authorise`](Self::authorise) answers the sign-in question — "what may this person do" — and
    /// throws the identity away, because a sign-in goes on to carry it in the session. A step-up is
    /// the other shape: the person authorising is *not* the person acting, so their identity is the
    /// whole point. `docs/pos-spec.md` §11.4 makes a manager-PIN override a fraud control, and
    /// `security.permission.overridden` records the approver as `approver_employee_id` while the
    /// envelope keeps the actor who performed the act.
    ///
    /// The PIN is not verified here. The caller verifies it under the lockout
    /// ([`crate::auth::Lockout::authenticate`], ADR-0030), so a wrong approval PIN counts against
    /// the approver exactly as a wrong sign-in PIN does.
    ///
    /// `None` for an unknown code, a member with no id, or a member with no PIN set, so a step-up
    /// under any of them authorises nothing and names nobody.
    #[must_use]
    pub fn approver(&self, code: &str) -> Option<(EmployeeId, &str, PermissionSet)> {
        let auth = self.by_code.get(code)?;
        Some((
            auth.employee_id?,
            auth.pin_phc.as_deref()?,
            auth.permissions,
        ))
    }

    /// The sign-in credentials under `code`: the employee the code acts as and the Argon2id hash to
    /// verify the PIN against. `None` for an unknown code, a member with no id, or one with no PIN set
    /// — the three cases a sign-in cannot proceed from. The rate-limited verification itself is
    /// [`crate::auth::Lockout::authenticate`] (ADR-0030); this only resolves what to verify against, so
    /// the HTTP sign-in route holds no roster detail (S0b, ADR-0084).
    #[must_use]
    pub fn credentials(&self, code: &str) -> Option<(EmployeeId, &str)> {
        let auth = self.by_code.get(code)?;
        Some((auth.employee_id?, auth.pin_phc.as_deref()?))
    }
}
