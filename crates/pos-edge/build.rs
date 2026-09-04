// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Two compile-time chores: the embedded-UI directory has to exist, and the target triple has to
//! be stamped into the binary.
//!
//! `ui/dist` is the SolidJS build output (P6) and is gitignored, so a fresh checkout has no
//! `ui/dist/index.html` for `rust-embed` to embed and the crate would not compile. Until P6's
//! toolchain populates that directory, this writes a placeholder there — enough for the edge to
//! serve a page and for the tests to pass — and it **never overwrites a real build**: if
//! `index.html` already exists, it does nothing. `rust-embed` emits its own rerun-if-changed for the
//! folder, so no such instruction is needed here.
//!
//! # The target triple
//!
//! `POS_EDGE_TARGET` is what the OTA artifact fetch sends as `arch`
//! ([ADR-0088](../../docs/adr/0088-ota-artifact-hosting.md) Correction 2, roadmap v3 **R5**), and a
//! build script is the only place it can come from: Cargo passes `TARGET` here and nowhere else.
//!
//! `version.rs` avoided a build script for the release version and that reasoning does not transfer —
//! the workflow knows the tag, but nothing except Cargo knows the target. The tempting alternative is
//! to compose the triple from `std::env::consts::{ARCH, OS}` at runtime. That is right for exactly
//! the two targets R1 builds and silently wrong the moment a fork cross-compiles to musl:
//! `x86_64-unknown-linux-musl` would report itself as `x86_64-unknown-linux-gnu`, the cloud would
//! hand it a glibc binary, and the failure would surface after the install as a self-test failure
//! with nothing pointing at the cause.

use std::path::Path;

/// The placeholder served until P6 builds the real interface. Kept in step with the tests, which
/// assert the served page names the product.
const PLACEHOLDER: &str = "<!doctype html>\n\
<html lang=\"en\">\n\
  <head>\n\
    <meta charset=\"utf-8\" />\n\
    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
    <title>Pizza 4P's POS — edge</title>\n\
  </head>\n\
  <body>\n\
    <main>\n\
      <h1>Pizza 4P's POS</h1>\n\
      <p>The store is running. The operator interface arrives in P6.</p>\n\
      <p>Health: <a href=\"/healthz\">/healthz</a></p>\n\
    </main>\n\
  </body>\n\
</html>\n";

// A build script's stdout *is* the protocol Cargo reads: `cargo::rustc-env=` and
// `cargo::rerun-if-changed=` are directives, not log lines, and `tracing` cannot reach Cargo. The
// workspace denies `println!` because in the shipped binary a log has to travel to the cloud
// (ADR-0031); that reasoning does not apply to code which never runs on a store.
#[expect(
    clippy::disallowed_macros,
    reason = "a build script talks to Cargo over stdout; there is no other channel, and this code \
              never runs on a store"
)]
fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let dist = Path::new(&manifest).join("../../ui/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::create_dir_all(&dist).expect("create ui/dist");
        std::fs::write(&index, PLACEHOLDER).expect("write placeholder index.html");
    }

    // Cargo sets TARGET for every build script; the fallback keeps this honest rather than panicking
    // in some future Cargo that does not, and `unknown` is a triple the cloud refuses loudly rather
    // than one it might serve the wrong binary for.
    let target = std::env::var("TARGET").unwrap_or_else(|_ignored| "unknown".to_owned());
    println!("cargo::rustc-env=POS_EDGE_TARGET={target}");
    println!("cargo::rerun-if-changed=build.rs");
}
