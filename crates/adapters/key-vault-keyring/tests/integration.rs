// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `key-vault-keyring` against a live OS credential store.
//!
//! The shared `KeyVault` contract suite ([`pos_contract_tests::key_vault_suite`]) — the same six
//! cases the in-memory backend passes in the fast gate — run here against the **real** keyring
//! ([`OsKeyring`]), so "the adapter round-trips a secret through the operating system's own store,
//! keeps secrets separate, and wipes every one" is a checked fact rather than an assumption.
//!
//! # Why this is gated — and why on real hardware
//!
//! A live credential store is not available in the ten-minute pull-request gate (ADR-0086), so the
//! whole file is behind the `integration` feature — the pull-request `test` job neither compiles nor
//! runs it. Run it where a real store exists:
//!
//! ```text
//! cargo test -p key-vault-keyring --features integration
//! ```
//!
//! This is a **real-hardware / real-OS gate, not a container CI job**. The `keyring` crate's
//! `linux-native` backend is the kernel keyring (keyutils), which needs a usable *session keyring* —
//! present on a booted store box or a developer workstation, but absent in a bare CI container, where
//! a `set` does not round-trip to a `get` (the credential store is genuinely unavailable). That is
//! exactly the "gated hardware/OS handoff" ADR-0086 flagged: the adapter's own mapping and semantics
//! are proven in the fast gate against the in-memory backend (see the crate's unit tests), and this
//! file proves the real OS integration on the box it will actually run on. Each case runs against a
//! `fresh()` vault; the suite stores under the fixed [`SecretName`] accounts and wipes them
//! (`wipes_every_secret_a_machine_holds`), so it leaves the host keyring as it found it.
#![cfg(feature = "integration")]

use key_vault_keyring::{KeyringVault, OsKeyring};
use pos_contract_tests::harness::{KeyVaultHarness, Setup};
use pos_fakes::executor::run_ready;

/// Drives the shared `KeyVault` contract suite against the adapter over the real OS keyring.
struct OsVaultHarness;

impl KeyVaultHarness for OsVaultHarness {
    type Vault = KeyringVault<OsKeyring>;

    async fn fresh(&self) -> Setup<Self::Vault> {
        // A fresh vault is the same OS store; the suite's own wipe leaves each account empty, and the
        // first thing every case does is establish the state it needs, so no explicit reset is owed.
        Ok(KeyringVault::new(OsKeyring::new()))
    }
}

pos_contract_tests::key_vault_suite!(OsVaultHarness, run_ready);
