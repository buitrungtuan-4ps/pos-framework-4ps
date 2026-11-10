// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The keys of a tenant's translation grid that carry an English text, which a write naming
//! translation keys is checked against: a receipt template's
//! ([ADR-0207](../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)
//! decision 3) and a package version's
//! ([ADR-0190](../../../docs/adr/0190-a-screen-is-drawn-from-the-package.md) decision 6).

use core::future::Future;
use core::pin::Pin;
use std::collections::BTreeSet;

use pos_cloud_catalog::translations::{FALLBACK_LOCALE, TranslationStore};
use pos_proto::ids::TenantId;

/// The keys of a tenant's grid that carry a non-empty `en` value, boxed so a router's state holds
/// one without a generic of its own.
pub(crate) type FallbackKeys<'a> =
    Pin<Box<dyn Future<Output = Result<BTreeSet<String>, String>> + Send + 'a>>;

/// Where the grid's keys are read.
pub(crate) trait GridKeys: Send + Sync {
    /// The keys of `tenant`'s grid that have an English text.
    fn fallback_keys(&self, tenant: TenantId) -> FallbackKeys<'_>;
}

/// The grid of a [`TranslationStore`].
pub(crate) struct StoreGrid<Tr>(pub(crate) Tr);

impl<Tr: TranslationStore + Send + Sync> GridKeys for StoreGrid<Tr> {
    fn fallback_keys(&self, tenant: TenantId) -> FallbackKeys<'_> {
        Box::pin(async move {
            let grid = self
                .0
                .load(tenant)
                .await
                .map_err(|error| error.to_string())?;
            Ok(grid
                .map(|versioned| {
                    versioned
                        .record
                        .as_map()
                        .iter()
                        .filter(|(_, texts)| {
                            texts
                                .get(FALLBACK_LOCALE)
                                .is_some_and(|text| !text.trim().is_empty())
                        })
                        .map(|(key, _)| key.clone())
                        .collect()
                })
                .unwrap_or_default())
        })
    }
}
