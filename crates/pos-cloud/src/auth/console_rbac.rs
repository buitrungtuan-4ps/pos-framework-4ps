// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Console role-based access control — the fixed permission catalogue and the role→permission
//! templates ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
//!
//! This mirrors the compile-forced registry pattern `pos-core` uses for store permissions
//! (`docs/pos-spec.md` §9), adapted to the console. A permission exists only as an entry in the
//! [`console_permissions!`] block, and that entry *requires* its `default_roles` — so adding a
//! console permission cannot compile until you have decided which roles receive it, and the default
//! is **deny**.
//!
//! One difference from the store side: console **roles are fixed**, not cloud-editable data. There
//! are exactly four ([`AdminRole`]), so the role→permission mapping is static and lives entirely
//! here; [`role_grants`] answers it directly from each permission's declared `default_roles`. Every
//! `/admin` route names the [`ConsolePermission`] it needs and the session guard checks it against
//! the acting admin's role (slice 2b); this module is only the vocabulary and the mapping.

use super::admin::AdminRole;

/// Everything a console permission declares.
#[derive(Debug, Clone, Copy)]
pub struct ConsolePermissionMeta {
    /// The stable id, `console.resource.action`.
    pub id: &'static str,
    /// The roles that receive it. Deny by default: a role not listed here does not get it.
    pub roles: &'static [AdminRole],
    /// A one-line description for the console and the docs.
    pub description: &'static str,
}

/// Declares the fixed console permission catalogue.
///
/// One entry per permission; the macro generates the [`ConsolePermission`] enum, its `ALL`, and
/// [`ConsolePermission::meta`] from the same block, so a new permission is one entry that updates all
/// three at once and *cannot omit its roles*.
macro_rules! console_permissions {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident {
                id: $id:literal,
                roles: [ $($role:ident),* $(,)? ],
                description: $description:literal $(,)?
            }
        ),+ $(,)?
    ) => {
        /// A permission from the fixed console catalogue.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum ConsolePermission {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl ConsolePermission {
            /// Every console permission, in declaration order.
            pub const ALL: &'static [Self] = &[ $(Self::$variant),+ ];

            /// The permission's declared metadata.
            #[must_use]
            pub const fn meta(self) -> ConsolePermissionMeta {
                match self {
                    $(
                        Self::$variant => ConsolePermissionMeta {
                            id: $id,
                            roles: &[ $(AdminRole::$role),* ],
                            description: $description,
                        },
                    )+
                }
            }
        }
    };
}

console_permissions! {
    /// Invite a new console admin, and view/revoke pending invitations and the admin roster.
    InviteAdmins {
        id: "console.admins.invite",
        roles: [Owner, Admin],
        description: "Invite console admins and view the roster and pending invitations",
    },
    /// Change an existing admin's role, and suspend or reactivate them.
    ManageAdmins {
        id: "console.admins.manage",
        roles: [Owner],
        description: "Change an admin's role, and suspend or reactivate an admin",
    },
    /// Create and edit tenants and brands (the top of the org tree).
    ManageOrgs {
        id: "console.orgs.manage",
        roles: [Owner, Admin],
        description: "Create and edit tenants and brands",
    },
    /// Create, rename, and archive stores, and reassign a store's brand.
    ManageStores {
        id: "console.stores.manage",
        roles: [Owner, Admin],
        description: "Create, rename, archive, and re-brand stores",
    },
    /// Issue and revoke per-tenant API keys.
    ManageApiKeys {
        id: "console.apikeys.manage",
        roles: [Owner, Admin],
        description: "Issue and revoke per-tenant API keys",
    },
    /// Approve, reject, and activate devices (printers/KDS onboarding).
    ManageDevices {
        id: "console.devices.manage",
        roles: [Owner, Admin, Ops],
        description: "Approve, reject, and activate devices",
    },
    /// Register, delete, and re-enable webhook endpoints.
    ManageWebhooks {
        id: "console.webhooks.manage",
        roles: [Owner, Admin, Ops],
        description: "Register, delete, and re-enable webhook endpoints",
    },
    /// Publish configuration, catalog, and layout to stores.
    PublishConfig {
        id: "console.config.publish",
        roles: [Owner, Admin, Ops],
        description: "Publish configuration, catalog, and layout to stores",
    },
    /// Author the catalog, menus, and layout (edit, not publish).
    ManageCatalog {
        id: "console.catalog.manage",
        roles: [Owner, Admin],
        description: "Author catalog items, menus, and layout",
    },
    /// Edit the translation grid.
    ManageTranslations {
        id: "console.translations.manage",
        roles: [Owner, Admin],
        description: "Edit the translation grid",
    },
    /// Manage a store's people: employees, role templates, per-store assignments, and PIN reset.
    ManagePeople {
        id: "console.people.manage",
        roles: [Owner, Admin],
        description: "Manage employees, role templates, store assignments, and reset staff PINs",
    },
    /// Author a store's floor and kitchen: areas, tables, stations, and item→station routing rules.
    ManageFloor {
        id: "console.floor.manage",
        roles: [Owner, Admin],
        description: "Author floor areas and tables, kitchen stations, and station routing rules",
    },
    /// Acknowledge and resolve operational alerts (ADR-0073). Ops gets it — alerts are a day-to-day
    /// operational concern — alongside owner and admin. Reading alerts needs only `Read`.
    ManageAlerts {
        id: "console.alerts.manage",
        roles: [Owner, Admin, Ops],
        description: "Acknowledge and resolve operational alerts",
    },
    /// Upload and delete media assets — item photos and brand logos (ADR-0075). Reading/serving a
    /// rendition needs only `Read`.
    ManageMedia {
        id: "console.media.manage",
        roles: [Owner, Admin],
        description: "Upload and delete media assets (item photos, brand logos)",
    },
    /// Author campaigns and promotions, and generate voucher batches (ADR-0077). Publishing them to a
    /// store reuses `PublishConfig`, exactly as every other node publish does. Owner/Admin, the manage
    /// norm — Ops publishes but does not author the promotion terms.
    ManageCampaigns {
        id: "console.campaigns.manage",
        roles: [Owner, Admin],
        description: "Author campaigns and promotions, and generate voucher batches",
    },
    /// Look up, export, and erase a data subject's personal data by id — the PDPD/GDPR subject-request
    /// tooling ([ADR-0076](../adr/0076-subject-request-tooling.md)). Owner-only: this is the console's
    /// most sensitive T1 surface (it can read and irreversibly erase a person's data), narrower than the
    /// Owner/Admin norm the other manage permissions use, and the tool is the Data Protection contact's
    /// deliberate instrument — never an autonomous or bulk path.
    ManageSubjects {
        id: "console.subjects.manage",
        roles: [Owner],
        description: "Look up, export, and erase a data subject's personal data (PDPD/GDPR requests)",
    },
    /// Read any tenant data — reports, registry, configuration, catalog, translations.
    Read {
        id: "console.data.read",
        roles: [Owner, Admin, Ops, Viewer],
        description: "Read tenant data: reports, registry, configuration, catalog, translations",
    },
}

/// Whether `role` is granted `permission`. The one authorisation question every `/admin` route asks;
/// deny by default, so a role not listed in the permission's `roles` is refused.
#[must_use]
pub fn role_grants(role: AdminRole, permission: ConsolePermission) -> bool {
    permission.meta().roles.contains(&role)
}

#[cfg(test)]
mod tests {
    use super::{ConsolePermission, role_grants};
    use crate::auth::admin::AdminRole;

    #[test]
    fn owner_has_every_permission() {
        for permission in ConsolePermission::ALL {
            assert!(
                role_grants(AdminRole::Owner, *permission),
                "owner should hold {}",
                permission.meta().id
            );
        }
    }

    #[test]
    fn admin_has_everything_except_managing_admins_and_subjects() {
        // Admin is denied managing other admins, and — the one manage permission narrower than the
        // Owner/Admin norm — the PDPD/GDPR subject-request tooling (owner-only, ADR-0076).
        assert!(!role_grants(
            AdminRole::Admin,
            ConsolePermission::ManageAdmins
        ));
        assert!(!role_grants(
            AdminRole::Admin,
            ConsolePermission::ManageSubjects
        ));
        for permission in ConsolePermission::ALL {
            if matches!(
                *permission,
                ConsolePermission::ManageAdmins | ConsolePermission::ManageSubjects
            ) {
                continue;
            }
            assert!(
                role_grants(AdminRole::Admin, *permission),
                "admin should hold {}",
                permission.meta().id
            );
        }
    }

    #[test]
    fn ops_gets_day_to_day_but_not_keys_orgs_or_authoring() {
        // Granted: the day-to-day surface the ADR names, plus read.
        for permission in [
            ConsolePermission::ManageDevices,
            ConsolePermission::ManageWebhooks,
            ConsolePermission::PublishConfig,
            ConsolePermission::ManageAlerts,
            ConsolePermission::Read,
        ] {
            assert!(
                role_grants(AdminRole::Ops, permission),
                "ops should hold {}",
                permission.meta().id
            );
        }
        // Denied: API keys, org/brand and store creation, catalog authoring, translations, and both
        // admin-management capabilities (inviting and managing).
        for permission in [
            ConsolePermission::ManageApiKeys,
            ConsolePermission::ManageOrgs,
            ConsolePermission::ManageStores,
            ConsolePermission::ManageCatalog,
            ConsolePermission::ManageTranslations,
            ConsolePermission::ManagePeople,
            ConsolePermission::InviteAdmins,
            ConsolePermission::ManageAdmins,
        ] {
            assert!(
                !role_grants(AdminRole::Ops, permission),
                "ops should not hold {}",
                permission.meta().id
            );
        }
    }

    #[test]
    fn viewer_is_read_only() {
        for permission in ConsolePermission::ALL {
            let expected = *permission == ConsolePermission::Read;
            assert_eq!(
                role_grants(AdminRole::Viewer, *permission),
                expected,
                "viewer should hold only console.data.read, checked {}",
                permission.meta().id
            );
        }
    }

    #[test]
    fn every_id_is_distinct_and_well_formed() {
        let mut ids: Vec<&str> = ConsolePermission::ALL.iter().map(|p| p.meta().id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two console permissions share an id");
        for permission in ConsolePermission::ALL {
            let id = permission.meta().id;
            let segments: Vec<&str> = id.split('.').collect();
            assert_eq!(segments.len(), 3, "{id} is not console.resource.action");
            assert_eq!(
                segments[0], "console",
                "{id} is not namespaced under console"
            );
            assert!(
                segments.iter().all(|segment| {
                    !segment.is_empty()
                        && segment.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                }),
                "{id} is not lower snake_case"
            );
        }
    }

    #[test]
    fn every_permission_is_granted_to_at_least_the_owner() {
        // Deny-by-default with a safety net: no permission is unreachable, and owner is the superset.
        for permission in ConsolePermission::ALL {
            assert!(
                !permission.meta().roles.is_empty(),
                "{} is granted to no role",
                permission.meta().id
            );
            assert!(
                permission.meta().roles.contains(&AdminRole::Owner),
                "{} is not held by the owner",
                permission.meta().id
            );
        }
    }
}
