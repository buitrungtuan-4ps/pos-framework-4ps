// A store's country against its cell's home country (ADR-0179 C6), for the store settings form.
//
// The cell's home country is fixed when the server starts, so it is read once per console load and
// shared. The form's country hint then says, before anything is published, when the country chosen
// is not the cell's: a warning, never a refusal. The store hub's cell card is where the difference
// is answered once the country is published.

import { createSignal } from "solid-js";

import { getCell } from "../api/cell";
import { t } from "../i18n";

const [home, setHome] = createSignal<string | null>(null);
let asked = false;

/** The cell's home country, or `null` until it is read, or when the cell has none. */
export function cellHomeCountry(): string | null {
  if (!asked) {
    asked = true;
    // Started inside the chain, so a read that throws before it returns a promise is caught too:
    // a hint must never take the form down with it.
    void Promise.resolve()
      .then(getCell)
      .then((cell) => setHome(cell.home_country))
      .catch(() => {
        // Asked again on the next render: a failed read is no reason to stop warning for good.
        asked = false;
      });
  }
  return home();
}

/** The country field's hint, with the cell warning added when `country` is not the cell's. */
export function countryHint(country: string): string {
  const base = t("storeSettings.countryHint");
  const cellCountry = cellHomeCountry();
  const own = country.trim().toUpperCase();
  if (cellCountry === null || own === "" || own === cellCountry) {
    return base;
  }
  return `${base} ${t("storeSettings.countryOtherCell", { country: own, cellCountry })}`;
}

/** A vendor connection's processing-country hint, with the cell warning added when `country` is
 *  not the cell's home country (ADR-0179 C7). A warning only: the connection still saves. */
export function processingHint(country: string): string {
  const base = t("integrations.processingHint");
  const cellCountry = cellHomeCountry();
  const own = country.trim().toUpperCase();
  if (cellCountry === null || own === "" || own === cellCountry) {
    return base;
  }
  return `${base} ${t("integrations.processingOtherCell", { country: own, cellCountry })}`;
}
