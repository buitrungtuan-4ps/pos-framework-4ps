// The four files a store server needs, and what it takes to hand them over a second time.
//
// # Why this is not just a helper inside the wizard
//
// The new-store wizard emitted these four artifacts and nothing else in the console did, which made
// them a one-shot: the moment the wizard's last step closed, the only way to get an installer for
// that store again was to create a *second* store. That is fine right up until the day a shop's
// machine dies, which is the day it matters most — and on that day the operator's real question is
// not "where is the download button", it is "which of these four things can I regenerate and which
// are gone with the disk".
//
// So the answer lives here rather than in a screen, in two parts: the file list (what to emit, under
// what name, with which MIME type) and [`RECOVERY`] (what each file's contents cost to reproduce).
// Both are data, both are testable without a browser, and both are read by the wizard's handoff step
// and by the Stores screen's replacement drawer.
//
// # What "the server died" actually costs
//
// Three of the four files are pure functions of the store's registry row plus the cloud's own
// origin, so they regenerate exactly. The credentials are the interesting part, and they are two
// different secrets with two different recoveries:
//
//   * **The scoped store key** (`POS_EDGE_SYNC_KEY`) is shown once at issuance and stored hashed, so
//     it cannot be read back. A replacement box needs a **fresh** key — which is a write, which is
//     why the drawer asks rather than doing it on open — and the dead box's key should then be
//     revoked, because a key nobody holds is still a key that works.
//   * **The device credential** — the thing activation mints — never leaves the box it was minted
//     for: it lives in that machine's OS keyring (ADR-0086). It is *not* in any of these four files
//     and no console screen can show it. A replacement box is an unactivated box and needs a new
//     activation code, which is the step operators miss, because everything else about the handoff
//     looks complete without it.
//
// And one thing no file can carry: the store's own event log. `store.sqlite` is on the dead disk.
// Whatever it had already published to the cloud is safe in the fleet stream; whatever was still in
// its outbox is not (ADR-0046's store-side backup is unbuilt, and that is recorded, not hidden).

import {
  configToml,
  envFile,
  linuxInstaller,
  windowsInstaller,
} from "../installers.mjs";
import type { InstallerValues } from "../installers.d.mts";
import type { MessageKey } from "../i18n";

/** One downloadable artifact: the name the browser saves it under, and how to render it. */
export interface HandoffFile {
  /** The file name. Load-bearing — the installers' own help text names their siblings. */
  readonly name: string;
  readonly mime: string;
  /** The download button's label. */
  readonly labelKey: MessageKey;
  /** One line on what this file is for. */
  readonly hintKey: MessageKey;
  readonly render: (values: InstallerValues) => string;
  /** Whether the rendered file embeds the store key, and so must be deleted after the install. */
  readonly holdsSecret: boolean;
}

/**
 * The artifacts, installers first.
 *
 * Installers lead because each one *contains* the two files below it, so an operator who takes only
 * the script for their OS has everything. `config.toml` and `env` stay for the cases a script cannot
 * cover: a hand-managed host, a box that is already half-installed, or a technician who wants to
 * read the configuration before it is written to a shop's only till.
 */
export const HANDOFF_FILES: readonly HandoffFile[] = [
  {
    name: "install-pos-edge.ps1",
    // Not `application/x-powershell`: no registry entry for it, and a MIME type the browser does
    // not know can turn a save into an open. Plain text saves.
    mime: "text/plain",
    labelKey: "handoff.downloadWindows",
    hintKey: "handoff.windowsHint",
    render: windowsInstaller,
    holdsSecret: true,
  },
  {
    name: "install-pos-edge.sh",
    mime: "application/x-shellscript",
    labelKey: "handoff.downloadLinux",
    hintKey: "handoff.linuxHint",
    render: linuxInstaller,
    holdsSecret: true,
  },
  {
    name: "config.toml",
    mime: "application/toml",
    labelKey: "handoff.downloadConfig",
    hintKey: "handoff.configHint",
    render: configToml,
    holdsSecret: false,
  },
  {
    name: "env",
    mime: "text/plain",
    labelKey: "handoff.downloadEnv",
    hintKey: "handoff.envHint",
    render: envFile,
    holdsSecret: true,
  },
];

/** How recoverable one part of a store's identity is, when the box holding it is gone. */
export type Recovery =
  /** A pure function of the registry row and this cloud's origin. Regenerated on demand. */
  | "regenerated"
  /** Shown once and stored hashed. A replacement needs a new one issued. */
  | "reissued"
  /** Never left the box. Not in any file here, and no screen can show it. */
  | "lost";

/** One row of the "what can I get back" table the replacement drawer leads with. */
export interface RecoveryFact {
  readonly labelKey: MessageKey;
  readonly recovery: Recovery;
  /** What to do about it, in one sentence. */
  readonly actionKey: MessageKey;
}

/**
 * The four facts, worst last.
 *
 * Ordered so the reading ends on the two that need an action, because the failure this list exists
 * to prevent is an operator who downloads the files, installs them on the new machine, and finds a
 * box that boots but will not sell — the device credential being the reason, and the one thing they
 * were never told about.
 */
export const RECOVERY: readonly RecoveryFact[] = [
  {
    labelKey: "handoff.factIdentity",
    recovery: "regenerated",
    actionKey: "handoff.factIdentityAction",
  },
  {
    labelKey: "handoff.factKey",
    recovery: "reissued",
    actionKey: "handoff.factKeyAction",
  },
  {
    labelKey: "handoff.factCredential",
    recovery: "lost",
    actionKey: "handoff.factCredentialAction",
  },
  {
    labelKey: "handoff.factLog",
    recovery: "lost",
    actionKey: "handoff.factLogAction",
  },
];

/**
 * Saves `body` as a real file download.
 *
 * A `Blob` and a synthetic `<a download>`, not a `data:` URL: the console is served by `pos_cloud`
 * from the same origin, so the browser honours the file name given here and the operator does not
 * have to rename what they downloaded. The object URL is revoked immediately — the click has already
 * been dispatched synchronously by then, and leaving it alive pins the whole file in memory for the
 * life of the document.
 *
 * The `Blob` constructor encodes a JavaScript string as UTF-8, which is what carries the PowerShell
 * scripts' leading U+FEFF out as the `EF BB BF` bytes Windows PowerShell 5.1 needs to read the
 * file as UTF-8 rather than as the machine's ANSI code page. Strip that and a store name with a
 * Vietnamese diacritic in it becomes a parse error.
 */
export function downloadFile(name: string, body: string, mime: string): void {
  const url = URL.createObjectURL(new Blob([body], { type: mime }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}

/** Renders one artifact for `values` and saves it. */
export function downloadHandoff(file: HandoffFile, values: InstallerValues): void {
  downloadFile(file.name, file.render(values), file.mime);
}
