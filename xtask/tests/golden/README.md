# Golden files for `cargo xtask new`

Each directory holds, byte for byte, what one generator writes for the fixed input its test names.
The `.golden` suffix keeps Cargo, rustfmt and the link check from reading them as real files. A
change to a template changes its golden file in the same pull request.

- `new-crate/`: `cargo xtask new crate pos-example --layer tool --purpose "An example crate the
  generator's golden test renders." --adr 170 --adr 176`, and `members.golden` is the line it adds to
  the workspace, after its last sibling.
