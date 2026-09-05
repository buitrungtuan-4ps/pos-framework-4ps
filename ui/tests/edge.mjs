// One `examples/minimal-edge` per test, started and stopped by the harness.
//
// The example is the whole point of running against it rather than a stub: it is the real edge on
// the in-memory fakes, serving the same `ui/dist` a till gets
// ([ADR-0018](../../docs/adr/0018-http-websocket-stack.md),
// [ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md) §4). A stub would be a
// second definition of the edge's behaviour, and the one that drifts is the one nobody runs.
//
// # Why the credentials are read from the log and not written here
//
// The pairing code is minted per boot; the demo badge code and PIN are constants in
// `crates/pos-edge/src/demo.rs` (ADR-0109 Amendment 1). Both are printed at start-up, and the
// harness parses them from the process output rather than repeating them. Repeating them would make
// this file a second declaration of the same thing, which is exactly the drift the whole gate
// exists to prevent — and reading them keeps the pairing path as a real device experiences it,
// rather than adding a back door that would stop the boot gate being proven.

import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

/** The example binary. Built by CI before this runs; `POS_EDGE_BIN` overrides it. */
const BINARY =
  process.env["POS_EDGE_BIN"] ?? fileURLToPath(new URL("../../target/debug/minimal-edge", import.meta.url));

/** How long to wait for the edge to print its pairing URL before giving up. */
const BOOT_TIMEOUT_MS = 30_000;

/** Terminal colour codes, which are not part of what the edge said. */
const ANSI = /\u001B\[[0-9;]*m/g;

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

/**
 * Starts an edge and waits until it has printed its pairing URL.
 *
 * Resolves to `{ baseURL, pairingCode, staffCode, staffPin, stop() }`. Every field but `baseURL`
 * comes out of the process's own output, so nothing here can disagree with the binary.
 */
export async function startEdge() {
  if (!existsSync(BINARY)) {
    throw new Error(
      `${BINARY} is not built — run \`cargo build -p minimal-edge\` first (it embeds ui/dist, so build the UI before it), or set POS_EDGE_BIN`,
    );
  }
  const port = await freePort();
  const baseURL = `http://127.0.0.1:${port}`;
  const child = spawn(BINARY, {
    env: {
      ...process.env,
      POS_EDGE_BIND: `127.0.0.1:${port}`,
      RUST_LOG: "info",
      // The edge's log writer colours its field names, and a colour code between `pairing_url` and
      // its `=` is enough to make the pattern below miss the one line this harness exists to read.
      // Asked for plainly here, and stripped below anyway.
      NO_COLOR: "1",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  let output = "";
  const collect = (chunk) => {
    output += chunk.toString().replaceAll(ANSI, "");
  };
  child.stdout.on("data", collect);
  child.stderr.on("data", collect);

  const stop = async () => {
    if (child.exitCode !== null || child.signalCode !== null) {
      return;
    }
    child.kill("SIGTERM");
    await new Promise((resolve) => child.once("exit", resolve));
  };

  const started = Date.now();
  for (;;) {
    const pairing = /pairing_url=\S*code=(\d{6})/.exec(output);
    const staff = /sign in with code (\S+) and PIN (\S+)/.exec(output);
    if (pairing !== null && staff !== null) {
      return { baseURL, pairingCode: pairing[1], staffCode: staff[1], staffPin: staff[2], stop };
    }
    if (child.exitCode !== null) {
      await stop();
      throw new Error(`minimal-edge exited with ${child.exitCode} before it came up:\n${output}`);
    }
    if (Date.now() - started > BOOT_TIMEOUT_MS) {
      await stop();
      throw new Error(`minimal-edge did not print a pairing URL within ${BOOT_TIMEOUT_MS}ms:\n${output}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}
