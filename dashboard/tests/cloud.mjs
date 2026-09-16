// One `pos_cloud` for the replay run: a throwaway database, a real first-boot enrolment, and the
// fixtures the declared flows need.
//
// The binary is the point of running against it rather than a stub. It serves the same
// `dashboard/dist` an operator loads ([ADR-0060](../../docs/adr/0060-cloud-dashboard.md)), answers
// on the same `/admin` routes, and enforces the same session and permission rules — a stub would be
// a second definition of the console's behaviour, and the one that drifts is the one nobody runs.
//
// # Why one cloud rather than one per test
//
// The till's harness starts an edge per test, because each test sells against it and two flows
// sharing a floor would interleave. The console's flows do not collide: five of the six only read,
// and the three that write (a store created by the wizard, a key issued by the handoff, a node
// published) each write something of their own. One boot, one migration run, one enrolment — and a
// Playwright config with a single worker, so "sequential" is a fact rather than a hope.
//
// # Why the credentials are minted here and not written down
//
// The setup token is this harness's own choice, written into the config it generates, so nothing is
// shared with a real installation. Everything after it comes from the server: `/admin/setup` returns
// the TOTP secret exactly once ([ADR-0045](../../docs/adr/0045-first-boot-admin-enrolment.md)), and
// the codes are computed from it the way an authenticator app would. That keeps the sign-in the
// browser performs a real one — password, second factor, session cookie — rather than a back door
// that would leave the login screen unexercised.
//
// # One sign-in per run, and why that is not laziness
//
// A TOTP code is spent when it is accepted: the server records the step it belongs to and refuses
// anything from that step or earlier, which is what stops a code read over a shoulder from being
// replayed ([ADR-0034](../../docs/adr/0034-super-admin-auth.md)). So a harness that signed in twice
// inside thirty seconds would be refused the second time — correctly. This one signs in **once**,
// in the browser, through the login screen; the spec hands the session back here with
// [`useSession`] so the fixtures are written as the same admin, and every test reuses the browser
// state. One code, spent the way an operator spends it.

import { spawn, spawnSync } from "node:child_process";
import { createHmac, randomBytes } from "node:crypto";
import { existsSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";

/** The cloud binary. Built by CI before this runs; `POS_CLOUD_BIN` overrides it. */
const BINARY =
  process.env["POS_CLOUD_BIN"] ??
  fileURLToPath(new URL("../../target/debug/pos-cloud", import.meta.url));

/**
 * Where Postgres is, in libpq form, **without** a database name — the harness appends its own.
 *
 * No default that points at a developer's machine: a harness that silently found *a* database would
 * eventually run its migrations against one somebody cared about.
 */
const POSTGRES = process.env["POS_E2E_POSTGRES"];

/** How long to wait for the cloud to answer `/healthz` before giving up. */
const BOOT_TIMEOUT_MS = 60_000;

/** The password the enrolled owner gets. Long enough for ADR-0045's minimum, and thrown away. */
const PASSWORD = "replay-harness-password";

/** A port nothing is listening on, by binding one and letting go. */
async function freePort() {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.on("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });
}

/** Runs one statement with `psql`, or throws saying what could not be done. */
function psql(connection, statement) {
  const result = spawnSync("psql", [connection, "-v", "ON_ERROR_STOP=1", "-c", statement], {
    encoding: "utf8",
  });
  if (result.error !== undefined && result.error.code === "ENOENT") {
    throw new Error("`psql` is not on PATH, and the harness needs it to make its own database");
  }
  if (result.status !== 0) {
    throw new Error(`psql failed on \`${statement}\`: ${result.stderr.trim()}`);
  }
}

// --- TOTP, as an authenticator app computes it (RFC 6238, SHA-1, six digits, thirty seconds) -----

const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

function base32Decode(text) {
  let bits = 0;
  let value = 0;
  const bytes = [];
  for (const character of text.replace(/=+$/u, "").toUpperCase()) {
    const index = BASE32.indexOf(character);
    if (index < 0) {
      continue;
    }
    value = (value << 5) | index;
    bits += 5;
    if (bits >= 8) {
      bytes.push((value >>> (bits - 8)) & 0xff);
      bits -= 8;
    }
  }
  return Buffer.from(bytes);
}

/** The current six-digit code for `secret`, base32 as `/admin/setup` returned it. */
export function totp(secret, atMs = Date.now()) {
  const counter = Math.floor(atMs / 1000 / 30);
  const message = Buffer.alloc(8);
  message.writeUInt32BE(Math.floor(counter / 2 ** 32), 0);
  message.writeUInt32BE(counter >>> 0, 4);
  const digest = createHmac("sha1", base32Decode(secret)).update(message).digest();
  const offset = digest[digest.length - 1] & 0x0f;
  const binary =
    ((digest[offset] & 0x7f) << 24) |
    (digest[offset + 1] << 16) |
    (digest[offset + 2] << 8) |
    digest[offset + 3];
  return String(binary % 1_000_000).padStart(6, "0");
}

let running = null;

/**
 * Starts a cloud, enrols its first admin, and signs in.
 *
 * Idempotent for the run: the second caller gets the first one's cloud, because the config keeps a
 * single worker and every flow can share it.
 */
export async function startCloud() {
  if (running !== null) {
    return running;
  }
  if (!existsSync(BINARY)) {
    throw new Error(
      `${BINARY} is not built — run \`cargo build -p pos-cloud\` first (it embeds dashboard/dist, so build the console before it), or set POS_CLOUD_BIN`,
    );
  }
  if (POSTGRES === undefined) {
    throw new Error(
      "POS_E2E_POSTGRES is unset. Point it at a Postgres the harness may create a database on, e.g. `postgres://pos:pos@127.0.0.1:5432` — it makes its own database, migrates it, and drops it afterwards",
    );
  }

  // A database of this run's own, so a second run never inherits the first one's enrolled admin —
  // `/admin/setup` is once per installation, and its TOTP secret is returned once.
  const database = `pos_replay_${randomBytes(6).toString("hex")}`;
  psql(`${POSTGRES}/postgres`, `CREATE DATABASE ${database}`);

  const port = await freePort();
  const baseURL = `http://127.0.0.1:${port}`;
  const directory = mkdtempSync(join(tmpdir(), "pos-replay-"));
  const setupToken = randomBytes(16).toString("hex");
  const config = join(directory, "cloud.toml");
  const url = new URL(`${POSTGRES}/${database}`);
  writeFileSync(
    config,
    [
      `bind = "127.0.0.1:${port}"`,
      // libpq form, and `password=` is omitted when there is none: a trust-authenticated local
      // cluster has no password, and an empty one in the string is not the same as its absence.
      `database_url = "host=${url.hostname} port=${url.port || "5432"} user=${url.username}${
        url.password === "" ? "" : ` password=${url.password}`
      } dbname=${database}"`,
      `admin_setup_token = "${setupToken}"`,
      // Both are required and neither is used by anything this harness drives; they are minted
      // rather than fixed so a copied config cannot become a real one (ADR-0097).
      `internal_shared_secret = "${randomBytes(32).toString("hex")}"`,
      `table_token_secret = "${randomBytes(32).toString("hex")}"`,
      "",
    ].join("\n"),
  );

  const child = spawn(BINARY, {
    env: { ...process.env, POS_CLOUD_CONFIG: config, RUST_LOG: "warn", NO_COLOR: "1" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let output = "";
  const collect = (chunk) => {
    output += chunk.toString();
  };
  child.stdout.on("data", collect);
  child.stderr.on("data", collect);

  const stop = async () => {
    running = null;
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      await new Promise((resolve) => child.once("exit", resolve));
    }
    // The database goes with it. Dropped rather than left named after a run nobody can trace.
    psql(`${POSTGRES}/postgres`, `DROP DATABASE IF EXISTS ${database} WITH (FORCE)`);
  };

  const started = Date.now();
  for (;;) {
    if (child.exitCode !== null) {
      await stop();
      throw new Error(`pos-cloud exited with ${child.exitCode} before it came up:\n${output}`);
    }
    if (Date.now() - started > BOOT_TIMEOUT_MS) {
      await stop();
      throw new Error(`pos-cloud did not answer /healthz within ${BOOT_TIMEOUT_MS}ms:\n${output}`);
    }
    try {
      const health = await fetch(`${baseURL}/healthz`);
      if (health.ok) {
        break;
      }
    } catch {
      // Not listening yet — the migrations run before the socket opens.
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }

  // First boot, exactly as an operator's does: the token authorises one enrolment and the response
  // carries the TOTP secret once.
  const enrolled = await fetch(`${baseURL}/admin/setup`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ setup_token: setupToken, password: PASSWORD }),
  });
  if (!enrolled.ok) {
    await stop();
    throw new Error(`/admin/setup refused the enrolment: ${enrolled.status} ${await enrolled.text()}`);
  }
  const { secret_base32: secret } = await enrolled.json();

  // No sign-in here: the browser performs the run's one sign-in and hands the session back.
  let cookie = null;

  /** One `/admin` call as the enrolled owner. Returns the parsed body, or throws with the refusal. */
  const call = async (method, path, body, etag) => {
    if (cookie === null) {
      throw new Error(
        "the harness has no session yet — the browser signs in first and passes the cookie to `useSession`",
      );
    }
    const response = await fetch(`${baseURL}${path}`, {
      method,
      headers: {
        cookie,
        // A conditional write says which version it is replacing (ADR-0094). The fixtures pass the
        // version they were just handed, which is what an operator's screen does.
        // Quoted, because a conditional write asserts a *strong* entity-tag and the route refuses
        // a bare one (ADR-0094) — the same shape `api/client.ts` sends.
        ...(etag === undefined ? {} : { "if-match": `"${etag}"` }),
        ...(body === undefined ? {} : { "content-type": "application/json" }),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    if (!response.ok) {
      throw new Error(`${method} ${path} → ${response.status} ${await response.text()}`);
    }
    return response.status === 204 ? null : response.json();
  };

  /** Adopts the session the browser just obtained, so fixtures are written as that same admin. */
  const useSession = (session) => {
    cookie = session;
  };

  running = { baseURL, password: PASSWORD, secret, call, useSession, stop, log: () => output };
  return running;
}

/** Stops the run's cloud and drops its database. Safe to call when nothing is running. */
export async function stopCloud() {
  await running?.stop();
}

/**
 * The shop, the catalogue and the cohort the declared flows act on.
 *
 * Written over `/admin` with the owner's session rather than into the database, so a fixture that
 * the API would refuse cannot exist here either — the harness sets up what an operator could.
 */
export async function seedFixtures(cloud) {
  const tenant = await cloud.call("POST", "/admin/tenants", { name: "Replay Tenant" });
  const store = await cloud.call("POST", "/admin/stores", {
    tenant_id: tenant.tenant_id,
    name: "Bến Thành",
  });
  const taxClass = await cloud.call("POST", "/admin/catalog/tax-classes", {
    tenant_id: tenant.tenant_id,
    name: "Standard",
  });
  const item = await cloud.call("POST", "/admin/catalog/items", {
    tenant_id: tenant.tenant_id,
    name: "Margherita",
    name_translations: {},
    tax_class_id: taxClass.tax_class_id,
    item_category_id: null,
    item_subcategory_id: null,
    image_ref: null,
  });
  const menu = await cloud.call("POST", "/admin/catalog/menus", {
    tenant_id: tenant.tenant_id,
    name: "All day",
    parent_menu_id: null,
  });
  // A placement, so the price flow has a row to open rather than an empty table: the flow it
  // measures is *changing* a price, and creating one first would be a different flow with a
  // different count.
  await cloud.call("POST", `/admin/catalog/menus/${menu.menu_id}/placements`, {
    tenant_id: tenant.tenant_id,
    menu_id: menu.menu_id,
    menu_item_id: item.menu_item_id,
    menu_section_id: null,
    // One channel priced, in the minor units the console authors in (VND has no minor part).
    prices: [
      {
        sales_channel: "SALES_CHANNEL_DINE_IN",
        unit_price: { currency_code: "VND", amount_minor: 120_000 },
      },
    ],
    available: true,
  });
  // The node order the server enforces, in the order it enforces it: a `menu` publish is refused
  // until `tax` exists, and `tax` until `locale` does — each refusal naming the one to do first.
  // This is a shop being set up the way a real one is, not a fixture convenience: the price flow
  // under test ends in a publish, and a publish to a shop with no locale is what the console
  // correctly refuses.
  await cloud.call("PUT", "/admin/config/locale", {
    tenant_id: tenant.tenant_id,
    store_id: store.store_id,
    country_code: "VN",
    currency_code: "VND",
    timezone: "Asia/Ho_Chi_Minh",
    cutoff_hour: 4,
    display_language: "vi",
    prices_include_tax: true,
    cash_rounding_increment: null,
    cash_denominations: [],
  });
  await cloud.call("PUT", "/admin/config/tax", {
    tenant_id: tenant.tenant_id,
    store_id: store.store_id,
  });
  const group = await cloud.call("POST", "/admin/store-groups", {
    tenant_id: tenant.tenant_id,
    name: "Airport branches",
  });
  await cloud.call(
    "PUT",
    `/admin/store-groups/${group.group_id}/members`,
    { tenant_id: tenant.tenant_id, store_ids: [store.store_id] },
    group.etag,
  );
  return { tenant, store, item, menu, group };
}
