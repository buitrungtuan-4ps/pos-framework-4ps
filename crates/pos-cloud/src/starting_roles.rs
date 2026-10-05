// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The six roles a new tenant starts with
//! ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md), Rollout).
//!
//! A tenant the console creates gets one editable role for each of `pos-core`'s built-in roles:
//! owner, manager, supervisor, cashier, server and cook ([`Role::ALL`]). Each grants what the
//! permission catalogue's `default_roles` say, the way migration 0074 gave the roles that already
//! existed their approvals:
//!
//! - a permission a role is a default for is granted **directly**;
//! - a **PIN-flagged** permission it is not a default for is granted **with approval**, so its
//!   holders can do it with somebody else's code and PIN, as any PIN-flagged act at a store goes
//!   today;
//! - any other permission is not granted.
//!
//! No role has a discount ceiling, which the edge reads as zero, as it does for every role nobody
//! has set one on. The tenant may rename, change or archive any of the six; they are ordinary role
//! templates from the moment they are written. A tenant that existed before this is not touched.
//!
//! **Names are console copy.** A role carries one name, not a translation key, so each is written
//! in the language the tenant is created in, from the console's own language packs
//! ([ADR-0020](../../../docs/adr/0020-i18n-runtime.md)): the keys `people.startingRole.<role>` in
//! `dashboard/src/i18n`, compiled in here, translated and parity-checked where every other console
//! string is. A language the packs do not carry names them in English, the fallback.
//!
//! **Idempotent.** Each starting role's id is derived from the tenant's ([`starting_role_id`]), so
//! seeding a tenant again finds the six already there and writes nothing: a role an owner has since
//! renamed or changed is never written over, and none is created twice.

use core::future::Future;
use core::pin::Pin;
use std::collections::BTreeMap;
use std::sync::LazyLock;

use pos_core::permission::{Permission, Role};
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;

use crate::people::{NewRoleTemplate, RoleTemplateId, RoleTemplateStore, RoleTemplateStoreError};

/// The console's English language pack, the fallback every name resolves to in the end.
const CONSOLE_EN: &str = include_str!("../../../dashboard/src/i18n/en.json");

/// The console's Vietnamese language pack.
const CONSOLE_VI: &str = include_str!("../../../dashboard/src/i18n/vi.json");

/// The packs by language, parsed once. A pack that does not parse reads as empty, which falls
/// through to English and then to the role's token; the tests prove neither is reached.
static CONSOLE_PACKS: LazyLock<BTreeMap<&'static str, BTreeMap<String, String>>> =
    LazyLock::new(|| {
        [("en", CONSOLE_EN), ("vi", CONSOLE_VI)]
            .into_iter()
            .map(|(language, pack)| (language, serde_json::from_str(pack).unwrap_or_default()))
            .collect()
    });

/// What seeding a tenant's starting roles did: the ids it wrote, and those it found already there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeededRoles {
    /// The starting roles written by this call.
    pub created: Vec<RoleTemplateId>,
    /// The starting roles the tenant already had, left as they are.
    pub kept: Vec<RoleTemplateId>,
}

/// The id the starting `role` of `tenant` takes: the tenant's own ULID with the last byte of its
/// randomness replaced by the role's place in [`Role::ALL`], counted from one.
///
/// Derived rather than minted, so seeding the same tenant twice addresses the same six rows, and
/// the second time finds them. Role ids need only be unique among role templates: two tenants'
/// starting roles could only share one if the tenants' ULIDs agreed in every bit but the last
/// byte, one chance in 2^72.
#[must_use]
pub fn starting_role_id(tenant: TenantId, place: u8) -> RoleTemplateId {
    let tenant = tenant.as_ulid().to_u128();
    RoleTemplateId::new(Ulid::from_u128((tenant & !0xFF) | u128::from(place)))
}

/// What the starting `role` grants: the permissions it holds directly, and those it holds only with
/// approval, each sorted.
///
/// A permission `role` is a default for is granted directly; a PIN-flagged one it is not a default
/// for is granted with approval; anything else is not granted.
#[must_use]
pub fn starting_role_grants(role: Role) -> (Vec<String>, Vec<String>) {
    let mut directly = Vec::new();
    let mut with_approval = Vec::new();
    for permission in Permission::ALL {
        let meta = permission.meta();
        if meta.default_roles.contains(&role) {
            directly.push(meta.id.to_owned());
        } else if meta.pin_required {
            with_approval.push(meta.id.to_owned());
        }
    }
    directly.sort_unstable();
    with_approval.sort_unstable();
    (directly, with_approval)
}

/// The name the starting `role` is given, in `language` where the console's packs carry it and in
/// English otherwise.
///
/// `language` is a language tag such as `vi` or `vi-VN`; only its primary subtag is read. The role's
/// own token (`OWNER`, …) is the last resort, for a pack that has lost the key, which the tests
/// keep from happening.
#[must_use]
pub fn starting_role_name(role: Role, language: Option<&str>) -> String {
    let key = format!(
        "people.startingRole.{}",
        role.as_token().to_ascii_lowercase()
    );
    let primary = language
        .and_then(|tag| tag.trim().split(['-', '_']).next())
        .map(str::to_ascii_lowercase);
    let named_in = |language: &str| {
        CONSOLE_PACKS
            .get(language)
            .and_then(|pack| pack.get(&key))
            .filter(|name| !name.trim().is_empty())
            .cloned()
    };
    primary
        .as_deref()
        .and_then(named_in)
        .or_else(|| named_in("en"))
        .unwrap_or_else(|| role.as_token().to_owned())
}

/// Writes whichever of `tenant`'s six starting roles it does not have yet, named in `language`.
///
/// # Errors
///
/// [`RoleTemplateStoreError`] if a read or a write fails. The roles written before it stay written,
/// and seeding again writes the rest.
pub async fn seed_starting_roles<P>(
    people: &P,
    tenant: TenantId,
    language: Option<&str>,
) -> Result<SeededRoles, RoleTemplateStoreError>
where
    P: RoleTemplateStore + Sync,
{
    let mut seeded = SeededRoles::default();
    for (place, role) in (1_u8..).zip(Role::ALL.iter().copied()) {
        let role_template_id = starting_role_id(tenant, place);
        if people.get(tenant, role_template_id).await?.is_some() {
            seeded.kept.push(role_template_id);
            continue;
        }
        let (permissions, permissions_with_approval) = starting_role_grants(role);
        let template = NewRoleTemplate {
            role_template_id,
            tenant_id: tenant,
            name: starting_role_name(role, language),
            permissions,
            permissions_with_approval,
            discount_ceiling_minor: None,
        };
        people.create(&template).await?;
        seeded.created.push(role_template_id);
    }
    Ok(seeded)
}

/// Seeds a new tenant's starting roles, behind an object-safe seam so the registry routes can hold
/// one without a role-store type parameter, as they hold their audit recorder.
pub trait StartingRoles: Send + Sync {
    /// Seeds `tenant`'s starting roles, named in `language` ([`seed_starting_roles`]).
    fn seed<'a>(
        &'a self,
        tenant: TenantId,
        language: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<SeededRoles, RoleTemplateStoreError>> + Send + 'a>>;
}

/// [`StartingRoles`] over a role-template store.
#[derive(Debug, Clone)]
pub struct RoleSeeder<P> {
    people: P,
}

impl<P> RoleSeeder<P> {
    /// Seeds into `people`.
    pub const fn new(people: P) -> Self {
        Self { people }
    }
}

impl<P> StartingRoles for RoleSeeder<P>
where
    P: RoleTemplateStore + Send + Sync,
{
    fn seed<'a>(
        &'a self,
        tenant: TenantId,
        language: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<SeededRoles, RoleTemplateStoreError>> + Send + 'a>>
    {
        Box::pin(seed_starting_roles(&self.people, tenant, language))
    }
}

#[cfg(test)]
mod tests {
    use super::{CONSOLE_PACKS, starting_role_grants, starting_role_id, starting_role_name};

    use std::collections::BTreeSet;

    use pos_core::permission::{Permission, Role};
    use pos_proto::ids::TenantId;
    use pos_proto::ulid::Ulid;

    /// Every built-in role has a name in each language the console ships, and none is blank, so the
    /// token fallback is never what a tenant sees.
    #[test]
    fn every_starting_role_is_named_in_every_console_language() {
        for (language, pack) in CONSOLE_PACKS.iter() {
            for role in Role::ALL {
                let key = format!(
                    "people.startingRole.{}",
                    role.as_token().to_ascii_lowercase()
                );
                let name = pack.get(&key).map(String::as_str).unwrap_or_default();
                assert!(!name.trim().is_empty(), "{language} names {key}");
            }
        }
        let names: BTreeSet<String> = Role::ALL
            .iter()
            .map(|role| starting_role_name(*role, Some("en")))
            .collect();
        assert_eq!(
            names.len(),
            Role::ALL.len(),
            "role names are unique within a tenant, so the six must differ: {names:?}"
        );
    }

    /// The name is in the language asked for where the console carries it, and English otherwise.
    #[test]
    fn a_name_is_in_the_tenants_language_and_english_otherwise() {
        let english = starting_role_name(Role::Cashier, None);
        let vietnamese = starting_role_name(Role::Cashier, Some("vi"));
        assert_ne!(
            english, vietnamese,
            "the Vietnamese pack names it differently"
        );
        assert_eq!(starting_role_name(Role::Cashier, Some("vi-VN")), vietnamese);
        assert_eq!(starting_role_name(Role::Cashier, Some(" VI ")), vietnamese);
        assert_eq!(starting_role_name(Role::Cashier, Some("en")), english);
        assert_eq!(starting_role_name(Role::Cashier, Some("ja")), english);
        assert_eq!(starting_role_name(Role::Cashier, Some("")), english);
    }

    /// Option B: a default role holds a permission directly; every other role holds a PIN-flagged
    /// one with approval and nothing else.
    #[test]
    fn a_starting_role_grants_its_defaults_directly_and_other_pin_flagged_acts_with_approval() {
        for role in Role::ALL {
            let (directly, with_approval) = starting_role_grants(*role);
            for permission in Permission::ALL {
                let meta = permission.meta();
                let id = meta.id.to_owned();
                let default = meta.default_roles.contains(role);
                assert_eq!(directly.contains(&id), default, "{role:?} {}", meta.id);
                assert_eq!(
                    with_approval.contains(&id),
                    !default && meta.pin_required,
                    "{role:?} {}",
                    meta.id
                );
            }
            let mut sorted = directly.clone();
            sorted.sort_unstable();
            assert_eq!(directly, sorted, "sorted");
        }
        // Spot checks against the catalogue, so a wrong reading of it would show here too.
        let (cook_directly, cook_with_approval) = starting_role_grants(Role::Cook);
        assert!(cook_directly.contains(&"sales.ticket.bump".to_owned()));
        assert!(!cook_directly.contains(&"billing.bill.void".to_owned()));
        assert!(cook_with_approval.contains(&"billing.bill.void".to_owned()));
        let (owner_directly, owner_with_approval) = starting_role_grants(Role::Owner);
        assert!(owner_directly.contains(&"billing.bill.void".to_owned()));
        assert!(
            owner_with_approval.is_empty(),
            "the owner is a default for every PIN-flagged permission"
        );
    }

    /// The six ids are distinct, stable for a tenant, and different for another tenant.
    #[test]
    fn a_starting_role_id_is_the_same_for_the_same_tenant_and_place() {
        let tenant = TenantId::new(Ulid::from_parts(
            1_790_000_000_000,
            0xABCD_EF01_2345_6789_ABCD,
        ));
        let other = TenantId::new(Ulid::from_parts(
            1_790_000_000_000,
            0x1234_5678_9ABC_DEF0_1234,
        ));
        let ids: BTreeSet<_> = (1_u8..=6)
            .map(|place| starting_role_id(tenant, place))
            .collect();
        assert_eq!(ids.len(), 6);
        assert_eq!(starting_role_id(tenant, 3), starting_role_id(tenant, 3));
        assert_ne!(starting_role_id(tenant, 3), starting_role_id(other, 3));
        assert_eq!(
            starting_role_id(tenant, 1).as_ulid().timestamp_ms(),
            tenant.as_ulid().timestamp_ms(),
            "the id keeps the tenant's creation time"
        );
    }
}
