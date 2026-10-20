---
kind: added
---
- **`cargo xtask new adapter --kind printer|storage` writes a sandbox adapter that passes its port's
  contract suite from its first build** (ADR-0176). It writes `crates/adapters/<kind>-sandbox`:
  - a `PrinterDriver` or `BlobStore` implemented over a transport seam;
  - `tests/contract.rs`, which runs the port's shared suite against an in-memory transport;
  - the crate's card, README and workspace line.

  It names no vendor. The crates it depends on still need their README's Used by line updated,
  which `cargo xtask crates` prints. The generator does not edit another crate's files.
