// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's [`CloudSync`](pos_ports::CloudSync) adapter, over HTTPS to the store's cloud
//! ([ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md)).
//!
//! The store has exactly one request/response channel to the cloud ([ADR-0053](../../../docs/adr/0053-cloud-sync-port.md)),
//! and this is its concrete adapter. Two calls ride it:
//!
//! - [`CloudSync::activate`](pos_ports::CloudSync::activate) — the first-boot activation exchange
//!   ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)): `POST /activate` with the
//!   operator's code, and the machine's long-lived credential comes back as a
//!   [`Secret`](pos_ports::Secret) to store in the [`KeyVault`](pos_ports::KeyVault).
//! - [`CloudSync::fetch_update`](pos_ports::CloudSync::fetch_update) — the over-the-air artifact
//!   ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)): `POST /internal/ota/artifact` with a
//!   release tag, and back come the artifact bytes **and** their detached signature, to verify with
//!   the [`Signer`](pos_ports::Signer) before staging. The bytes are the raw response body and the
//!   signature rides `X-Pos-Artifact-Signature` as lowercase hex
//!   ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)) — a JSON envelope would mean
//!   base64-encoding tens of megabytes to carry a few hundred bytes. A `2xx` with no signature
//!   header is a failed fetch, not an artifact.
//!
//! # A transport seam, and a pure core
//!
//! The socket lives behind [`HttpTransport`]; everything else — building the request body, and
//! mapping the cloud's HTTP status to the right [`PortError`](pos_ports::PortError) status — is pure.
//! That is what lets the shared `CloudSync` contract suite run in the fast pull-request gate against a
//! stub transport, while the real TLS path ([`TlsHttpTransport`]) is exercised in the gated
//! integration lane and the soak — the split [ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md)
//! already drew for the webhook sender.
//!
//! # The transport is not a trust boundary
//!
//! [`CloudSync::fetch_update`](pos_ports::CloudSync::fetch_update) returns the bytes as received,
//! paired with the signature that judges them. The caller verifies with the
//! [`Signer`](pos_ports::Signer) ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)) before
//! trusting them, so a spoofed or compromised cloud cannot make the edge install code — it can only
//! fail verification downstream.
//!
//! Pairing them is what makes that sentence enforceable rather than advisory: this adapter cannot
//! hand a caller bytes with no signature, because
//! [`SignedArtifact`](pos_ports::SignedArtifact) has nowhere to put "none". A cloud that answers
//! `2xx` without the header gets a retryable failure here, never a successful fetch.

#![forbid(unsafe_code)]

mod client;
mod wire;

pub use client::HttpCloudSync;
pub use wire::{HttpResponse, HttpTransport, TlsHttpTransport, TransportError};
