# Golden files for `cargo xtask new`

Each directory holds, byte for byte, what one generator writes for the fixed input its test names.
The `.golden` suffix keeps Cargo, rustfmt and the link check from reading them as real files. A
change to a template changes its golden file in the same pull request.

- `new-crate/`: `cargo xtask new crate pos-example --layer tool --purpose "An example crate the
  generator's golden test renders." --adr 170 --adr 176`, and `members.golden` is the line it adds to
  the workspace, after its last sibling.
- `new-adapter/<kind>/`: `cargo xtask new adapter --kind <kind>`, for printer, storage, card,
  fiscal, courier, erp and delivery. Each generated crate passed its port's contract suite, clippy with
  `-D warnings` and rustfmt before its files were copied here.
- `new-country/`: `cargo xtask new country xa --display-name "Example country" --currency XTS
  --exponent 2 --prices-include-tax false --cash-rounding 5 --tax-rate food=1000 --tax-rate
  alcohol=1000 --tax-rate exempt=0 --tax-rate food@takeaway=800 --retention-days 365 --language en`.
  `XA` is an ISO user-assigned code and `XTS` the currency code ISO reserves for testing, so neither
  names a real market. `wiring.golden` is each line it adds to the binaries, by file. The module
  passed the fiscalization suite, its own tests, clippy and rustfmt, and both binaries built with
  `country-xa`, before its files were copied here.
- `new-screen/ui/`: `cargo xtask new screen --app ui stockCount --path /stock --title "Stock count"
  --title-vi "Kiểm kho"`. `StockCount.tsx.golden` is the screen it writes; `edits.golden` is each line
  it adds to an existing file, as `<file>: <line>`.
- `new-screen/dashboard/`: `cargo xtask new screen --app dashboard stockCount --path /stock --title
  "Stock count" --title-vi "Kiểm kho" --scope store --icon package --nav-group overview`: the screen,
  its vitest stub, and in `edits.golden` each line it adds or changes in an existing file.
