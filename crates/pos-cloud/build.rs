// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Ensures the embedded-dashboard directory exists at compile time.
//!
//! `dashboard/dist` is the SolidJS build output (ADR-0060) and is gitignored, so a fresh checkout
//! has no `dashboard/dist/index.html` for `rust-embed` to embed and the crate would not compile.
//! This writes a placeholder there when it is absent — enough for the cloud to serve a page and for
//! the tests to pass — and it **never overwrites a real build**: if `index.html` already exists, it
//! does nothing. This mirrors `pos_edge`'s `build.rs` exactly. `rust-embed` emits its own
//! rerun-if-changed for the folder, so no such instruction is needed here.

use std::path::Path;

/// The placeholder served until the dashboard toolchain builds the real interface. Kept in step with
/// the tests, which assert the served page names the product.
const PLACEHOLDER: &str = "<!doctype html>\n\
<html lang=\"en\">\n\
  <head>\n\
    <meta charset=\"utf-8\" />\n\
    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
    <title>Pizza 4P's — Back office</title>\n\
  </head>\n\
  <body>\n\
    <main>\n\
      <h1>Pizza 4P's — Back office</h1>\n\
      <p>The cloud is running. Build the dashboard (dashboard/: pnpm build) to embed the real UI.</p>\n\
      <p>Health: <a href=\"/health\">/health</a></p>\n\
    </main>\n\
  </body>\n\
</html>\n";

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let dist = Path::new(&manifest).join("../../dashboard/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::create_dir_all(&dist).expect("create dashboard/dist");
        std::fs::write(&index, PLACEHOLDER).expect("write placeholder index.html");
    }
}
