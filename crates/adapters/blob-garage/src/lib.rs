// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud [`BlobStore`](pos_ports::blob_store::BlobStore) over Garage / S3 (P7).
//!
//! Deliberately thin. [ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md) first gave object
//! storage one job, Litestream's WAL shipping, and scheduled this port for removal once that was
//! in-house. `pos-cloud` now also keeps OTA release artifacts here
//! ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md), Correction 1), so the port stays.
//! It is still four methods over a handful of objects, so rather than carry an S3 SDK — dozens of
//! crates — the adapter hand-rolls `SigV4` request signing and `HTTP/1.1` over `tokio`
//! ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)).
//!
//! "Hand-rolled" does not mean "unverified". The one part that must be exactly right — the `SigV4`
//! signature — is checked in [`sign`]'s unit test against AWS's published vector, with no server;
//! and the end-to-end behaviour (put/get/delete, and segment-aware prefix listing) runs the shared
//! `BlobStore` contract suite against a real S3 server behind the `integration` feature — in CI
//! against Garage, the server the fork deploys, so the signature is proven against the thing
//! production answers with rather than against a second implementation.
//!
//! Path-style addressing (`http://endpoint/bucket/key`) and plain `http://` only — the cloud
//! reaches its own object store over the private network of its box; a public path terminates TLS
//! at the proxy (P8), not here.

#![forbid(unsafe_code)]

mod sign;
mod store;

pub use store::S3Blobs;
