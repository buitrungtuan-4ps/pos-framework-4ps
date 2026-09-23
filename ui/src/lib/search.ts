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

// Single-entry cache for fold() to avoid repeated Unicode normalization and regexes
// when fold() is called on the same string (e.g. query.trim()) across array filters.
let lastFoldInput: string | null = null;
let lastFoldOutput = "";

/** The form two strings are compared in: lower case, no tone marks, `đ` as `d`. */
export function fold(text: string): string {
  if (text === lastFoldInput) {
    return lastFoldOutput;
  }
  const result = text
    .toLowerCase()
    // `Đ` lower-cases to `đ` first, so one replacement covers both.
    .replace(/đ/g, "d")
    .normalize("NFD")
    // The combining diacritical marks block — what NFD just split off.
    .replace(/[\u0300-\u036f]/g, "");
  lastFoldInput = text;
  lastFoldOutput = result;
  return result;
}

/**
 * Whether any of `captions` contains `query`, both folded.
 * Accepts optional pre-folded captions `foldedCaptions` to bypass re-folding static captions during typing.
 *
 * Several captions per item rather than one, because an item has more than one name an operator
 * might reach for: the price book's, and whatever the console wrote on the button for it
 * (ADR-0066). Searching only the first would fail the operator who knows the item by what the grid
 * calls it.
 *
 * An empty query matches nothing rather than everything — the caller decides what an empty box
 * means, and for this screen it means "show the grid", not "show every item as a result".
 */
export function matches(
  query: string,
  captions: readonly string[],
  foldedCaptions?: readonly string[],
): boolean {
  const needle = fold(query.trim());
  if (needle === "") {
    return false;
  }
  const targets = foldedCaptions ?? captions.map((caption) => fold(caption));
  return targets.some((caption) => caption.includes(needle));
}
