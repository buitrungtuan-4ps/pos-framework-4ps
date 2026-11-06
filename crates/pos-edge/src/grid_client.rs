// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The loop that keeps the tenant's translation grid on the store
//! ([ADR-0043](../../../docs/adr/0043-translation-grid.md)): the copy kept on disk first, so a store
//! that restarts offline still shows its labels, then the grid pulled whole beside the store's
//! configuration, at start and every [`GRID_POLL_INTERVAL`] after.
//!
//! A grid past its bounds (`pos_edge_app::labels`) is refused whole and the store keeps the one it
//! had. The grid holds labels HQ wrote, never personal data.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pos_edge_app::app::Edge;
use pos_edge_app::labels::LabelGrid;

use crate::cloud_http::GridHttpTransport;

/// How often the store pulls the grid. Labels change rarely, and a pull is the whole grid.
pub const GRID_POLL_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The name of the copy kept in the store's state directory.
pub const GRID_FILE_NAME: &str = "translations.json";

/// A tenant grid as the cloud and the kept copy write it.
type Grid = BTreeMap<String, BTreeMap<String, String>>;

/// Keeps the grid: the kept copy, then a pull at start and on every interval.
#[derive(Debug)]
pub struct GridClient<S> {
    transport: GridHttpTransport,
    edge: Arc<Edge<S>>,
    file: PathBuf,
}

impl<S: Send + Sync + 'static> GridClient<S> {
    /// A client pulling through `transport` into `edge`, keeping its copy at `file`.
    #[must_use]
    pub const fn new(transport: GridHttpTransport, edge: Arc<Edge<S>>, file: PathBuf) -> Self {
        Self {
            transport,
            edge,
            file,
        }
    }

    /// Reads the kept copy, then pulls until `shutdown`.
    pub async fn run<F>(self, interval: Duration, shutdown: F)
    where
        F: Future<Output = ()> + Send,
    {
        if let Some(kept) = read_kept(self.file.clone()).await {
            self.edge.apply_grid(kept);
        }
        tokio::pin!(shutdown);
        loop {
            self.pull_once().await;
            tokio::select! {
                () = &mut shutdown => break,
                () = tokio::time::sleep(interval) => {}
            }
        }
    }

    /// One pull: holds the grid and keeps a copy when it is new and within its bounds.
    async fn pull_once(&self) {
        let pulled = match self.transport.pull().await {
            Ok(grid) => grid,
            Err(error) => {
                tracing::warn!(%error, "the translation grid pull failed; keeping the grid held");
                return;
            }
        };
        if self.edge.label_grid().entries() == &pulled {
            return;
        }
        let Some(grid) = LabelGrid::bounded(pulled.clone()) else {
            tracing::warn!(
                keys = pulled.len(),
                "the translation grid is past its bounds; keeping the grid held"
            );
            return;
        };
        self.edge.apply_grid(grid);
        keep(self.file.clone(), pulled).await;
    }
}

/// The kept copy, or `None` when there is none or it does not read.
async fn read_kept(file: PathBuf) -> Option<LabelGrid> {
    let read = tokio::task::spawn_blocking(move || std::fs::read(&file)).await;
    let bytes = match read {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Ok(Err(error)) => {
            tracing::warn!(%error, "the kept translation grid could not be read");
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, "reading the kept translation grid did not finish");
            return None;
        }
    };
    serde_json::from_slice::<Grid>(&bytes)
        .ok()
        .and_then(LabelGrid::bounded)
}

/// Keeps `grid` at `file`, replacing the copy whole: written beside it, then renamed over it.
async fn keep(file: PathBuf, grid: Grid) {
    let written = tokio::task::spawn_blocking(move || {
        let bytes = serde_json::to_vec(&grid).map_err(std::io::Error::other)?;
        let partial = file.with_extension("json.partial");
        std::fs::write(&partial, bytes)?;
        std::fs::rename(&partial, &file)
    })
    .await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the translation grid could not be kept on disk"),
        Err(error) => tracing::warn!(%error, "keeping the translation grid did not finish"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{keep, read_kept};

    #[tokio::test]
    async fn a_kept_grid_reads_back_and_a_missing_one_is_none() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let file = dir.path().join("translations.json");
        assert!(read_kept(file.clone()).await.is_none());

        let mut grid = BTreeMap::new();
        grid.insert(
            "data.reservation".to_owned(),
            BTreeMap::from([("en".to_owned(), "Reservation".to_owned())]),
        );
        keep(file.clone(), grid.clone()).await;
        let kept = read_kept(file).await.expect("kept");
        assert_eq!(kept.entries(), &grid);
    }
}
