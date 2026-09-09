// The theme choice: three states, and the one that means "stop choosing" (Wave 3 · Stage 6).
//
// The defect these were written from is an absence: `tokens.css` has honoured `data-theme` since P6
// and nothing in the console ever set it, so an operator could not ask for light or dark. The
// interesting part of adding the control is not the control — it is that "system" has to *remove*
// the attribute. Writing `data-theme="system"` would leave the stylesheet matching
// `:root:not([data-theme="light"])`, which silently means "not light", so an operator returning to
// "system" on a light machine would be handed dark. That is the case with a test.

import { beforeEach, describe, expect, it } from "vitest";

import { applyTheme, loadTheme, rememberTheme, THEMES, type Theme } from "../src/lib/theme";

function root(): HTMLElement {
  const element = document.createElement("html");
  document.body.appendChild(element);
  return element;
}

describe("applyTheme", () => {
  it("writes an explicit choice to the root element", () => {
    const element = root();
    applyTheme("dark", element);
    expect(element.dataset["theme"]).toBe("dark");
    applyTheme("light", element);
    expect(element.dataset["theme"]).toBe("light");
  });

  it("removes the attribute for system, rather than writing the word", () => {
    const element = root();
    applyTheme("dark", element);
    applyTheme("system", element);
    // Not `"system"`, and not `""` either: the stylesheet's `:not([data-theme="light"])` matches an
    // element carrying any other value, so anything left behind here reads as a choice of dark.
    expect(element.dataset["theme"]).toBeUndefined();
    expect(element.hasAttribute("data-theme")).toBe(false);
  });

  it("is idempotent, so applying the boot value twice is safe", () => {
    const element = root();
    for (const choice of THEMES) {
      applyTheme(choice, element);
      const first = element.getAttribute("data-theme");
      applyTheme(choice, element);
      expect(element.getAttribute("data-theme")).toBe(first);
    }
  });
});

describe("loadTheme", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("defaults to system, so a first run follows the operating system", () => {
    expect(loadTheme()).toBe("system");
  });

  it("reads back every choice it can store", () => {
    for (const choice of THEMES) {
      rememberTheme(choice);
      expect(loadTheme()).toBe(choice);
    }
  });

  it("treats an unrecognised stored value as system", () => {
    // A key written by an older release, a hand-edited value, a half-finished write. None of them
    // may leave the console stuck in a palette nobody chose.
    for (const junk of ["", "SYSTEM", "solarized", "null", "{}"]) {
      localStorage.setItem("pos.dashboard.theme", junk);
      expect(loadTheme(), junk).toBe("system");
    }
  });

  it("survives storage that throws, which is a private window", () => {
    const original = Storage.prototype.getItem;
    Storage.prototype.getItem = () => {
      throw new Error("access denied");
    };
    try {
      expect(loadTheme()).toBe("system");
    } finally {
      Storage.prototype.getItem = original;
    }
  });
});

describe("rememberTheme", () => {
  it("swallows a storage failure, because the choice still holds for this page", () => {
    const original = Storage.prototype.setItem;
    Storage.prototype.setItem = () => {
      throw new Error("quota exceeded");
    };
    try {
      const choice: Theme = "dark";
      expect(() => rememberTheme(choice)).not.toThrow();
    } finally {
      Storage.prototype.setItem = original;
    }
  });
});
