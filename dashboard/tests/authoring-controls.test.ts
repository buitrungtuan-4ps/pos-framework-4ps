// One way to author an entity, enforced ([ADR-0121](../../docs/adr/0121-one-way-to-author-an-entity.md) §7).
//
// # Why this suite exists, in one sentence
//
// The console had this before. `components/ui.tsx` shipped with P6, `components/kit.tsx` with F2, and
// by the time anybody counted there were 45 raw `<select>` and 46 raw `<input>` across 22 screens
// anyway — every one of them written by somebody who could have imported a primitive and did not,
// because nothing said they had to and each one on its own was easier than looking. ADR-0121 §7
// calls this gate "not optional" for exactly that reason: U1 built the kit and U2 adopted it, and
// without a gate U2 is a snapshot rather than a floor.
//
// # What it checks, and what it deliberately does not
//
// Two properties, both about `src/screens/**`:
//
//   1. **No raw form control.** A `<select>`, `<input>` or `<textarea>` written by hand in a screen
//      is a violation. Every shape the console actually needs has a primitive — `TextField`,
//      `SelectField`, `MultiSelectField`, `CheckboxField`, `NumberField`, `MoneyField`, `TextArea`,
//      `FileButton`, `CellField` — and the last two exist *because* the sweep found shapes the
//      others could not express (a file picker; a grid cell whose label is its column header). If a
//      new shape turns up that none of them fit, the answer is a new primitive, not a raw control:
//      that is the whole argument of §5, and it is cheap, because writing it once is what the screen
//      would have done by hand anyway.
//
//   2. **No create/edit form standing open in a `Card`.** This is the owner's original report —
//      "thêm đang hiện nguyên cái card bự ra", adding shows a whole big card — and the property is
//      that a form appears when an operator asks for one. `FormPanel` is how a screen asks; a
//      screen that mounts labelled fields inside a `Card` is asking nobody.
//
// It does **not** check `components/`, which is where the primitives live and therefore where the
// raw controls belong, nor `App.tsx`/`Login.tsx`-style shell files outside `screens/`. It does not
// check that a screen uses `useEntityCrud`: a screen with no writes has nothing to author, and a
// gate that demanded the lifecycle everywhere would be demanding it of the Reports screen.

import { describe, expect, it } from "vitest";

const sources: Record<string, string> = import.meta.glob("../src/screens/**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
});

/**
 * The file with its comments stripped.
 *
 * Same reasoning as `stale-writes.test.ts`: a check a comment can satisfy — or, here, a check a
 * comment can *fail* — checks the wrong thing. Several screens explain in prose what they used to
 * do ("the kind was a `<select>` in the table cell that wrote on `change`"), and that history is
 * worth keeping. Block comments go first, then whole-line `//`, then trailing `//` that is not part
 * of a `://` so a URL in a string survives.
 */
function code(text: string): string {
  return text
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .map((line) => (line.trimStart().startsWith("//") ? "" : line.replace(/(?<!:)\/\/.*$/, "")))
    .join("\n");
}

const entries = Object.entries(sources).map(
  ([path, text]) => [path.replace("../src/screens/", ""), code(text)] as [string, string],
);

/** A hand-written form control. JSX components are capitalised, so the lower-case tag is the tell. */
const RAW_CONTROL = /<(select|input|textarea)[\s>/]/;

/**
 * A **list** screen whose fields are on the page rather than in a panel.
 *
 * The three conditions together are what make this the report's shape and not a false alarm:
 *
 *   * `<DataTable` — this screen's job is to show rows;
 *   * a field primitive — it also asks for input;
 *   * no panel of any kind — so that input is standing open under the table.
 *
 * The "list screen" clause is load-bearing. Without it the check flags eight screens that are
 * fields on a page *by design* — Login and Setup and AcceptInvite (the form **is** the page, there
 * is nothing to ask for), Reports (a date range that filters what is shown), StoreSettings and
 * Channels (settings, edited in place and published), NewStore (a wizard step), StoreHub (one
 * acknowledgement beside the store it is about). A gate that fails on those is a gate somebody
 * switches off, and then it protects nothing.
 *
 * Deliberately shallow in the other direction: it does not try to catch a screen that has a panel
 * *and* a stray field on the page, which is a judgement a reviewer makes better than a regex. The
 * precise version needs a JSX parse and a tree walk, and is worth building the day this one lets
 * something through.
 */
const FIELD_PRIMITIVES = [
  "TextField",
  "SelectField",
  "MultiSelectField",
  "CheckboxField",
  "NumberField",
  "MoneyField",
  "TextArea",
];
const PANELS = ["FormPanel", "Drawer", "Modal", "ConfirmDialog"];

function authorsOnThePage([, text]: [string, string]): boolean {
  if (!text.includes("<DataTable")) {
    return false;
  }
  const hasFields = FIELD_PRIMITIVES.some((name) => text.includes(`<${name}`));
  const hasPanel = PANELS.some((name) => text.includes(`<${name}`));
  return hasFields && !hasPanel;
}

describe("the authoring kit", () => {
  it("finds the screens, so this suite cannot pass by looking at nothing", () => {
    expect(entries.length).toBeGreaterThan(30);
  });

  it("owns every form control, so no screen hand-rolls one", () => {
    const offenders = entries
      .filter(([, text]) => RAW_CONTROL.test(text))
      .map(([path]) => path)
      .sort();
    expect(offenders).toEqual([]);
  });

  it("is where the fields are, so no list screen leaves a form standing open under its table", () => {
    const offenders = entries.filter(authorsOnThePage).map(([path]) => path).sort();
    expect(offenders).toEqual([]);
  });
});
