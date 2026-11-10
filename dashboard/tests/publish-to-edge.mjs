// A package version published through the console is running on a store within about a second
// (roadmap-v4 phase 1; ADR-0189, ADR-0195 decision 6).
//
// Every piece is the real one: the `pos_cloud` binary over its own PostgreSQL database, a NATS
// server, and the `pos-edge` binary dialling the cloud over https with a store's own `read_config`
// key, exactly as a provisioned box does. The console's calls are the `/admin` routes its screens
// make, signed in with a password and a second factor.
//
// What is timed is the whole chain: from the moment the console sends the settings write that pins
// a version, through the publish, the wake on `pos.store.<store_id>.config`, the store's pull, to
// the edge saying it applied the configuration that wake announced. The edge says so only once the
// document, package included, has been applied as a whole (ADR-0189 decision 6), so that line is
// "running", not "received".
//
// Run from `dashboard/` after building both binaries (`cargo build -p pos-cloud -p pos-edge`, the
// edge stamped with a release that runs packages):
//
//   POS_E2E_POSTGRES=postgres://pos:pos@127.0.0.1:5432 NATS_URL=127.0.0.1:4222 \
//     node tests/publish-to-edge.mjs
//
// It exits non-zero, saying why, if any publish took longer than the budget or reached the store
// by any path but the wake.

import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { connect, createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { startCloud, totp } from "./cloud.mjs";

/** The store binary. `POS_EDGE_BIN` overrides it. */
const EDGE_BINARY =
  process.env["POS_EDGE_BIN"] ??
  fileURLToPath(new URL("../../target/debug/pos-edge", import.meta.url));

/** The broker both ends use, as `link-nats` takes it. */
const NATS = process.env["NATS_URL"];

/**
 * The budget: ADR-0195 decision 6's one second, from the console's write to the store running it.
 * The network round trip it adds is the loopback's here, so nothing is added for it.
 */
const BUDGET_MS = 1_000;

/** How many times a version is published and timed: switches between two versions, so half are rollbacks. */
const ROUNDS = 6;

/**
 * The gap between two publishes. A store pulls at most once a second for wakes (ADR-0195 decision
 * 2), so publishes closer than that would time the spacing, not the delivery.
 */
const GAP_MS = 1_500;

/** How long to wait for something the store does once, such as its first pull, before giving up. */
const SETTLE_MS = 60_000;

/** Terminal colour codes, which are not part of what the edge said. */
const ANSI = /\u001B\[[0-9;]*m/g;

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

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Polls `check` until it returns something other than `undefined`, or throws after `within`. */
async function until(what, check, within = SETTLE_MS) {
  const started = Date.now();
  for (;;) {
    const found = await check();
    if (found !== undefined) {
      return found;
    }
    if (Date.now() - started > within) {
      throw new Error(`gave up waiting for ${what} after ${within}ms`);
    }
    await sleep(100);
  }
}

/**
 * Creates the fleet stream on `NATS`, as the first store to connect would.
 *
 * The cloud's ingest cursor binds to a stream that exists and refuses to start otherwise, and in a
 * fleet the stores create it (ADR-0087 Amendment 1). This run starts the cloud before its one
 * store, so it creates the stream the store would, with the store's subject, over NATS's own
 * request protocol; an existing stream of that name is taken as it is.
 */
async function ensureFleetStream() {
  const [host, port] = NATS.replace(/^nats:\/\//u, "").split(":");
  const inbox = `_INBOX.e2e_${randomBytes(4).toString("hex")}`;
  const request = JSON.stringify({
    name: "POS_FLEET",
    subjects: ["pos.fleet.events"],
    retention: "limits",
    storage: "file",
    discard: "new",
    max_msgs: -1,
    max_bytes: -1,
    num_replicas: 1,
  });
  const answer = await new Promise((resolve, reject) => {
    const socket = connect(Number(port ?? "4222"), host);
    let buffered = "";
    let sent = false;
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error(`NATS at ${NATS} did not answer the stream request`));
    }, 10_000);
    socket.on("error", reject);
    socket.on("data", (chunk) => {
      buffered += chunk.toString();
      if (!sent && buffered.includes("INFO ")) {
        sent = true;
        socket.write('CONNECT {"verbose":false,"pedantic":false,"headers":false}\r\n');
        socket.write(`SUB ${inbox} 1\r\n`);
        socket.write(`PUB $JS.API.STREAM.CREATE.POS_FLEET ${inbox} ${Buffer.byteLength(request)}\r\n${request}\r\n`);
      }
      const message = new RegExp(`MSG ${inbox.replaceAll(".", "\\.")} 1 (\\d+)\r\n`, "u").exec(buffered);
      if (message !== null) {
        const start = message.index + message[0].length;
        const body = buffered.slice(start, start + Number(message[1]));
        if (body.length === Number(message[1])) {
          clearTimeout(timer);
          socket.end();
          resolve(JSON.parse(body));
        }
      }
      if (buffered.includes("-ERR")) {
        clearTimeout(timer);
        socket.destroy();
        reject(new Error(`NATS refused the stream request: ${buffered}`));
      }
    });
  });
  // 10058: a stream of that name exists, which is what this asks for.
  if (answer.error !== undefined && answer.error.err_code !== 10058) {
    throw new Error(`NATS would not create the fleet stream: ${JSON.stringify(answer.error)}`);
  }
}

/** Signs in as the enrolled owner the way the login screen does, and returns the session cookie. */
async function signIn(cloud) {
  const response = await fetch(`${cloud.baseURL}/admin/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ password: cloud.password, totp_code: totp(cloud.secret) }),
  });
  if (response.status !== 204) {
    throw new Error(`/admin/login refused the owner: ${response.status} ${await response.text()}`);
  }
  const cookie = response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ");
  if (cookie === "") {
    throw new Error("/admin/login set no session cookie");
  }
  return cookie;
}

/**
 * A store box for `store` against `cloud`: its `config.toml`, a sealed vault of its own, and the
 * means to start `pos-edge` on them. Every line the edge says is kept with the moment it was said,
 * so a line can be timed against a write the harness made.
 *
 * The vault is sealed under a key made for the run (ADR-0151), so the credential activation stores
 * survives the restart activation asks for, as it does on a box systemd restarts.
 */
async function storeBox(cloud, store) {
  if (!existsSync(EDGE_BINARY)) {
    throw new Error(`${EDGE_BINARY} is not built — run \`cargo build -p pos-edge\`, or set POS_EDGE_BIN`);
  }
  const directory = mkdtempSync(join(tmpdir(), "pos-e2e-edge-"));
  const port = await freePort();
  const config = join(directory, "config.toml");
  const vaultKey = join(directory, "vault-key");
  const vaultDir = join(directory, "vault");
  mkdirSync(vaultDir);
  writeFileSync(vaultKey, randomBytes(32), { mode: 0o600 });
  writeFileSync(
    config,
    [
      `store_id = "${store.store_id}"`,
      `cloud_url = "${cloud.consoleURL}"`,
      `cloud_ca_file = "${cloud.caFile}"`,
      `bind = "127.0.0.1:${port}"`,
      `store_path = "${join(directory, "store.sqlite")}"`,
      "",
      "[nats]",
      'stream = "POS_FLEET"',
      'subject = "pos.fleet.events"',
      "",
    ].join("\n"),
  );
  const lines = [];
  let child = null;
  const start = () => {
    child = spawn(EDGE_BINARY, {
      env: {
        ...process.env,
        POS_EDGE_CONFIG: config,
        POS_EDGE_VAULT_KEY: vaultKey,
        POS_EDGE_VAULT_DIR: vaultDir,
        POS_EDGE_NATS_URL: NATS,
        RUST_LOG: "info",
        NO_COLOR: "1",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let partial = "";
    const collect = (chunk) => {
      const at = performance.now();
      partial += chunk.toString().replaceAll(ANSI, "");
      const parts = partial.split("\n");
      partial = parts.pop() ?? "";
      for (const text of parts) {
        lines.push({ at, text });
      }
    };
    child.stdout.on("data", collect);
    child.stderr.on("data", collect);
  };
  const stop = async () => {
    if (child !== null && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      await new Promise((resolve) => child.once("exit", resolve));
    }
  };
  return {
    baseURL: `http://127.0.0.1:${port}`,
    lines,
    start,
    stop,
    exited: () => child?.exitCode ?? null,
    said: (text) => lines.some((line) => line.text.includes(text)),
    log: () => lines.map((line) => line.text).join("\n"),
  };
}

/** The line in which the edge says it applied `configVersionId` because a wake announced it. */
function wokenApply(edge, configVersionId) {
  return edge.lines.find(
    (line) =>
      line.text.includes("applied the configuration a wake announced") &&
      line.text.includes(`config_version_id=${configVersionId}`),
  );
}

/** Nearest-rank percentile of `values`, as the till's perf gate computes them. */
function percentile(values, p) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil((p / 100) * sorted.length) - 1)];
}

async function main() {
  if (NATS === undefined) {
    throw new Error("NATS_URL is unset. Point it at a NATS server with JetStream, e.g. `127.0.0.1:4222`");
  }
  await ensureFleetStream();
  const cloud = await startCloud({
    extraConfig: [
      "[nats]",
      `url = "${NATS}"`,
      'stream = "POS_FLEET"',
      `durable = "e2e_${randomBytes(4).toString("hex")}"`,
      'filter_subject = "pos.fleet.events"',
    ],
  });
  let edge = null;
  try {
    cloud.useSession(await signIn(cloud));
    const tenant = await cloud.call("POST", "/admin/tenants", { name: "Publish Timing Tenant" });
    const store = await cloud.call("POST", "/admin/stores", {
      tenant_id: tenant.tenant_id,
      name: "Timing Store",
    });
    const pkg = await cloud.call("POST", "/admin/packages", {
      tenant_id: tenant.tenant_id,
      name: "Dine-in",
    });
    const versions = [];
    for (const version_label of ["March", "April"]) {
      const record = await cloud.call("POST", `/admin/packages/${pkg.package_id}/versions`, {
        tenant_id: tenant.tenant_id,
        version_label,
      });
      versions.push(record.content.package_version_id);
    }

    const setting = (setting_key, value) =>
      cloud.call("PUT", "/admin/settings", {
        tenant_id: tenant.tenant_id,
        setting_key,
        scope: "SETTING_SCOPE_TENANT",
        scope_id: tenant.tenant_id,
        value,
      });
    const standing = () =>
      cloud.call("GET", `/admin/stores/${store.store_id}/package?tenant_id=${tenant.tenant_id}`);
    const published = async () =>
      (await cloud.call("GET", `/admin/fleet/${store.store_id}?tenant_id=${tenant.tenant_id}`))
        .config_version_published ?? null;

    // The box is activated the way a store's is: a device slot, its code from the setup sheet,
    // typed at the box's own `/setup`. The box restarts itself so its cloud loops start with the
    // credential; a service manager starts it again, which here is the harness.
    edge = await storeBox(cloud, store);
    edge.start();
    await until("the edge to come up", () =>
      edge.exited() !== null
        ? Promise.reject(new Error(`pos-edge exited with ${edge.exited()}:\n${edge.log()}`))
        : edge.said("pos_edge listening")
          ? true
          : undefined,
    );
    const slot = await cloud.call("POST", `/admin/stores/${store.store_id}/devices`, {
      tenant_id: tenant.tenant_id,
      name: "Back office box",
      kind: "pos",
    });
    const code = await cloud.call("POST", "/admin/activation-codes", {
      tenant_id: tenant.tenant_id,
      store_id: store.store_id,
      device_id: slot.device_id ?? slot.value?.device_id,
    });
    const activated = await fetch(`${edge.baseURL}/api/activate`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code: code.activation_code }),
    });
    if (!activated.ok) {
      throw new Error(`the box refused its activation code: ${activated.status} ${await activated.text()}`);
    }
    await until("the edge to stop for its restart", () => (edge.exited() !== null ? true : undefined));
    edge.start();
    // The store's boot report tells the cloud its release, which decides whether a version may
    // reach it (ADR-0189 decision 5), and its wake rides the broker connection it opens. The
    // untimed first publish below is delivered by the poll if the subscription is not up yet, so
    // nothing here waits on a guess; every timed one after it must arrive by the wake.
    await until("the edge to connect to the broker with its wake on", () =>
      edge.exited() !== null
        ? Promise.reject(new Error(`pos-edge exited with ${edge.exited()}:\n${edge.log()}`))
        : edge.said("config wake enabled over NATS") && edge.said("event: connected")
          ? true
          : undefined,
    );

    // Lock the store to the first version, untimed, and wait until it runs it.
    await setting("package.locked_version_id", versions[0]);
    await setting("package.mode", "PACKAGE_MODE_LOCKED");
    await until("the cloud to compile the first version into the store", async () => {
      const now = await standing();
      return now.running_version_id === versions[0] ? true : undefined;
    });
    const first = await published();
    await until("the store to apply the first version", () =>
      edge.lines.some((line) => line.text.includes(`version=${first}`) || line.text.includes(`config_version_id=${first}`))
        ? true
        : undefined,
    );
    await sleep(GAP_MS);

    const rounds = [];
    for (let round = 0; round < ROUNDS; round += 1) {
      const target = versions[(round + 1) % versions.length];
      const sent = performance.now();
      await setting("package.locked_version_id", target);
      const now = await standing();
      if (now.running_version_id !== target) {
        throw new Error(`the publish did not compile ${target} into the store: ${JSON.stringify(now)}`);
      }
      const configVersionId = await published();
      const applied = await until(
        `the store to apply ${configVersionId} on its wake`,
        () => wokenApply(edge, configVersionId),
        BUDGET_MS * 10,
      );
      rounds.push({ target, configVersionId, ms: Math.round(applied.at - sent) });
      await sleep(GAP_MS);
    }

    const refused = edge.lines.filter((line) => line.text.includes("refused a store config"));
    const times = rounds.map((round) => round.ms);
    const report = {
      rounds,
      p50_ms: percentile(times, 50),
      p95_ms: percentile(times, 95),
      max_ms: Math.max(...times),
      budget_ms: BUDGET_MS,
    };
    console.log(JSON.stringify(report, null, 2));
    if (refused.length > 0) {
      throw new Error(`the store refused a configuration:\n${refused.map((line) => line.text).join("\n")}`);
    }
    if (report.p95_ms > BUDGET_MS) {
      throw new Error(`publish to running took ${report.p95_ms}ms at p95, over the ${BUDGET_MS}ms budget`);
    }
  } catch (error) {
    if (edge !== null) {
      console.error(`--- pos-edge said ---\n${edge.log()}`);
    }
    console.error(`--- pos-cloud said ---\n${cloud.log()}`);
    throw error;
  } finally {
    await edge?.stop();
    await cloud.stop();
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
