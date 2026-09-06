// Puts every artifact the new-store wizard generates through a real parser before it can ship.
//
// The scripts in `src/installers.mjs` are typed by nobody and run with administrator rights on a
// shop's only till. A stray quote in an embedded heredoc is a store that does not open, and until
// this gate existed nothing checked them at all — `sh -n` appears nowhere else in the tree, and the
// Windows script did not exist, which issue #182 attributed precisely to the absence of a way to
// check one.
//
// Two halves, because no single runner can parse both languages:
//
//   * **Here**, on any machine with a POSIX shell: `sh -n` over the generated `install-pos-edge.sh`,
//     and a TOML sanity pass over `config.toml`. Runs as part of `pnpm build`.
//   * **On the Windows CI runner**, which is the only place with a PowerShell parser: this script is
//     invoked with `--emit <dir>` to write the artifacts out, and the workflow then parses the
//     `.ps1` with `[System.Management.Automation.Language.Parser]`. `--emit` skips `sh -n`, since a
//     Windows runner has no `sh`.
//
// The values below are deliberately hostile. A store is named by a person in a form, so the name
// carries an unbalanced quote and an unbalanced backtick — which is what makes this gate provably
// non-vacuous: remove the escaping in `installers.mjs` and `sh -n` fails here rather than passing.
//
// It also carries `$HOME` and a *balanced* backtick pair, which `sh -n` accepts happily: a name like
// `Quán \`id\`` is not a syntax error, it is a command substitution that would run as root when the
// technician executes the script. No parser catches that one — the escaping does — so both live in
// the same case, and this comment is the record of which half each mechanism covers.

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { argv, exit } from "node:process";

import {
  configToml,
  envFile,
  linuxInstaller,
  printAgentInstallerTemplate,
  windowsInstaller,
  windowsInstallerTemplate,
} from "../src/installers.mjs";

/**
 * The checked-in templates, relative to this script, each with the generator it must match.
 *
 * Two entries rather than one variable, because the print agent's installer (ADR-0112) is a second
 * script that runs elevated on a machine in a shop: it earns the same gate, for the same reason.
 */
const TEMPLATES = [
  {
    path: new URL("../../deploy/edge/install-pos-edge.ps1", import.meta.url),
    name: "deploy/edge/install-pos-edge.ps1",
    emit: "install-template.ps1",
    generate: windowsInstallerTemplate,
    generator: "windowsInstallerTemplate",
  },
  {
    path: new URL("../../deploy/edge/install-pos-print-agent.ps1", import.meta.url),
    name: "deploy/edge/install-pos-print-agent.ps1",
    emit: "install-print-agent-template.ps1",
    generate: printAgentInstallerTemplate,
    generator: "printAgentInstallerTemplate",
  },
];

/**
 * The stand-in for a store's scoped key.
 *
 * Deliberately repetitive, so its Shannon entropy (~3.0) sits below the 3.5 threshold gitleaks'
 * `generic-api-key` rule uses. A realistic-looking fixture — the first version of this file used
 * `pos_sk_live_…`, entropy 4.49 — fails the `secrets` job, and **correctly**: a literal shaped like a
 * live credential beside a field called `key` is exactly what that gate exists to catch. Adding it to
 * an allowlist instead would teach the next person that the scanner can be argued with, which is a
 * worse outcome than an ugly fixture.
 *
 * It still ends in a quote, because the key reaches PowerShell inside a single-quoted string where
 * doubling is the only escape — so this fixture proves that path too.
 */
const SAMPLE_KEY = "not-a-secret-not-a-secret-not-a-secret'";

/** @type {import("../src/installers.d.mts").InstallerValues} */
const HOSTILE = {
  storeName: `Quán "Bảy \\ $HOME \`id\` \` 4P's`,
  storeId: "01JBQ9ZK7X8N4M2P6R3T5V7W9Y",
  tenantLabel: "Pizza 4P's — Việt Nam",
  tenantId: "01JBQ9ZK7X8N4M2P6R3T5V7W9Z",
  cloudUrl: "https://cloud.example.com",
  cloudHost: "cloud.example.com",
  bindPort: "9100",
  key: SAMPLE_KEY,
};

/** The same store before a key was issued — the other branch of every generator. */
const NO_KEY = { ...HOSTILE, key: null };

/** Ordinary values, so a failure here is never blamed on the hostile ones. */
const PLAIN = {
  ...HOSTILE,
  storeName: "Le Van Sy",
  tenantLabel: "Pizza 4P's",
  bindPort: "",
  key: SAMPLE_KEY,
};

const CASES = [
  { label: "hostile", values: HOSTILE },
  { label: "no-key", values: NO_KEY },
  { label: "plain", values: PLAIN },
];

/**
 * The checks a TOML file has to pass here. Not a parser — `cargo test` owns that — but enough to
 * catch the one mistake this generator can actually make: a key emitted *below* the `[nats]` header,
 * which the edge reads as `nats.<key>` and refuses under `deny_unknown_fields`.
 *
 * @param {string} toml
 * @param {string} where
 */
function checkToml(toml, where) {
  const lines = toml.split("\n");
  const table = lines.findIndex((line) => line.startsWith("["));
  if (table === -1) {
    throw new Error(`${where}: no [nats] table, so the store publishes nowhere`);
  }
  const stray = lines
    .slice(table + 1)
    .findIndex((line) => /^\s*(store_id|cloud_url|bind|advertised_ip|store_path)\s*=/u.test(line));
  if (stray !== -1) {
    throw new Error(
      `${where}: top-level key on line ${table + stray + 2} sits below [${lines[table]}], so the edge reads it as nats.* and refuses the file`,
    );
  }
  if (!lines.some((line) => line.startsWith("store_id = "))) {
    throw new Error(`${where}: no store_id, so the box does not know which store it is`);
  }
}

/**
 * Writes every artifact for one case into `dir` and returns what was written.
 *
 * @param {string} dir
 * @param {{label: string, values: import("../src/installers.d.mts").InstallerValues}} testCase
 */
function emit(dir, testCase) {
  const { label, values } = testCase;
  const written = [];
  for (const [name, body] of [
    [`config-${label}.toml`, configToml(values)],
    [`env-${label}`, envFile(values)],
    [`install-${label}.sh`, linuxInstaller(values)],
    [`install-${label}.ps1`, windowsInstaller(values)],
  ]) {
    const path = join(dir, name);
    writeFileSync(path, body, "utf8");
    written.push(path);
  }
  checkToml(configToml(values), `config.toml (${label})`);
  return written;
}

const emitFlag = argv.indexOf("--emit");
const outDir = emitFlag === -1 ? mkdtempSync(join(tmpdir(), "pos-installers-")) : argv[emitFlag + 1];

if (!outDir) {
  console.error("usage: installer-syntax.mjs [--emit <dir>]");
  exit(2);
}
mkdirSync(outDir, { recursive: true });

let failures = 0;

// The checked-in templates are emitted from the same generators, so this is the check that keeps
// them from drifting: two definitions of a service registration is one that nobody runs until a
// store will not come up.
//
// Compare content, not line-ending policy. A Windows checkout hands these back with CRLF — git's
// `autocrlf` is on by default on the runner — while the generators emit LF, and without normalising
// the gate fails on a file that has not drifted by a single character. `.gitattributes` pins both
// checked-in files to LF so this should not arise; this is the belt to that pair of braces.
const lf = (text) => text.replace(/\r\n/gu, "\n");
for (const template of TEMPLATES) {
  const onDisk = readFileSync(template.path, "utf8");
  if (lf(onDisk) !== lf(template.generate())) {
    console.error(
      `✗ ${template.name} no longer matches ${template.generator}().\n` +
        "  Regenerate it:  node -e 'import(\"./dashboard/src/installers.mjs\").then(m => " +
        `require("fs").writeFileSync("${template.name}", m.${template.generator}()))'`,
    );
    failures += 1;
  } else {
    console.log(`✓ ${template.name} matches its generator`);
  }

  // Written before any failure can exit: a drift failure must not also rob the Windows job of the
  // files it most needs to parse. The first run of this gate did exactly that — the emission sat at
  // the end of the script, so the checked-in template was never parsed on the run that reported
  // drift.
  if (emitFlag !== -1) {
    writeFileSync(join(outDir, template.emit), onDisk, "utf8");
  }
}

for (const testCase of CASES) {
  let written;
  try {
    written = emit(outDir, testCase);
  } catch (error) {
    console.error(`✗ ${testCase.label}: ${error instanceof Error ? error.message : String(error)}`);
    failures += 1;
    continue;
  }

  if (emitFlag !== -1) {
    // The Windows half. The workflow parses the .ps1 files this wrote; there is no `sh` here.
    continue;
  }

  const script = written.find((path) => path.endsWith(".sh"));
  try {
    execFileSync("sh", ["-n", script], { stdio: "pipe" });
    console.log(`✓ ${testCase.label}: install-pos-edge.sh parses`);
  } catch (error) {
    const detail = error && typeof error === "object" && "stderr" in error ? String(error.stderr) : String(error);
    console.error(`✗ ${testCase.label}: install-pos-edge.sh does not parse\n${detail}`);
    failures += 1;
  }
}

if (failures > 0) {
  console.error(
    `\n${failures} generated artifact(s) failed. These run as root on a store's only till; a parse error here is a shop that does not open.`,
  );
  exit(1);
}

if (emitFlag !== -1) {
  console.log(`wrote ${CASES.length} case(s) plus the checked-in template to ${outDir}`);
} else {
  console.log(`installer syntax: ${CASES.length} case(s) ok`);
}
