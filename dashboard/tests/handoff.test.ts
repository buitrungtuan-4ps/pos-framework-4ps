// The handoff, rendered a second time for a store that already exists.
//
// The wizard could hand a technician four files for a store it had just created; nothing could hand
// them over again. What that cost was invisible until the day a shop's machine dies, so the
// properties pinned here are the ones that day depends on:
//
//   * the four files render for a store the wizard is no longer holding — a plain registry row plus
//     the browser's origin is enough, which is what makes this need no new cloud route;
//   * the store key reaches the files that are supposed to carry it and *only* those. `config.toml`
//     is the one an operator pastes into a ticket or a chat, and a credential in it would leak by
//     the most ordinary route there is;
//   * the two PowerShell artifacts keep their byte-order mark through the render, because that mark
//     is the difference between an installer that runs and one that dies at parse time on a
//     Vietnamese store name (see `PS_BOM` in `src/installers.mjs`);
//   * every message key the recovery table and the file list name actually exists. They are read
//     through `t()` at render time, so a typo is a screen that shows a key to an operator rather
//     than a compile error.

import { describe, expect, it } from "vitest";

import { HANDOFF_FILES, RECOVERY } from "../src/lib/handoff";
import type { InstallerValues } from "../src/installers.d.mts";
import en from "../src/i18n/en.json";

/**
 * A store as this screen has it: the registry row, the tenant in context, and the origin the
 * console is being served from. Nothing the wizard held privately.
 *
 * The name is deliberately non-ASCII. A Vietnamese store name is the ordinary case, not the edge
 * case, and it is what turns a missing byte-order mark from a cosmetic detail into a store that
 * cannot be installed.
 */
const VALUES: InstallerValues = {
  storeName: "4P's Bến Thành",
  storeId: "01M221BB8BB5SESQDB895SJHJS",
  tenantLabel: "Pizza 4P's Vietnam",
  tenantId: "01M22190WCY5PS7KCA7ET7H679",
  cloudUrl: "https://cloud.example.com",
  cloudHost: "cloud.example.com",
  bindPort: "",
  // Deliberately **not** shaped like a vendor credential — see the note in
  // `store-handoff.test.tsx`. A fixture wearing a live-key prefix is indistinguishable from a real
  // leak to every scanner that will ever read this repository.
  key: "test-store-key-not-a-credential",
};

const named = (name: string) => {
  const found = HANDOFF_FILES.find((file) => file.name === name);
  if (found === undefined) {
    throw new Error(`no handoff file is named ${name}`);
  }
  return found;
};

describe("the files a replacement box needs", () => {
  it("renders all four from a registry row and an origin", () => {
    expect(HANDOFF_FILES.map((file) => file.name)).toEqual([
      "install-pos-edge.ps1",
      "install-pos-edge.sh",
      "config.toml",
      "env",
    ]);
    for (const file of HANDOFF_FILES) {
      const body = file.render(VALUES);
      expect(body.length).toBeGreaterThan(0);
      // The store this is for has to be in the file, or the technician is holding four
      // indistinguishable downloads for an estate of shops.
      expect(body).toContain(VALUES.storeId);
    }
  });

  it("keeps the key out of the file an operator pastes into a chat", () => {
    // `config.toml` is the readable one — it is quoted in tickets, screenshotted, and checked into
    // a fork's own repository. It carries the store's identity and no credential, and the type says
    // so, so the two claims cannot drift apart.
    expect(named("config.toml").holdsSecret).toBe(false);
    expect(named("config.toml").render(VALUES)).not.toContain(VALUES.key);

    for (const file of HANDOFF_FILES.filter((entry) => entry.holdsSecret)) {
      expect(file.render(VALUES)).toContain(VALUES.key);
    }
  });

  it("emits a credential-less env file rather than a broken one, when no key was issued", () => {
    // The "this box already holds its key" path. It must produce a file that installs — the store
    // trades either way — and it must not smuggle a placeholder that looks like a credential.
    const body = named("env").render({ ...VALUES, key: null });
    expect(body).not.toContain(VALUES.key);
    expect(body.length).toBeGreaterThan(0);
  });

  it("carries the PowerShell byte-order mark out through the render", () => {
    // Windows PowerShell 5.1 reads a BOM-less script in the machine's ANSI code page, where the
    // store name above becomes mojibake and one of its bytes is U+201D — which the parser accepts
    // as a string delimiter. The mark is what makes the file UTF-8 to the edition a shop actually
    // runs, and the `Blob` in `downloadFile` encodes it as the EF BB BF bytes on the way out.
    expect(named("install-pos-edge.ps1").render(VALUES).startsWith("﻿")).toBe(true);
    // And the shell script must NOT have one: three bytes before `#!` stop it being a shebang.
    expect(named("install-pos-edge.sh").render(VALUES).startsWith("#!")).toBe(true);
    expect(named("config.toml").render(VALUES).startsWith("﻿")).toBe(false);
  });
});

describe("what a dead machine takes with it", () => {
  it("names the device credential as unrecoverable, which is the fact operators miss", () => {
    // Not a spelling check. The failure this table exists to prevent is a replacement box that
    // installs, boots and then will not sell, because the credential activation minted lives in the
    // dead machine's keyring and no file here carries it. If that row ever softens to "re-issue",
    // the drawer stops warning about the one step that has no download button.
    const credential = RECOVERY.find((fact) => fact.labelKey === "handoff.factCredential");
    expect(credential?.recovery).toBe("lost");
    const log = RECOVERY.find((fact) => fact.labelKey === "handoff.factLog");
    expect(log?.recovery).toBe("lost");
  });

  it("has a message for every key it names", () => {
    // These keys are resolved at render time, so a typo shows an operator `handoff.factKeyAction`
    // where a sentence should be. `i18n:parity` proves en and vi agree; this proves en has them.
    const messages = en as Record<string, string>;
    for (const fact of RECOVERY) {
      expect(messages[fact.labelKey], fact.labelKey).toBeTruthy();
      expect(messages[fact.actionKey], fact.actionKey).toBeTruthy();
    }
    for (const file of HANDOFF_FILES) {
      expect(messages[file.labelKey], file.labelKey).toBeTruthy();
      expect(messages[file.hintKey], file.hintKey).toBeTruthy();
    }
  });
});
