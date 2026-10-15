// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a console read of a config document leaves out: staff credentials, staff identities a role
//! may not read, and prices.

use crate::auth::admin::AdminRole;
use crate::auth::console_rbac::{ConsolePermission, role_grants};

/// Removes the credentials a console read must never carry (production-readiness **S7**).
///
/// The `permissions` node the People publish writes contains each staff member's Argon2id PIN hash —
/// **T1**, the credential the edge verifies a sign-in against. It belongs on the `/sync` route a store
/// reads with its own scoped key, and nowhere else. Until this existed, `GET /admin/stores/{id}/config`
/// handed the whole effective document to anyone holding `console.data.read` — which every role down
/// to Viewer holds — so a read-only console account could lift the PIN hash of every member of staff
/// in the fleet, one store at a time.
///
/// Stripped unconditionally rather than gated on a permission: no console screen reads a PIN hash, no
/// console screen could sensibly do anything with one, and a value nobody needs is not worth a role
/// check that could later be widened by mistake. The field is *removed*, not blanked, so a reader
/// cannot mistake an empty string for "this member has no PIN set" — which is a real and different
/// state (they cannot sign in).
pub(in crate::http) fn without_staff_credentials(
    document: &serde_json::Value,
) -> serde_json::Value {
    let mut document = document.clone();
    if let Some(staff) = document
        .get_mut("permissions")
        .and_then(|node| node.get_mut("staff"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for member in staff {
            if let Some(member) = member.as_object_mut() {
                member.remove("pin_phc");
            }
        }
    }
    document
}

/// Removes each staff member's name and staff code from a console read for a caller without
/// [`ConsolePermission::ReadPeople`]
/// ([ADR-0158](../../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 9).
///
/// The `permissions` node names every member of staff a store authorises: the till shows the name,
/// and the person signs in with the code. Both are **T1** personal data (ADR-0070), and the console's
/// own staff records — the roster and the assignments — are behind `ReadPeople`. Without this, the
/// config screen would be the way round that gate: a Viewer refused the roster could read every
/// name in the fleet here, one store at a time.
///
/// **Gated, unlike the credential.** An Owner or Admin manages these people and is shown the same
/// names on the People screen, so the node tells them nothing new. A PIN hash is different in kind,
/// which is why [`without_staff_credentials`] removes it from every read.
///
/// Only the two fields go. Each member's id, permission set and discount ceiling stay, and so does
/// the rest of the document, so the config screen still shows what each person at the store may
/// do. The fields are *removed*, not blanked, for the reason S7 gives: a blank name would read as a
/// person who has none, not as a field this role is not shown.
pub(in crate::http) fn without_staff_identities(document: &serde_json::Value) -> serde_json::Value {
    let mut document = document.clone();
    if let Some(staff) = document
        .get_mut("permissions")
        .and_then(|node| node.get_mut("staff"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for member in staff {
            if let Some(member) = member.as_object_mut() {
                member.remove("name");
                member.remove("code");
            }
        }
    }
    document
}

/// A config document with its staff as `role` may read them: never a PIN hash, whoever asks
/// ([`without_staff_credentials`], **S7**), and a name or staff code only for a role holding
/// [`ConsolePermission::ReadPeople`] ([`without_staff_identities`],
/// [ADR-0158](../../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
///
/// The two rules always travel together, so every console surface that hands back configuration
/// applies them through this one function: the current and past config reads, and the merge patch
/// both publish previews answer with. The patch needs nothing of its own. Every value in an
/// RFC 7386 patch is either the candidate's own or a `null` that deletes a key, and a changed array
/// is carried whole rather than patched, so wherever a patch reaches `permissions.staff` it holds
/// complete members, exactly as the document does.
pub(in crate::http) fn staff_redacted_for(
    document: &serde_json::Value,
    role: AdminRole,
) -> serde_json::Value {
    let document = without_staff_credentials(document);
    if role_grants(role, ConsolePermission::ReadPeople) {
        document
    } else {
        without_staff_identities(&document)
    }
}

/// The config nodes whose published shape carries money, and which a console caller therefore reads
/// only with [`ConsolePermission::ReadRevenue`] (production-readiness **S8**).
///
/// A node earns a place here by carrying a **price a competitor would want** — `Money` in the wire
/// type the publish path writes — not merely by being commercially interesting. Today that is
/// three:
///
/// - **`menu`** — the compiled per-channel price book. This is the node S8 was raised for: closing
///   `GET /admin/catalog/menus/{id}/placements` under **S5** left the same price book readable one
///   route over, through the config screen.
/// - **`campaigns`** — combo prices, discount amounts and minimum-bill thresholds
///   ([ADR-0077](../../../docs/adr/0077-campaigns.md)). Found while fixing S8, and included because a
///   redaction that closes one price surface and leaves its sibling open is not a fix; a promotion's
///   economics are the same **T2** class as a menu price.
/// - **`fees`** — each fee's rate or amount, and which items and channels it covers
///   ([ADR-0159](../../../../../docs/adr/0159-a-fee-is-configuration.md)), the same class again; the fee
///   routes read them only with `ReadRevenue` for the same reason.
///
/// `layout` is deliberately absent — buttons and their order carry no money — and so are `tax`
/// (published rates, which a receipt shows the guest anyway) and `permissions` (whose credential is
/// removed unconditionally by [`without_staff_credentials`], and whose names and codes go by role in
/// [`without_staff_identities`]).
pub(in crate::http) const PRICED_CONFIG_NODES: &[&str] = &["menu", "campaigns", "fees"];

/// Removes the priced nodes from a console read for a caller without `ReadRevenue`
/// (production-readiness **S8**).
///
/// **Why redact rather than refuse the whole read.** The alternative was to gate
/// `GET /admin/stores/{id}/config` on `ReadRevenue` outright, which is one line and costs Ops the
/// config screen — the screen they use to see a store's capabilities, floor, stations and printers.
/// The point of carving `ReadRevenue` out of `Read` was that prices are narrower than data, so the
/// narrow thing is what should be withheld.
///
/// The node is **removed, not blanked**, for the reason S7 gives: an empty object would read as
/// "this store has no menu published", which is a real and different state. The console tells the
/// operator that something is hidden by their role rather than letting them infer it from an absence.
pub(in crate::http) fn without_prices(document: &serde_json::Value) -> serde_json::Value {
    let mut document = document.clone();
    if let Some(nodes) = document.as_object_mut() {
        for node in PRICED_CONFIG_NODES {
            nodes.remove(*node);
        }
    }
    document
}
