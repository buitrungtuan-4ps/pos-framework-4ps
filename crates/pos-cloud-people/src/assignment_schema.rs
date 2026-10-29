// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The OpenAPI schema of [`AssignmentScope`], written by hand because its `Serialize` is
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 3).
//!
//! On the wire a scope is `scope_kind` with the id that names it beside it, so the schema is a
//! `oneOf` of three objects, one per kind. [`tests`] holds each branch to what the variant
//! serialises to.

use pos_proto::wire_enum::WireEnum as _;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, OneOfBuilder, Schema, Type};

use crate::people::{AssignmentScope, AssignmentScopeKind};

/// One kind's object: its `scope_kind` token, and the id that names it where there is one.
fn branch(kind: AssignmentScopeKind, id: Option<(&str, &str)>) -> ObjectBuilder {
    let token = ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some([kind.as_wire()]));
    let object = ObjectBuilder::new()
        .schema_type(Type::Object)
        .property("scope_kind", token)
        .required("scope_kind");
    match id {
        Some((field, description)) => object
            .property(
                field,
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .description(Some(description)),
            )
            .required(field),
        None => object,
    }
}

impl utoipa::PartialSchema for AssignmentScope {
    fn schema() -> RefOr<Schema> {
        OneOfBuilder::new()
            .item(branch(
                AssignmentScopeKind::Store,
                Some(("store_id", "The one store it reaches, a ULID")),
            ))
            .item(branch(
                AssignmentScopeKind::StoreGroup,
                Some((
                    "store_group_id",
                    "The group whose stores it reaches when read, a ULID",
                )),
            ))
            .item(branch(AssignmentScopeKind::Tenant, None))
            .description(Some(
                "Where an assignment grants its role: `scope_kind`, with `store_id` for one store, \
                 `store_group_id` for a group, and neither for every store of the tenant",
            ))
            .into()
    }
}

impl utoipa::ToSchema for AssignmentScope {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;
    use utoipa::PartialSchema as _;

    use crate::people::AssignmentScope;
    use crate::store_groups::StoreGroupId;

    /// The branch whose `scope_kind` token is `token`: its property names and its required ones.
    fn branch_for(token: &str) -> (BTreeSet<String>, BTreeSet<String>) {
        let schema = serde_json::to_value(AssignmentScope::schema()).expect("serialise");
        let branch = schema["oneOf"]
            .as_array()
            .expect("a oneOf")
            .iter()
            .find(|branch| branch["properties"]["scope_kind"]["enum"][0] == token)
            .unwrap_or_else(|| panic!("no branch for {token}"));
        let names = |value: &serde_json::Value| -> BTreeSet<String> {
            value
                .as_object()
                .map(|object| object.keys().cloned().collect())
                .unwrap_or_default()
        };
        let required = branch["required"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(|n| n.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        (names(&branch["properties"]), required)
    }

    #[test]
    fn each_scope_serialises_as_its_branch_describes() {
        let scopes = [
            AssignmentScope::Store(StoreId::new(Ulid::from_u128(1))),
            AssignmentScope::StoreGroup(StoreGroupId::new(Ulid::from_u128(1))),
            AssignmentScope::Tenant,
        ];
        for scope in scopes {
            let sent = serde_json::to_value(scope).expect("serialise");
            let token = sent["scope_kind"].as_str().expect("a scope_kind token");
            let keys: BTreeSet<String> = sent
                .as_object()
                .expect("an object")
                .keys()
                .cloned()
                .collect();
            let (properties, required) = branch_for(token);
            assert_eq!(
                keys, properties,
                "{token}: the fields sent are the branch's properties"
            );
            assert_eq!(
                keys, required,
                "{token}: every field the branch names is always sent"
            );
        }
    }
}
