// A tax component's name, as the console checks it before the cloud does
// ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 2).
//
// A settled bill records a rate's parts, such as CGST and SGST, under their names, and the cloud
// sums its reports by them, so a name is a code rather than free text: two to eight upper-case
// letters and digits, starting with a letter. The cloud's `TaxComponentName` is the rule and refuses
// any other name on save; this is the console's one copy of it, so the tax grid can say which cell
// is wrong while it is being typed rather than after the save.

/** Two to eight upper-case ASCII letters and digits, the first a letter. */
const CODE = /^[A-Z][A-Z0-9]{1,7}$/u;

/**
 * Whether `name` is a tax component's code, such as `CGST`, `SGST`, `UTGST` or `CESS`.
 *
 * Nothing is trimmed or upper-cased here: the grid upper-cases what is typed, where the operator
 * sees it, and this answers for the name as it would be saved.
 */
export function isTaxComponentName(name: string): boolean {
  return CODE.test(name);
}
