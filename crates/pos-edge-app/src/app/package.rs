// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What became of the last configuration document's package on this edge
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 6):
//! the config-pull loop records a refusal, and the heartbeat and `GET /api/package` read it.

use std::sync::{Mutex, PoisonError};

/// Why the edge refuses a document's package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageRefusal {
    /// The node does not read as a `package` node.
    Malformed,
    /// The content breaks a rule the cloud checks at publish; the message says which.
    Invalid(String),
    /// The version needs a newer edge than this one, or this edge cannot tell its own release.
    NeedsNewerEdge {
        /// The release the version needs.
        min_edge_version: String,
    },
}

impl PackageRefusal {
    /// The reason as a token: `PACKAGE_MALFORMED`, `PACKAGE_INVALID` or `EDGE_TOO_OLD`.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::Malformed => "PACKAGE_MALFORMED",
            Self::Invalid(_) => "PACKAGE_INVALID",
            Self::NeedsNewerEdge { .. } => "EDGE_TOO_OLD",
        }
    }
}

/// A document the edge refused for its package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedPackage {
    /// The configuration version refused.
    pub config_version_id: String,
    /// The package version it carried, when that could be read.
    pub package_version_id: Option<String>,
    /// Why.
    pub refusal: PackageRefusal,
}

/// The last package refusal, shared between the config loop that records it and whoever reports it.
/// Cleared when a document is applied.
#[derive(Debug, Default)]
pub struct PackageStanding {
    refused: Mutex<Option<RefusedPackage>>,
}

impl PackageStanding {
    /// The last refusal, if the latest document was refused.
    #[must_use]
    pub fn refused(&self) -> Option<RefusedPackage> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records the latest document's outcome: its refusal, or `None` once one is applied.
    pub fn record(&self, refused: Option<RefusedPackage>) {
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = refused;
    }
}
