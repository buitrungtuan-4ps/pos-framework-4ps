// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud [`BlobStore`](pos_ports::blob_store::BlobStore) over Garage / S3 (P7).
//!
//! Deliberately thin, and deliberately temporary: [ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md)
//! records that object storage exists in this system **only** for Litestream, and that this port is
//! **deleted outright** once WAL shipping is in-house. So rather than carry an S3 SDK — dozens of
//! crates for four methods over small objects that are scheduled for removal — the adapter
//! hand-rolls `SigV4` request signing and `HTTP/1.1` over `tokio`
//! ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)).
//!
//! "Hand-rolled" does not mean "unverified". The one part that must be exactly right — the `SigV4`
//! signature — is checked in [`sign`]'s unit test against AWS's published vector, with no server;
//! and the end-to-end behaviour (put/get/delete, and segment-aware prefix listing) runs the shared
//! `BlobStore` contract suite against a real S3 server (MinIO/Garage) behind the `integration`
//! feature.
//!
//! Path-style addressing (`http://endpoint/bucket/key`) and plain `http://` only — the cloud
//! reaches its own object store over the private network of its box; a public path terminates TLS
//! at the proxy (P8), not here.

#![forbid(unsafe_code)]

mod sign;
mod store;

pub use store::S3Blobs;
