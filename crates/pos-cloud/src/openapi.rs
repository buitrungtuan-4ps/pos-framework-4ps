// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The generated OpenAPI document for the public `/v1` surface, and its drift gate.
//!
//! [ADR-0019](../../../docs/adr/0019-openapi-generation.md): the document is generated from the
//! handlers ([`utoipa::path`] on each `/v1` function) and their response types (`utoipa::ToSchema`),
//! never hand-written. The test below renders [`ApiDoc`] to the committed `docs/openapi.json` and
//! fails CI when the code and that file disagree — exactly as `pos-proto`'s event-catalogue snapshot
//! does. Changing the API therefore means regenerating the document in the same pull request
//! (`POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi`).
//!
//! Internal routes (`/health`, `/internal/*`) are deliberately absent: the document is the external
//! contract, and those are not part of it.

use utoipa::OpenApi;

use crate::cloud::DailyRollup;

/// The public `/v1` API document.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Pizza 4P's POS Cloud API",
        version = "1.0.0",
        description = "The public read API for the Pizza 4P's cloud. Generated from the handlers; \
                       see ADR-0019."
    ),
    paths(crate::http::daily_rollups),
    components(schemas(DailyRollup)),
    tags((name = "rollups", description = "Per-store, per-trading-day activity rollups."))
)]
pub(crate) struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::ApiDoc;
    use utoipa::OpenApi as _;

    /// Where the generated document is committed, relative to the repository root.
    const SNAPSHOT_PATH: &str = "docs/openapi.json";

    /// Renders the document as pretty JSON, so the committed diff is reviewable line by line.
    fn render() -> String {
        ApiDoc::openapi().to_pretty_json().unwrap_or_default()
    }

    fn snapshot_file() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(SNAPSHOT_PATH)
    }

    #[test]
    fn the_committed_openapi_matches_the_code() {
        let rendered = render();
        let path = snapshot_file();

        // Opt-in regeneration, like the event snapshot: a check that silently fixes itself is not a
        // check.
        if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create the docs directory");
            }
            std::fs::write(&path, &rendered).expect("write the openapi document");
            return;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "{SNAPSHOT_PATH} is missing. Generate it with:\n\
                 \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi\n"
            )
        });
        assert_eq!(
            committed, rendered,
            "the /v1 API no longer matches {SNAPSHOT_PATH}. The OpenAPI document is generated from \
             the code (ADR-0019); regenerate it in this pull request with:\n\
             \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi\n"
        );
    }

    #[test]
    fn the_rendering_is_deterministic() {
        assert_eq!(render(), render());
    }
}
