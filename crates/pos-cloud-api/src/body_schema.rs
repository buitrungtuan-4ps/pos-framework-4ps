// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Schemas for bodies that are not JSON, as the console API document describes them
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 3).

use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, SchemaType, Type};

/// Raw bytes: an uploaded image, a rendition, an archive. A schema only; nothing constructs it.
///
/// `body = Vec<u8>` would describe an array of integers, which is not what goes on the wire, so a
/// route whose body is bytes names this instead, with its own `content_type`.
#[derive(Debug)]
pub struct Binary;

impl utoipa::PartialSchema for Binary {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
            .into()
    }
}

impl utoipa::ToSchema for Binary {}

/// Free-form JSON: a stored configuration document, or a node whose shape no wire type in reach
/// describes (ADR-0180 D-T3). A schema only; nothing constructs it.
///
/// A route that answers one says in its description what the JSON is and where its shape is
/// defined, so the console knows what it is reading even though the schema cannot say.
#[derive(Debug)]
pub struct FreeFormJson;

impl utoipa::PartialSchema for FreeFormJson {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(SchemaType::AnyValue)
            .description(Some(
                "Free-form JSON. The route's description names what it is and where its shape is \
                 defined",
            ))
            .into()
    }
}

impl utoipa::ToSchema for FreeFormJson {}

#[cfg(test)]
mod tests {
    use utoipa::PartialSchema as _;

    use super::Binary;

    #[test]
    fn free_form_json_constrains_nothing() {
        let schema = serde_json::to_value(super::FreeFormJson::schema()).expect("serialise");
        let keys: Vec<&str> = schema
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["description"],
            "no type, no properties: any JSON value"
        );
    }

    #[test]
    fn binary_is_a_string_of_format_binary() {
        let schema = serde_json::to_value(Binary::schema()).expect("serialise");
        assert_eq!(
            schema,
            serde_json::json!({ "type": "string", "format": "binary" })
        );
    }
}
