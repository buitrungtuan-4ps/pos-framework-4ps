// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Where the value in force of a setting that `config.toml` used to hold comes from, and the log
//! line that says so. Moved out of `pos_edge::config`, which re-exports both.

/// The `tracing` target this module logs under: the path it had in `pos_edge`, kept when it moved
/// into this crate so log filters and dashboards see the same target.
const LOG_TARGET: &str = "pos_edge::config";

/// Where the value in force of a setting `config.toml` used to hold comes from
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 6), in the order the edge chooses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSource {
    /// The store's configuration sets it, and it wins.
    Published,
    /// The configuration sets none, and `config.toml` still carries the deprecated key.
    LocalFile,
    /// Neither does: the default.
    Default,
}

impl ValueSource {
    /// The words the log gives it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Published => "the store's configuration",
            Self::LocalFile => "config.toml (deprecated)",
            Self::Default => "the default",
        }
    }
}

/// Says which value of `setting` is in force and where it comes from: once at start-up, and again
/// whenever it changes ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 6).
///
/// `file_key` is the deprecated `config.toml` key the setting replaces, and `file_sets` whether this
/// box's file still carries a value for it. While that value is the one in use the line is a warning
/// that says what to do; once the store's configuration sets the setting, the line says that it
/// overrides the file, so an operator who still has a file value can see which of the two the box
/// runs.
pub fn log_in_force(
    setting: &'static str,
    file_key: &'static str,
    value: u64,
    source: ValueSource,
    file_sets: bool,
) {
    match source {
        ValueSource::Published if file_sets => tracing::info!(
            target: LOG_TARGET,
            setting,
            value,
            from = source.as_str(),
            "{setting} is set by the store's configuration, which overrides config.toml's \
             deprecated {file_key} (ADR-0160 decision 6)"
        ),
        ValueSource::LocalFile => tracing::warn!(
            target: LOG_TARGET,
            setting,
            value,
            from = source.as_str(),
            "config.toml sets {file_key}, which is deprecated, and it is the value in use because \
             the store's configuration sets no {setting}: set {setting} in the console's shared \
             settings, and it replaces this one (ADR-0160 decision 6)"
        ),
        ValueSource::Published | ValueSource::Default => {
            tracing::info!(target: LOG_TARGET, setting, value, from = source.as_str(), "{setting} in force");
        }
    }
}
