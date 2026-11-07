// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `erasures` config node: the subjects a store must mask now
//! ([ADR-0206](../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
//! decision 4).
//!
//! A personal custom value stays on the store that collected it, so the cloud cannot erase it
//! itself. When the owner erases a subject the cloud does not hold but a store's events name, the
//! cloud adds the subject's id to that store's node. A subject id is not personal data, so the
//! instruction travels the way configuration does. The store masks the subject when it applies the
//! node and confirms with `data.subject.masked`; the cloud then takes the id off the node.
//!
//! The cloud validates the node before it publishes it and the edge reads it before it acts, both
//! through [`Erasures::read`], so the two cannot disagree about what a store is asked to do.

use serde::{Deserialize, Serialize};

use crate::ids::SubjectId;

/// The most subject ids one store's node may carry. The console refuses an erasure that would
/// overflow it (ADR-0206 decision 4).
pub const MAX_ERASURES: usize = 1_000;

/// The `erasures` node.
///
/// `subject_ids` is required, as `revoked_devices`' `device_ids` is: a node whose key was misspelled
/// must not read as a node naming nobody. No `deny_unknown_fields`, as for every published node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Erasures {
    /// The subjects to mask, in the order they were asked for.
    pub subject_ids: Vec<SubjectId>,
}

/// Why a published `erasures` node was refused whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErasuresError {
    /// The node is not an object with a `subject_ids` list of subject ids.
    Malformed,
    /// The node lists more than [`MAX_ERASURES`] ids.
    TooMany {
        /// How many it lists.
        count: usize,
    },
}

impl core::fmt::Display for ErasuresError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Malformed => formatter.write_str(
                "the `erasures` node is not an object with a `subject_ids` list of subject ids",
            ),
            Self::TooMany { count } => write!(
                formatter,
                "the `erasures` node lists {count} subject ids, over its cap of {MAX_ERASURES}"
            ),
        }
    }
}

impl Erasures {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "erasures";

    /// The node `document` carries: `Ok(None)` when it has none, and an error when it has one that
    /// cannot be read or is over its cap. A node is refused whole, never applied in part.
    ///
    /// Read through `to_string` and `from_str`, the path every node the edge reads takes.
    ///
    /// # Errors
    ///
    /// [`ErasuresError`] for a node that is malformed or lists more than [`MAX_ERASURES`] ids.
    pub fn read(document: &serde_json::Value) -> Result<Option<Self>, ErasuresError> {
        let Some(node) = document.get(Self::NODE) else {
            return Ok(None);
        };
        let parsed = serde_json::to_string(node)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .ok_or(ErasuresError::Malformed)?;
        if parsed.subject_ids.len() > MAX_ERASURES {
            return Err(ErasuresError::TooMany {
                count: parsed.subject_ids.len(),
            });
        }
        Ok(Some(parsed))
    }
}

#[cfg(test)]
mod tests {
    use super::{Erasures, ErasuresError, MAX_ERASURES};

    use crate::ids::SubjectId;
    use crate::ulid::Ulid;

    use serde_json::json;

    fn subject(n: u128) -> SubjectId {
        SubjectId::new(Ulid::from_u128(n))
    }

    #[test]
    fn a_document_without_the_node_has_none() {
        assert_eq!(Erasures::read(&json!({ "menu": {} })), Ok(None));
    }

    #[test]
    fn a_node_reads_its_subject_ids() {
        let document = json!({ "erasures": { "subject_ids": [subject(1).to_string()] } });
        assert_eq!(
            Erasures::read(&document),
            Ok(Some(Erasures {
                subject_ids: vec![subject(1)]
            }))
        );
    }

    #[test]
    fn a_misspelled_or_malformed_node_is_refused_whole() {
        for node in [
            json!({ "subject_id": [subject(1).to_string()] }),
            json!({ "subject_ids": ["not a ulid"] }),
            json!([subject(1).to_string()]),
        ] {
            assert_eq!(
                Erasures::read(&json!({ "erasures": node })),
                Err(ErasuresError::Malformed)
            );
        }
    }

    #[test]
    fn a_node_over_its_cap_is_refused_whole() {
        let ids: Vec<String> = (0..=MAX_ERASURES)
            .map(|n| subject(n as u128 + 1).to_string())
            .collect();
        assert_eq!(
            Erasures::read(&json!({ "erasures": { "subject_ids": ids } })),
            Err(ErasuresError::TooMany {
                count: MAX_ERASURES + 1
            })
        );
    }
}
