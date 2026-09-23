// Finding an item by typing at it, on a till whose menu is Vietnamese.
//
// The order screen's grid is the fast path for the twenty things a store sells all day. A menu with
// two hundred items on it has a long tail that the grid cannot hold and a category tree cannot make
// quick — and the operator asking for one of them has a guest waiting. So: type a few letters, tap
// the item. The typing is not a tap (`docs/ui-ux.md` §6 counts taps, and the shift float and the
// manager's PIN are typed for the same reason), so the flow still costs the one tap the grid costs.
//
// # Why folding, and why this folding
//
// A Vietnamese keyboard puts the tones in with the letters, and a member of staff mid-service does
// not switch input mode to find *Phở bò*. `pho` has to reach it. Unicode does most of that for us:
// `NFD` splits a precomposed letter into its base and its combining marks, and dropping the marks
// leaves the base. What it does **not** do is `đ` — that letter has no decomposition, because it is
// not `d` with a mark on it; it is its own letter with a stroke through it. Unicode is right and the
// operator typing `banh mi dac biet` is also right, so this maps it by hand. It is the one
// exception, and the reason it is here rather than in a general-purpose table.
//
// Deliberately **not** a locale-aware collator. `Intl.Collator` with `sensitivity: "base"` compares
// whole strings; what a search needs is a *substring* test, which a collator does not offer. Folding
// both sides and using `includes` is the operation this actually is.

/** The form two strings are compared in: lower case, no tone marks, `đ` as `d`. */
export function fold(text: string): string {
  return (
    text
      .toLowerCase()
      // `Đ` lower-cases to `đ` first, so one replacement covers both.
      .replace(/đ/g, "d")
      .normalize("NFD")
      // The combining diacritical marks block — what NFD just split off.
      .replace(/[\u0300-\u036f]/g, "")
  );
}

/**
 * Whether any of `captions` contains `needle`. Every string here is **already folded** — the caller
 * runs both sides through [`fold`] first.
 *
 * Several captions per item rather than one, because an item has more than one name an operator
 * might reach for: the price book's, and whatever the console wrote on the button for it
 * (ADR-0066). Either should find it.
 *
 * Folded in, not folded here, because of where this is called from: once per menu item, on every
 * keystroke. Folding inside would normalize the same query on every item of the scan and re-fold
 * two hundred captions that have not changed since the menu loaded — a few hundred NFD passes per
 * letter typed, on the cheapest till in the estate. The caller has both a memo for the captions and
 * a memo for the needle, so each string is folded when it changes rather than when it is compared.
 *
 * An empty needle matches nothing rather than everything — the caller decides what an empty box
 * means, and for this screen it means "show the grid", not "show every item as a result".
 */
export function matches(needle: string, captions: readonly string[]): boolean {
  if (needle === "") {
    return false;
  }
  return captions.some((caption) => caption.includes(needle));
}
