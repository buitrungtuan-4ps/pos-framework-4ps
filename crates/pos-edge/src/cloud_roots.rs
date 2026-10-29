// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The extra roots this box trusts its cloud's certificate from: `cloud_ca_file` in `config.toml`.
//!
//! Every client that dials the store's cloud — activation, config-pull, heartbeat, the order relay,
//! event publish, OTA, archives and the claim — trusts the bundled Mozilla roots. A cloud deployed
//! with a certificate from a company's own CA (`TLS_MODE=byo-cert`,
//! [ADR-0090](../../../docs/adr/0090-tls-postures.md)) presents one none of them signed, and every
//! handshake fails. `cloud_ca_file` names a PEM file of that CA's root certificates, which are trusted
//! **as well as** the public roots, never instead of them.
//!
//! It is bootstrap configuration rather than published configuration for the same reason
//! `cloud_url` is: the box needs it before it can reach the cloud that publishes everything else.

use std::path::{Path, PathBuf};

use tokio_rustls::rustls::RootCertStore;
use tokio_rustls::rustls::pki_types::CertificateDer;
use tokio_rustls::rustls::pki_types::pem::PemObject as _;

/// The roots a cloud client trusts: the bundled Mozilla set plus `extra`.
#[must_use]
pub fn trust_store(extra: &[CertificateDer<'static>]) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots.add_parsable_certificates(extra.iter().cloned());
    roots
}

/// The certificates in `pem`, every one of which must be usable as a root.
///
/// # Errors
///
/// A sentence for the log when a PEM section is damaged, when there is no certificate at all, or
/// when one cannot be a trust anchor. A file that half-loads is refused whole: the operator would
/// otherwise be told the CA is trusted while the one that mattered was skipped.
pub fn parse(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String> {
    let certificates = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("it is not readable PEM: {error}"))?;
    if certificates.is_empty() {
        return Err("it holds no CERTIFICATE section".to_owned());
    }
    let (_added, ignored) =
        RootCertStore::empty().add_parsable_certificates(certificates.iter().cloned());
    if ignored > 0 {
        return Err(format!(
            "{ignored} of its {} certificates cannot be used as a root",
            certificates.len()
        ));
    }
    Ok(certificates)
}

/// Reads and parses the file at `path`. Blocking: for the claim, which runs before any runtime, and
/// for [`load`], which moves it off the async workers.
///
/// # Errors
///
/// As [`parse`], and when the file cannot be read.
pub fn read(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let pem = std::fs::read(path).map_err(|error| format!("it cannot be read: {error}"))?;
    parse(&pem)
}

/// The extra roots `cloud_ca_file` names, or none.
///
/// A file that cannot be used is logged as an error and the box carries on with the public roots
/// alone: the store trades either way, and a cloud on a private CA then fails its handshake with a
/// reason `/setup` shows, rather than the box refusing to start at all.
pub async fn load(path: Option<PathBuf>) -> Vec<CertificateDer<'static>> {
    let Some(path) = path else {
        return Vec::new();
    };
    let shown = path.display().to_string();
    match tokio::task::spawn_blocking(move || read(&path)).await {
        Ok(Ok(roots)) => {
            tracing::info!(cloud_ca_file = %shown, roots = roots.len(), "trusting the cloud's private CA as well as the public roots");
            roots
        }
        Ok(Err(reason)) => {
            tracing::error!(cloud_ca_file = %shown, %reason, "cloud_ca_file is set but cannot be used, so only the public roots are trusted and a cloud on a private CA will fail its TLS handshake");
            Vec::new()
        }
        Err(error) => {
            tracing::error!(cloud_ca_file = %shown, %error, "reading cloud_ca_file did not finish, so only the public roots are trusted");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::sync::Arc;

    use tokio_rustls::rustls::client::WebPkiServerVerifier;
    use tokio_rustls::rustls::client::danger::ServerCertVerifier as _;
    use tokio_rustls::rustls::pki_types::{ServerName, UnixTime};

    use tokio_rustls::rustls::pki_types::pem::PemObject as _;

    use super::{CertificateDer, parse, trust_store};

    /// A throwaway private CA (`CN=pos-edge test private CA`), as a company's own would be. Only
    /// certificates are here; the keys that signed them were never kept.
    const PRIVATE_CA: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
MIIBrTCCAVOgAwIBAgIUapefNlVHT30UWZ2J9nrMTUoVyA4wCgYIKoZIzj0EAwIw\n\
IzEhMB8GA1UEAwwYcG9zLWVkZ2UgdGVzdCBwcml2YXRlIENBMCAXDTI2MTAyOTA2\n\
MDIxNVoYDzIxMjYxMDA1MDYwMjE1WjAjMSEwHwYDVQQDDBhwb3MtZWRnZSB0ZXN0\n\
IHByaXZhdGUgQ0EwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASgR5NpvAd8RrJx\n\
ivECJih28XTC8HXc/zBtS3OylBlFORJnISJqiIDiCgyUUwEOcalevpVJPsLXdGYa\n\
+jWboGr2o2MwYTAdBgNVHQ4EFgQUxV6dwVpzOpMrinZgpQwZ1KREi3QwHwYDVR0j\n\
BBgwFoAUxV6dwVpzOpMrinZgpQwZ1KREi3QwDwYDVR0TAQH/BAUwAwEB/zAOBgNV\n\
HQ8BAf8EBAMCAgQwCgYIKoZIzj0EAwIDSAAwRQIgPRIC+4EPwd4Q04OG6AIhXiX2\n\
fL5t4ErY+ruiBvRs3dECIQDQlvT8sgJkZcVY8fAQ/TxOC8FZ59hQUH3DgoRKWU+O\n\
tQ==\n\
-----END CERTIFICATE-----\n";

    /// A server certificate for `cloud.pilot.internal`, issued by [`PRIVATE_CA`].
    const CLOUD_LEAF: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
MIIB3jCCAYSgAwIBAgIUGQG+L2J3ZJJbqAJQbIWTnilv0d0wCgYIKoZIzj0EAwIw\n\
IzEhMB8GA1UEAwwYcG9zLWVkZ2UgdGVzdCBwcml2YXRlIENBMCAXDTI2MTAyOTA2\n\
MDI0MloYDzIxMjUwNTIzMDYwMjQyWjAfMR0wGwYDVQQDDBRjbG91ZC5waWxvdC5p\n\
bnRlcm5hbDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABLzypJFiHswzpkSq8qe5\n\
Gb0xpOTjbEa56mepRmFFaBuSmWOeuzYXgWQedHkHKgojYqSZVhDwTISdj1M5EDfT\n\
4eSjgZcwgZQwHwYDVR0RBBgwFoIUY2xvdWQucGlsb3QuaW50ZXJuYWwwEwYDVR0l\n\
BAwwCgYIKwYBBQUHAwEwDAYDVR0TAQH/BAIwADAOBgNVHQ8BAf8EBAMCB4AwHQYD\n\
VR0OBBYEFHK1rDDt63jwUwtQb8VAJpXPnS6OMB8GA1UdIwQYMBaAFMVencFaczqT\n\
K4p2YKUMGdSkRIt0MAoGCCqGSM49BAMCA0gAMEUCIGS9+1Dxu8ClFgAEt7gh97H5\n\
1/WDbRKbWHPX/XyCCNiBAiEAyCHCAMwNcEEVn7zIXA0V6yg7QFFCd86JAv79McKd\n\
DHA=\n\
-----END CERTIFICATE-----\n";

    /// Whether a client trusting `trust_store(extra)` accepts the cloud's certificate, in 2027.
    fn accepts(extra: &[CertificateDer<'static>]) -> bool {
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let Ok(verifier) =
            WebPkiServerVerifier::builder_with_provider(Arc::new(trust_store(extra)), provider)
                .build()
        else {
            return false;
        };
        let Ok(leaf) = CertificateDer::from_pem_slice(CLOUD_LEAF) else {
            return false;
        };
        let Ok(name) = ServerName::try_from("cloud.pilot.internal") else {
            return false;
        };
        let now = UnixTime::since_unix_epoch(Duration::from_secs(1_800_000_000));
        verifier
            .verify_server_cert(&leaf, &[], &name, &[], now)
            .is_ok()
    }

    #[test]
    fn a_cloud_on_a_private_ca_is_trusted_only_once_its_root_is_named() {
        assert!(!accepts(&[]), "the public roots alone must refuse it");
        let roots = parse(PRIVATE_CA).unwrap_or_default();
        assert_eq!(roots.len(), 1);
        assert!(accepts(&roots), "the named CA must make it trusted");
    }

    #[test]
    fn the_public_roots_are_kept_beside_the_extra_ones() {
        let roots = parse(PRIVATE_CA).unwrap_or_default();
        assert_eq!(
            trust_store(&roots).len(),
            webpki_roots::TLS_SERVER_ROOTS.len() + 1
        );
    }

    #[test]
    fn a_file_with_no_usable_certificate_is_refused() {
        assert!(parse(b"").is_err(), "empty");
        assert!(parse(b"not a certificate").is_err(), "no PEM at all");
        assert!(
            parse(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n").is_err(),
            "a section that is not a certificate"
        );
    }
}
