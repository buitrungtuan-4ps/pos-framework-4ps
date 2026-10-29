# Templates for `cargo xtask new`

What the generators write ([ADR-0176](../../docs/adr/0176-the-adapter-and-country-standard.md)).
Each file is embedded in `xtask` with `include_str!` and filled in by plain `{{placeholder}}`
substitution; a placeholder the generator does not fill is an error, not an empty string. The
`.tmpl` suffix keeps Cargo, rustfmt and the link check from reading them as real files.

- `crate/`: `cargo xtask new crate`, the base every other generator builds on.
- `adapter/<kind>/`: `cargo xtask new adapter --kind <kind>`: the crate root (`lib.rs.tmpl`), a
  transport seam with a pure core implementing the port, and `contract.rs.tmpl`, the port's shared
  suite against an in-memory transport. A template is kept as rustfmt formats it.
- `country/`: `cargo xtask new country <cc>`: the module (`lib.rs.tmpl`) with every legal value
  from a flag, its README with the values table, and `contract.rs.tmpl`, the `Fiscalization` suite.
  The manifest comes from `crate/`.
- `screen/ui/`: `cargo xtask new screen --app ui`: a till screen with its title, and nothing else.
- `screen/dashboard/`: `cargo xtask new screen --app dashboard`: a console screen with its title,
  and a vitest test that it draws it.

A change here changes the matching golden file under [`../tests/golden/`](../tests/golden/README.md)
in the same pull request.
