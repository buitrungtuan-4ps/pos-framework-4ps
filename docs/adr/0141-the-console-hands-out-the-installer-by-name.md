# ADR-0141 — The console hands out the installer by name

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-23
· Follows [ADR-0140](0140-a-store-pc-installs-itself-from-one-file.md) · Relates to
[ADR-0088](0088-ota-artifact-hosting.md), [ADR-0086](0086-edge-keyvault-and-activation.md)

## The problem

ADR-0140 taught `pos-edge.exe` to install itself when its file is named
`pos-edge-setup_<cloud host>[@<port>]_<store ULID>.exe`, and left two things undone. Nothing handed
that file out, so a technician renamed a release binary by hand — a ULID typed into a file name, the
step most likely to go wrong. And the file carries no store key, so a store installed that way sold
and activated but did not sync its configuration until somebody put a key on the box.

## Options considered

1. **Leave the rename to the technician.** The shortest route to a box that installs itself for the
   wrong store.
2. **Rename in the browser.** The console can set a download's name, but the rules for what the edge
   can read back would then live in TypeScript as well as Rust, and a `curl` or a provisioning
   script would have no URL to ask.
3. **A cloud route that serves the hosted release under the store's name**, and a setup window that
   asks for the key.

## Decision

**Option 3.**

- **`GET /admin/ota/releases/{release}/installer?store_id=<ULID>&cloud=<host[:port]>[&arch=]`**
  answers with the release's own Windows executable, byte for byte, and a `Content-Disposition` that
  names it `pos-edge-setup_<host>[@<port>]_<store>.exe`. The bytes are unchanged, so the minisign
  signature over them still verifies. The route needs only `console.data.read`, like the list beside
  it: it provisions nothing, and a box it installs stays unactivated until someone with
  `console.devices.manage` mints it a code.
- **The cloud's address comes from the caller**, not the `Host` header: behind a reverse proxy a
  handler can see the upstream's name, and a file naming `127.0.0.1` would install a store that dials
  itself. The console passes the address its browser is showing, which is what every generated
  installer already uses. Anything the edge could not read back — an underscore, an IPv6 literal, a
  port outside 1–65535 — is refused, naming `cloud`, rather than escaped.
- **Windows only.** `arch` defaults to `x86_64-pc-windows-msvc`; a Linux triple is refused, because a
  Linux store installs with the generated `install-pos-edge.sh`.
- **The console shows the file only when it will work.** The Stores handoff drawer and the new-store
  wizard offer it, starting from the version the store's rollout targets. The link appears once the
  cloud confirms it holds a Windows build of that version; when it does not, the console says to
  fetch the release on the OTA screen. If the console was reached over plain http at a network
  address, it declines to offer a file whose name tells the store to dial `https`.
- **The setup window asks for the key.** A new `-AskSyncKey` switch on the generated Windows script,
  passed by `pos-edge install`, prompts for the store key with `Read-Host -AsSecureString` when no
  `-SyncKey` was given. The key is pasted into the elevated window on the store PC and goes where
  `-SyncKey` has always put it: the service's own registry key. It never crosses the network and
  never lands in shell history. Enter skips it, which leaves the store as before.

## Consequences accepted

- **Two things still travel by hand**: the store key (pasted at the prompt) and the activation code
  (typed at `/setup`). Having activation deliver the key is the port and wire change ADR-0140 left for
  its own record; this does not pre-empt it.
- **The key is not in the keyring.** It sits where the script puts it today. ADR-0086's keyring is the
  better home, and moving the script there is its own change.
- **A new store starts with a blank version.** The console primes from the store's own rollout, and a
  store the wizard has just created has none. The operator types the version the fleet runs. A "latest
  hosted" default was rejected, because the newest hosted build can be a canary nobody has finished
  judging.
- **The download is buffered**, as the store-facing artifact route is: one file, for one person at a
  console.
