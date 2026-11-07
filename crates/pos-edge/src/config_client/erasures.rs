// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `erasures` node on the edge
//! ([ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
//! decision 4): the subjects the cloud asked this store to mask.
//!
//! Applied beside the session swap, as the device deny-list is, because it awaits durable writes
//! that the pure `session_from_config` cannot make. Each id is masked and confirmed with
//! `data.subject.masked` by [`Edge::erase_subjects`]; an id already confirmed is skipped, so a node
//! applied on every pull changes nothing after the first.
//!
//! - **A malformed node, or one over its cap, is refused whole** and logged: half of an erasure
//!   list is not a smaller list, and the store cannot tell which half is missing. The cloud
//!   validates the node through the same [`Erasures::read`], so this only meets a node written
//!   around it.
//! - **A write that fails holds the config version back**, so the next pull fetches the same
//!   document and the erasure is attempted again rather than forgotten.

use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::erasures::Erasures;
use pos_proto::ids::{DeviceId, SubjectId};

use crate::app::{AppError, Edge};

use super::ConfigClient;

/// What the config loop masks subjects through: the live edge, behind a trait object so the loop
/// keeps its own store bound.
pub trait SubjectErasure: Send + Sync + core::fmt::Debug {
    /// Masks and confirms each subject not yet confirmed, and returns how many it confirmed.
    fn erase<'a>(
        &'a self,
        subject_ids: &'a [SubjectId],
    ) -> Pin<Box<dyn Future<Output = Result<usize, AppError>> + Send + 'a>>;
}

/// The live edge, writing as the box itself: `device_id` is the box's own system device id.
pub struct EdgeErasures<S> {
    edge: Arc<Edge<S>>,
    device_id: DeviceId,
}

impl<S> EdgeErasures<S> {
    /// The carrier `compose` hands the config loop.
    #[must_use]
    pub const fn new(edge: Arc<Edge<S>>, device_id: DeviceId) -> Self {
        Self { edge, device_id }
    }
}

/// A name and the device id: the edge itself is not worth printing.
impl<S> core::fmt::Debug for EdgeErasures<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EdgeErasures")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl<S> SubjectErasure for EdgeErasures<S>
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    fn erase<'a>(
        &'a self,
        subject_ids: &'a [SubjectId],
    ) -> Pin<Box<dyn Future<Output = Result<usize, AppError>> + Send + 'a>> {
        Box::pin(self.edge.erase_subjects(self.device_id, subject_ids))
    }
}

impl<T, S> ConfigClient<T, S> {
    /// Shares the edge the `erasures` node is carried into (ADR-0206 decision 4).
    ///
    /// Optional, as the origins and the deny-list are: the examples and most tests compose a loop
    /// without one, and a store the cloud never asked to erase anything is unaffected either way.
    #[must_use]
    pub fn with_erasures(mut self, erasures: Arc<dyn SubjectErasure>) -> Self {
        self.erasures = Some(erasures);
        self
    }
}

/// Carries out the `erasures` node of `document`, and says whether the document is done with:
/// `false` only when a write failed and the version must be pulled again.
pub(super) async fn apply(
    erasures: Option<&dyn SubjectErasure>,
    document: &serde_json::Value,
) -> bool {
    let Some(erasures) = erasures else {
        return true;
    };
    let node = match Erasures::read(document) {
        Ok(Some(node)) => node,
        Ok(None) => return true,
        Err(refusal) => {
            // Published configuration, never personal: a subject id is not personal data.
            tracing::warn!(%refusal, "a published erasures node was refused whole");
            return true;
        }
    };
    match erasures.erase(&node.subject_ids).await {
        Ok(confirmed) => {
            if confirmed > 0 {
                tracing::info!(
                    confirmed,
                    "masked the subjects the cloud asked this store to erase"
                );
            }
            true
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "could not mask every subject the cloud asked this store to erase; holding this \
                 config version back so the next pull tries again"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use core::future::Future;
    use core::pin::Pin;
    use std::sync::Mutex;

    use pos_proto::ids::SubjectId;
    use pos_proto::ulid::Ulid;
    use serde_json::json;

    use super::{SubjectErasure, apply};
    use crate::app::AppError;

    /// Records what it was asked to erase, and fails when told to.
    #[derive(Debug, Default)]
    struct Recorder {
        asked: Mutex<Vec<SubjectId>>,
        fail: bool,
    }

    impl SubjectErasure for Recorder {
        fn erase<'a>(
            &'a self,
            subject_ids: &'a [SubjectId],
        ) -> Pin<Box<dyn Future<Output = Result<usize, AppError>> + Send + 'a>> {
            Box::pin(async move {
                self.asked
                    .lock()
                    .expect("lock")
                    .extend_from_slice(subject_ids);
                if self.fail {
                    Err(AppError::Clock)
                } else {
                    Ok(subject_ids.len())
                }
            })
        }
    }

    fn subject() -> SubjectId {
        SubjectId::new(Ulid::from_u128(0x5AB))
    }

    #[tokio::test]
    async fn a_node_asks_the_edge_to_erase_its_ids() {
        let recorder = Recorder::default();
        let document = json!({ "erasures": { "subject_ids": [subject().to_string()] } });
        assert!(apply(Some(&recorder), &document).await);
        assert_eq!(*recorder.asked.lock().expect("lock"), vec![subject()]);
    }

    #[tokio::test]
    async fn a_malformed_node_is_refused_whole_and_nothing_is_erased() {
        let recorder = Recorder::default();
        let document = json!({ "erasures": { "subject_id": [subject().to_string()] } });
        assert!(
            apply(Some(&recorder), &document).await,
            "a refusal is not retried"
        );
        assert!(recorder.asked.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn a_failed_write_holds_the_version_back() {
        let recorder = Recorder {
            fail: true,
            ..Recorder::default()
        };
        let document = json!({ "erasures": { "subject_ids": [subject().to_string()] } });
        assert!(!apply(Some(&recorder), &document).await);
    }
}
