// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cloud configuration.
//!
//! Minimal for this slice: where to bind, and how to reach PostgreSQL. The four-level
//! Tenant→Brand→Store→Device config tree the cloud *serves* (`docs/roadmap.md` P7) is a later
//! slice; this is only the process's own boot configuration.

use std::net::SocketAddr;

use serde::Deserialize;

/// How the `pos_cloud` process boots.
#[derive(Debug, Clone, Deserialize)]
pub struct CloudConfig {
    /// The address the HTTP server binds.
    pub bind: SocketAddr,
    /// The PostgreSQL connection string, in libpq form
    /// (`host=… port=… user=… dbname=…`).
    pub database_url: String,
}

impl CloudConfig {
    /// Parses configuration from TOML text.
    ///
    /// # Errors
    ///
    /// [`toml::de::Error`] if the text is not a valid configuration document.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::CloudConfig;

    #[test]
    fn parses_a_minimal_config() {
        let config = CloudConfig::from_toml(
            "bind = \"127.0.0.1:8443\"\ndatabase_url = \"host=localhost user=pos dbname=poscloud\"\n",
        )
        .expect("valid config");
        assert_eq!(config.bind.port(), 8443);
        assert!(config.database_url.contains("dbname=poscloud"));
    }
}
