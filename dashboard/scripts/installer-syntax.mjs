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
//     and a TOML sanity pass over `config.toml`. Runs as part of `pnpm build`. The hand-written
//     appliance scripts in `deploy/appliance/` (ADR-0150) get `bash -n` and `shellcheck` here too;
//     see the end of this file.
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
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { argv, exit } from "node:process";
import { fileURLToPath } from "node:url";

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
 * The one thing a PowerShell script has to carry before a shop can run it: a byte-order mark.
 *
 * Windows PowerShell 5.1 — what a technician gets by typing `powershell` — reads a BOM-less `.ps1`
 * as ANSI in the machine's code page, not UTF-8. PowerShell 7 reads it as UTF-8 either way, and
 * that difference is why this shipped: the Windows job parses with `pwsh`, so it read the one
 * encoding under which the scripts were fine while a store read the other and got a parse error on
 * the first em dash. See `PS_BOM` in `src/installers.mjs` for the byte-level walk-through.
 *
 * Checked here rather than only on the Windows runner because this runs on every pull request, and
 * because the invariant is about the *bytes the generator emits* — which needs no PowerShell to
 * assert.
 *
 * @param {string} text
 * @param {string} where
 */
function checkPowerShellBom(text, where) {
  if (!text.startsWith("\ufeff")) {
    throw new Error(
      `${where}: no UTF-8 byte-order mark. Windows PowerShell 5.1 will read it as ANSI, and the ` +
        "first non-ASCII character — an em dash in the prose, or a Vietnamese store name in the " +
        "help block — becomes a smart quote its parser treats as a string delimiter.",
    );
  }
}

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
  checkPowerShellBom(windowsInstaller(values), `install-pos-edge.ps1 (${label})`);
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

// Whether `deploy/edge/` is reachable from here at all — and what its absence means, which is not
// the same answer for every caller.
//
// The cloud image's dashboard stage copies `dashboard/` AND NOTHING ELSE, then runs `pnpm build`.
// These two templates live two directories above that, so inside the image they do not exist. A
// bare `readFileSync` there dies on ENOENT and takes the whole image build with it — which is
// exactly what happened twice: the check was chained into `pnpm build`, unchained in #228 to
// unblock the image, then chained back in #269 to give the drift gate a home in the `dashboard`
// job. Both changes were right about their own problem and wrong about the other one. The fix is
// for the script to know where it is rather than for the build chain to keep swapping sides.
//
// So: absence is TOLERATED on the `pnpm build` path (the image, and any checkout without
// `deploy/`), and REFUSED under `--emit`. `--emit` exists solely to hand these files to the
// Windows job's PowerShell parser, so a run that cannot find them has failed at its only purpose —
// and that is what stops the skip from quietly becoming universal the day a path moves.
/**
 * PowerShell scripts under `deploy/` that nothing generates — written by hand, so there is no
 * generator to match, but they run elevated or in the release pipeline all the same. They get the
 * two checks that do not need a generator: the byte-order mark here, and both PowerShell parsers on
 * the Windows runner, which parses every `.ps1` this script writes under `--emit`.
 */
const HAND_WRITTEN = [
  {
    path: new URL("../../deploy/release/sign-windows.ps1", import.meta.url),
    name: "deploy/release/sign-windows.ps1",
    emit: "release-sign-windows.ps1",
  },
  {
    path: new URL("../../deploy/release/new-internal-signing-cert.ps1", import.meta.url),
    name: "deploy/release/new-internal-signing-cert.ps1",
    emit: "release-new-internal-signing-cert.ps1",
  },
  {
    path: new URL("../../deploy/edge/trust-internal-signing-cert.ps1", import.meta.url),
    name: "deploy/edge/trust-internal-signing-cert.ps1",
    emit: "edge-trust-internal-signing-cert.ps1",
  },
];

const missingTemplates = [...TEMPLATES, ...HAND_WRITTEN].filter(
  (template) => !existsSync(template.path),
);
if (missingTemplates.length > 0 && emitFlag !== -1) {
  console.error(
    `✗ --emit needs the checked-in templates and cannot find ${missingTemplates
      .map((template) => template.name)
      .join(", ")}.\n` +
      "  This run exists to hand them to the PowerShell parser, so there is nothing to emit.",
  );
  exit(1);
}
const templatesChecked = missingTemplates.length === 0;
if (!templatesChecked) {
  // Loud, and on stdout beside the ✓ lines rather than hidden in a warning stream: a reader
  // counting checks must see that this one did not run.
  console.log(
    `— skipping the installer-template checks: ${missingTemplates
      .map((template) => template.name)
      .join(", ")} not present.\n` +
      "  Expected inside the cloud image, whose dashboard stage copies only dashboard/.\n" +
      "  On a full checkout these run here; the Windows CI job parses them under --emit.",
  );
}
for (const template of templatesChecked ? TEMPLATES : []) {
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

  try {
    checkPowerShellBom(onDisk, template.name);
    console.log(`✓ ${template.name} carries its byte-order mark`);
  } catch (error) {
    console.error(`✗ ${error instanceof Error ? error.message : String(error)}`);
    failures += 1;
  }

  // Written before any failure can exit: a drift failure must not also rob the Windows job of the
  // files it most needs to parse. The first run of this gate did exactly that — the emission sat at
  // the end of the script, so the checked-in template was never parsed on the run that reported
  // drift.
  if (emitFlag !== -1) {
    writeFileSync(join(outDir, template.emit), onDisk, "utf8");
  }
}

for (const script of templatesChecked ? HAND_WRITTEN : []) {
  const onDisk = readFileSync(script.path, "utf8");
  try {
    checkPowerShellBom(onDisk, script.name);
    console.log(`✓ ${script.name} carries its byte-order mark`);
  } catch (error) {
    console.error(`✗ ${error instanceof Error ? error.message : String(error)}`);
    failures += 1;
  }
  if (emitFlag !== -1) {
    writeFileSync(join(outDir, script.emit), onDisk, "utf8");
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

// The Linux appliance (ADR-0150). Nothing generates these scripts, and they run as root on a store
// box: `provision.sh` from a technician's shell or a cloud-init first boot, and the kiosk launcher it
// installs on every boot after that. So each one is parsed by its own shell, and linted wherever the
// machine has `shellcheck`. A machine without it is told so on stdout, and not failed.
//
// "Each one" includes the scripts that live inside another file: the launcher is a heredoc in
// `provision.sh`, and the first-boot script is a block in `cloud-init.yaml`. A syntax error there is
// the same shop that does not open, one file further down.
//
// And two drift checks, because `provision.sh` carries two things it did not invent: a copy of
// `deploy/edge/pos-edge.service`, for when it runs outside a checkout, and the `config.toml` the
// console writes. Either one drifting is a second layout, where ADR-0150 has the appliance use the
// one the console's installer already lays out.
//
// Linux only, like `sh -n` above: `--emit` runs on the Windows runner, and exists to feed .ps1 files
// to PowerShell.

/** Where the appliance lives, relative to this script. Absent inside the cloud image. */
const APPLIANCE = new URL("../../deploy/appliance/", import.meta.url);

/**
 * The body of the quoted heredoc `<<'MARKER'` in `text`, including its final newline, or `null`.
 * Quoted, so the body is exactly what the script writes to disk.
 *
 * @param {string} text
 * @param {string} marker
 * @returns {string | null}
 */
function heredoc(text, marker) {
  const open = `<<'${marker}'\n`;
  const start = text.indexOf(open);
  if (start === -1) {
    return null;
  }
  const body = start + open.length;
  const end = text.indexOf(`\n${marker}\n`, body - 1);
  return end === -1 ? null : text.slice(body, end + 1);
}

/**
 * The scripts inside a file: every quoted heredoc whose body starts with `#!`, and every YAML
 * `content: |` block that does. Just enough YAML for cloud-init's `write_files`, because this script
 * runs on bare `node` with no dependencies.
 *
 * @param {string} text
 * @returns {Array<{ label: string, body: string }>}
 */
function embeddedScripts(text) {
  const scripts = [];
  for (const [, marker] of text.matchAll(/<<'([A-Z_]+)'\n/gu)) {
    const body = heredoc(text, marker);
    if (body?.startsWith("#!")) {
      scripts.push({ label: marker, body });
    }
  }
  const lines = text.split("\n");
  lines.forEach((line, at) => {
    const header = /^( *)content: \|$/u.exec(line);
    if (!header) {
      return;
    }
    const block = [];
    let indent = 0;
    for (const next of lines.slice(at + 1)) {
      const width = next.length - next.trimStart().length;
      if (next.trim() !== "" && width <= header[1].length) {
        break;
      }
      if (indent === 0 && next.trim() !== "") {
        indent = width;
      }
      block.push(next.slice(indent));
    }
    const body = `${block.join("\n").trimEnd()}\n`;
    if (body.startsWith("#!")) {
      scripts.push({ label: `content block on line ${at + 1}`, body });
    }
  });
  return scripts;
}

/** Whether `shellcheck` is on PATH. Asked once, and said once below if it is not. */
function haveShellcheck() {
  try {
    execFileSync("shellcheck", ["--version"], { stdio: "pipe" });
    return true;
  } catch {
    return false;
  }
}

/**
 * Parses the script at `path` with the shell its first line names (`sh` for `#!/bin/sh`, `bash`
 * otherwise) and, when `lint`, runs shellcheck over it. Returns the number of failures.
 *
 * @param {string} path
 * @param {string} name
 * @param {boolean} lint
 * @returns {number}
 */
function checkShell(path, name, lint) {
  const detail = (error) =>
    error && typeof error === "object" && "stdout" in error
      ? `${String(error.stdout)}${String(error.stderr)}`
      : String(error);
  const shell = readFileSync(path, "utf8").startsWith("#!/bin/sh\n") ? "sh" : "bash";
  let failed = 0;
  try {
    execFileSync(shell, ["-n", path], { stdio: "pipe" });
    console.log(`✓ ${name} parses (${shell} -n)`);
  } catch (error) {
    console.error(`✗ ${name} does not parse (${shell} -n)\n${detail(error)}`);
    failed += 1;
  }
  if (lint) {
    try {
      execFileSync("shellcheck", [path], { stdio: "pipe" });
      console.log(`✓ ${name} passes shellcheck`);
    } catch (error) {
      console.error(`✗ ${name} fails shellcheck\n${detail(error)}`);
      failed += 1;
    }
  }
  return failed;
}

let applianceChecked = 0;
if (emitFlag === -1 && !existsSync(APPLIANCE)) {
  console.log(
    "— skipping the appliance checks: deploy/appliance/ not present.\n" +
      "  Expected inside the cloud image, whose dashboard stage copies only dashboard/.",
  );
} else if (emitFlag === -1) {
  const lint = haveShellcheck();
  if (!lint) {
    console.log(
      "— shellcheck is not on PATH: the appliance scripts are parsed but not linted here.\n" +
        "  `apt-get install shellcheck` (or your platform's package) adds the lint.",
    );
  }
  const scratch = mkdtempSync(join(tmpdir(), "pos-appliance-"));
  for (const file of readdirSync(APPLIANCE).sort()) {
    const path = fileURLToPath(new URL(file, APPLIANCE));
    const name = `deploy/appliance/${file}`;
    if (file.endsWith(".sh")) {
      failures += checkShell(path, name, lint);
      applianceChecked += 1;
    }
    if (!file.endsWith(".sh") && !file.endsWith(".yaml")) {
      continue;
    }
    for (const { label, body } of embeddedScripts(lf(readFileSync(path, "utf8")))) {
      const extracted = join(scratch, `${file}.${applianceChecked}.sh`);
      writeFileSync(extracted, body, "utf8");
      failures += checkShell(extracted, `${name} (${label})`, lint);
      applianceChecked += 1;
    }
  }

  const provisionPath = new URL("provision.sh", APPLIANCE);
  const unitPath = new URL("../../deploy/edge/pos-edge.service", import.meta.url);
  if (existsSync(provisionPath)) {
    const provision = lf(readFileSync(provisionPath, "utf8"));

    if (heredoc(provision, "POS_EDGE_SERVICE") === lf(readFileSync(unitPath, "utf8"))) {
      console.log("✓ deploy/appliance/provision.sh carries deploy/edge/pos-edge.service unchanged");
    } else {
      console.error(
        "✗ the POS_EDGE_SERVICE heredoc in deploy/appliance/provision.sh no longer matches\n" +
          "  deploy/edge/pos-edge.service. That file is the source of truth: paste it into the heredoc.",
      );
      failures += 1;
    }

    // The store and cloud of the `plain` case, written the way provision.sh writes them: no name,
    // the default port, no store_path.
    const store = { ...PLAIN, storeName: "", tenantLabel: "", bindPort: "" };
    const rendered = heredoc(provision, "POS_EDGE_CONFIG")
      ?.replaceAll("@STORE_ID@", store.storeId)
      .replaceAll("@CLOUD_URL@", store.cloudUrl);
    if (rendered === configToml(store)) {
      console.log("✓ deploy/appliance/provision.sh writes the console's config.toml");
    } else {
      console.error(
        "✗ the POS_EDGE_CONFIG heredoc in deploy/appliance/provision.sh no longer matches\n" +
          "  configToml() in src/installers.mjs. Render configToml() with storeId \"@STORE_ID@\",\n" +
          "  cloudUrl \"@CLOUD_URL@\" and no name, port or store_path, and paste it into the heredoc.",
      );
      failures += 1;
    }
  }
}

if (failures > 0) {
  console.error(
    `\n${failures} artifact(s) failed. These run as root on a store's only till; a parse error here is a shop that does not open.`,
  );
  exit(1);
}

if (emitFlag !== -1) {
  console.log(`wrote ${CASES.length} case(s) plus the checked-in template to ${outDir}`);
} else {
  console.log(
    `installer syntax: ${CASES.length} case(s) ok` +
      (applianceChecked > 0 ? `, ${applianceChecked} appliance script(s) ok` : ""),
  );
}
