// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The macro the event catalogue is declared with, kept beside it so the catalogue's file holds
//! the catalogue. It expands where it is invoked, in [`super`], and names that module's items.

/// Declares the whole catalogue: the [`EventType`] enumeration, one payload struct per
/// entry, the [`TypedPayload`] union, and the decode step.
///
/// Generating all four from one list is what stops them drifting: a payload cannot
/// declare one event type and be registered under another, and a new entry cannot be
/// added to the enumeration without a payload or vice versa.
macro_rules! event_catalogue {
    (
        $(
            $(#[$meta:meta])*
            $variant:ident => $token:literal, version = $version:literal {
                $(
                    $(#[$field_meta:meta])*
                    $field:ident : $field_type:ty
                ),* $(,)?
            }
        ),* $(,)?
    ) => {
        /// Every event type the framework knows.
        ///
        /// Closed on purpose: an unrecognised token from a newer sender is represented
        /// by `None` from [`EventTypeRef::known`](crate::envelope::EventTypeRef::known)
        /// rather than by a variant here, so forward compatibility lives at the wire
        /// and exhaustiveness survives in the code.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub enum EventType {
            $(
                $(#[$meta])*
                $variant,
            )*
        }

        impl EventType {
            /// Every type, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The `domain.resource.action` token.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $token,)* }
            }

            /// Parses a token. `None` for anything this build does not know.
            #[must_use]
            pub fn parse(token: &str) -> Option<Self> {
                match token { $($token => Some(Self::$variant),)* _ => None }
            }

            /// The payload contract version currently published for this type.
            #[must_use]
            pub const fn schema_version(self) -> u16 {
                match self { $(Self::$variant => $version,)* }
            }

            /// Every field name on this type's payload.
            #[must_use]
            pub const fn field_names(self) -> &'static [&'static str] {
                match self { $(Self::$variant => &[$(stringify!($field),)*],)* }
            }
        }

        impl core::fmt::Display for EventType {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        $(
            $(#[$meta])*
            #[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub struct $variant {
                $(
                    $(#[$field_meta])*
                    pub $field: $field_type,
                )*
            }

            impl EventPayload for $variant {
                const EVENT_TYPE: EventType = EventType::$variant;
                const SCHEMA_VERSION: u16 = $version;
                const FIELD_NAMES: &'static [&'static str] = &[$(stringify!($field),)*];
            }

            // The personal-data barrier, applied field by field at compile time. A
            // `String` here fails to compile with the remedy in the error text; see
            // `crate::pii`.
            const _: () = {
                $( $crate::pii::assert_no_pii::<$field_type>(); )*
            };
        )*

        /// A decoded payload.
        #[derive(Clone, PartialEq, Debug)]
        pub enum TypedPayload {
            $(
                $(#[$meta])*
                $variant($variant),
            )*
            /// From a sender speaking a newer vocabulary.
            ///
            /// Retained verbatim rather than dropped, so a node that does not
            /// understand an event can still store it, forward it, include it in a
            /// checksum, and hand it to a consumer that does.
            Unrecognised {
                /// The token as received.
                event_type: String,
                /// The payload bytes, untouched.
                data: RawPayload,
            },
        }

        impl TypedPayload {
            /// The token this payload belongs to, including for an unrecognised one.
            #[must_use]
            pub fn event_type(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $token,)*
                    Self::Unrecognised { event_type, .. } => event_type,
                }
            }
        }

        impl EventEnvelope<RawPayload> {
            /// Decodes the payload according to the envelope's event type.
            ///
            /// An unrecognised type is **not** an error: it yields
            /// [`TypedPayload::Unrecognised`] with the bytes intact.
            ///
            /// # Errors
            ///
            /// [`DecodeError`] only when the type *is* recognised and its payload does
            /// not match the declared shape.
            pub fn into_typed(self) -> Result<EventEnvelope<TypedPayload>, DecodeError> {
                let decoded = match self.event_type.known() {
                    $(
                        Some(EventType::$variant) => {
                            TypedPayload::$variant(self.data.decode()?)
                        }
                    )*
                    None => TypedPayload::Unrecognised {
                        event_type: self.event_type.as_str().to_owned(),
                        data: self.data.clone(),
                    },
                };
                Ok(self.map_data(|_| decoded))
            }
        }
    };
}
