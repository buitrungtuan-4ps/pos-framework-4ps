# Templates for `cargo xtask new`

What the generators write ([ADR-0176](../../docs/adr/0176-the-adapter-and-country-standard.md)).
Each file is embedded in `xtask` with `include_str!` and filled in by plain `{{placeholder}}`
substitution; a placeholder the generator does not fill is an error, not an empty string. The
`.tmpl` suffix keeps Cargo, rustfmt and the link check from reading them as real files.

- `crate/`: `cargo xtask new crate`, the base every other generator builds on.

A change here changes the matching golden file under [`../tests/golden/`](../tests/golden/README.md)
in the same pull request.
