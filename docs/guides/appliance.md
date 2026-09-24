# Build a Linux store appliance

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-24

An appliance is a small PC running Debian 12 or Ubuntu 24.04 that is the store server and, if it has
a screen, one of the store's tills. One script turns a stock install into one. Run the same script
inside an image build and the image is generic: every box made from it claims its store from the
console on first boot, and a stolen copy claims nothing.

This is [ADR-0150](../adr/0150-the-appliance-is-a-linux-image-that-claims-itself.md) in practice,
with the claim flow of [ADR-0148](../adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md).
It is a script and a cloud-init file, not a published image. Building and patching an OS image is a
fork's job, and [Building an image](#building-an-image) shows where the script fits in one.

## What you end up with

[`deploy/appliance/provision.sh`](../../deploy/appliance/provision.sh) lays out exactly what the
console's `install-pos-edge.sh` lays out, so everything in
[`deploy/edge/README.md`](../../deploy/edge/README.md) (the log, updates, printers) applies to an
appliance unchanged.

| What | Where | Notes |
|---|---|---|
| The service account | `pos` | No shell and no home. The unit runs the store as this user |
| The binary | `/var/lib/pos-edge/bin/slot-a`, with `bin/current` pointing at it | The update slots the edge manages itself ([ADR-0055](../adr/0055-edge-ota-updater.md) Amendment 1) |
| The rescue copy | `/usr/local/bin/pos-edge` | For `--self-test` and break-glass. Not what runs |
| The unit | `/etc/systemd/system/pos-edge.service` | [`deploy/edge/pos-edge.service`](../../deploy/edge/pos-edge.service), unchanged |
| The store's identity | `/var/lib/pos-edge/config.toml` | Written with `--store`, or by the claim |
| The secrets file | `/etc/pos-edge/env` | Root-owned, mode 0600, every line commented out until you fill one in |
| Fonts | `fonts-dejavu-core` | Without them receipts print ASCII only |
| The first-boot claim | `pos-edge-claim.service` | Only without `--store` |
| The kiosk | the `pos-kiosk` account, `pos-kiosk.service`, `/etc/pam.d/pos-kiosk`, `/usr/local/libexec/pos-kiosk` | Only with `--kiosk`. Pulls in `cage`, `curl`, `dbus`, `libpam-systemd` and Chromium |

The dashboard build holds the script to that layout. Its copy of the unit must match
`deploy/edge/pos-edge.service`, and the `config.toml` it writes must match the console's, or
`pnpm build` fails.

## Before you start

- **Debian 12 or Ubuntu 24.04, installed without a desktop**: a server or minimal install. The
  script refuses any other distribution, because its package names and the kiosk's session are only
  known to hold on those two. A display manager would fight the kiosk for the screen.
- **The signed release binary for the machine's architecture**, `pos-edge-<tag>-x86_64-unknown-linux-gnu.bin`
  or `pos-edge-<tag>-aarch64-unknown-linux-gnu.bin` ([release runbook](../release-runbook.md)).
  Check its signature before it goes anywhere near root:

  ```
  minisign -Vm pos-edge-v1.2.0-x86_64-unknown-linux-gnu.bin -P "$(sed -n 2p minisign.pub)"
  ```

  `-P` takes the key itself, which is the second line of `minisign.pub`. Given the whole file it
  fails with `base64 conversion failed`; `-p minisign.pub` is the other form that works.
- **Root, and a network** that reaches the cloud and the distribution's package archive.
- **For a box that claims itself**, a `pos-edge` release that has the `claim` command
  (`pos-edge claim --cloud <url>`, ADR-0148). It exits 0 only once it has written `config.toml`, which
  `pos-edge-claim.service` checks. With an older release, provision with `--store`.
- **For the kiosk**, a screen, and a USB keyboard for the two moments a till needs typing: pairing,
  and activation on the `--store` path. The kiosk has no on-screen keyboard.

## See it first: `--dry-run`

```
./provision.sh --dry-run --binary ./pos-edge-v1.2.0-x86_64-unknown-linux-gnu.bin \
  --cloud https://cloud.example.com --kiosk
```

This prints every command and every file a real run would write, contents included, and changes
nothing. It needs no root. It does check the arguments, the distribution and the binary the way a
real run does, so a dry run that ends cleanly means a real run gets as far.

## Three ways to provision

### 1. With a store id

For a box whose store you know now: the store exists in the console, and you have its ULID from the
Stores screen.

```
sudo ./provision.sh --binary ./pos-edge-v1.2.0-x86_64-unknown-linux-gnu.bin \
  --cloud https://cloud.example.com --store 01JBQ9ZK7X8N4M2P6R3T5V7W9Y
```

The script writes `config.toml`, enables and starts the edge, and prints its status. From here the box
is exactly a box the console's installer set up, and [Bring a store online](bring-a-store-online.md)
takes over at Step 3: **activate** it at `http://<box>:8787/setup` with a code from the console, then
**pair** the tills.

Nothing asks for the store key. The script never takes a secret on its command line, where any
process on the box could read it. If the store needs `POS_EDGE_SYNC_KEY` or `POS_EDGE_NATS_URL`, put
them in `/etc/pos-edge/env`; its commented lines say what goes there.

### 2. Without one: the box claims itself

Leave out `--store`:

```
sudo ./provision.sh --binary ./pos-edge-v1.2.0-x86_64-unknown-linux-gnu.bin \
  --cloud https://cloud.example.com
```

No `config.toml` is written. `pos-edge-claim.service` runs `pos-edge claim --cloud
https://cloud.example.com` straight away, and again on every boot until the box has been claimed:

1. The box asks the cloud for a code and shows it: on the kiosk if it has one, and on the page
   `claim` serves (ADR-0148).
2. Someone with console rights opens **Activation → Claim a box**, types the code, and picks the
   store and the device slot.
3. The box collects its credential and writes `config.toml`, and the unit starts the edge. A claimed
   box is an activated box, so there is no activation step. Pair the tills and trade.

The claim runs as `pos`, the edge's own user, and that matters. The Linux keyring the credential
goes into is per user, so a credential collected as root is one the edge would never find.

Follow it with `journalctl -u pos-edge-claim -f`. A failed attempt (no network yet, say) is retried
every 30 seconds, and once `config.toml` exists the unit is skipped on every boot.

### 3. With cloud-init

[`deploy/appliance/cloud-init.yaml`](../../deploy/appliance/cloud-init.yaml) is user-data for a
mini-PC or a VM that boots a Debian or Ubuntu cloud image. On first boot it:

1. installs `curl` and `minisign`;
2. downloads the release binary and checks its signature;
3. downloads `provision.sh` and checks it against the SHA-256 of the copy you reviewed;
4. runs `provision.sh` with `--cloud` (and `--kiosk`);
5. reboots once, so that the kiosk can take the screen.

The box then claims itself, as in 2.

Every value in its *Replace* block is a placeholder, and the script refuses to run until all of them
are replaced: the release URL, tag and target, the release public key (the second line of
`minisign.pub`), where you host `provision.sh` and its checksum (`sha256sum provision.sh`), and the
cloud. None of them is a secret and none names a store, so one file serves every box.

Output goes to `/var/log/cloud-init-output.log`. The box reboots only if provisioning finished, so a
failed run leaves it up, with the reason in that log.

## The kiosk

`--kiosk` adds a till on the box's own screen: cage, a Wayland compositor that shows one window full
screen, running Chromium on the till. There is no display manager and no desktop.

- **At boot**, `pos-kiosk.service` logs the unprivileged `pos-kiosk` account in on tty1 through PAM.
  That registers a session with logind, and the session is what lets cage use the screen, keyboard
  and touch without any group membership. `/usr/local/libexec/pos-kiosk` then waits for something to
  show: the till once the edge answers `/healthz`, or the claim page on a box that has not been
  claimed yet. The screen stays blank while it waits, rather than showing an error. When the claim
  writes `config.toml`, the session ends and comes back on the till.
- **It is a till like any other, and pairs like one** ([ADR-0030](../adr/0030-pairing-and-offline-auth.md)).
  It opens on the pairing screen. For the store's first device, read the code on the box with
  `journalctl -u pos-edge | grep pair` and type it in. After that, mint codes from **Devices** on a
  till that is already paired.
- **It opens `http://127.0.0.1:8787/`**, or wherever `bind` in `config.toml` points; a wildcard bind
  is reached on loopback. A browser treats loopback as a secure context even over plain http, so the
  kiosk gets everything a till on https gets.
- **It starts at the next boot, never during provisioning.** Starting it replaces the login on tty1,
  which may be the very session running the script. Reboot, or run `sudo systemctl start pos-kiosk`
  from an SSH session.
- **tty1 belongs to the kiosk, and there is no way off it at the till.** The script disables the
  login prompt on tty1, and cage does not switch consoles unless started with `-s`, so `Ctrl+Alt+F2`
  does nothing: a customer at the counter cannot reach a login. Use SSH. To get the screen back for a
  console login, run `sudo systemctl stop pos-kiosk && sudo systemctl start getty@tty1` from an SSH
  session.
- **The browser is the distribution's Chromium.** Debian installs the `chromium` package. Ubuntu
  24.04 has no Chromium deb (its `chromium-browser` package only installs the snap), so the script
  installs the snap when snapd is running. In a chroot, where it is not, the script says to run
  `sudo snap install chromium` once the box is up. The kiosk waits until `/usr/bin/chromium` or
  `/snap/bin/chromium` exists.
- **To take it off**, run `sudo systemctl disable --now pos-kiosk && sudo systemctl enable getty@tty1`.

`journalctl -u pos-kiosk` says what the kiosk is waiting for, and what cage and Chromium said.

## Building an image

The appliance is not a published image: ADR-0150 declines the supply chain that would take. A fork
builds its own, and the script is written to run inside whatever builds it. In a chroot systemd is not
running, so the units are **enabled and nothing is started**. It all starts on the image's first boot.

One rule: **never pass `--store` to an image build.** An image with a `config.toml` in it belongs to
one store, and every box made from it would be that store. Without one the image is generic, and each
box claims its own store.

The script's part is the same whatever the tool (a Packer chroot, mmdebstrap, `virt-customize
--run-command` against a cloud image). With the image's root filesystem at `./rootfs`:

```
sudo mount -t proc proc ./rootfs/proc
sudo cp -r deploy ./rootfs/root/deploy
sudo cp pos-edge-v1.2.0-x86_64-unknown-linux-gnu.bin ./rootfs/root/pos-edge
sudo chroot ./rootfs /root/deploy/appliance/provision.sh --binary /root/pos-edge \
  --cloud https://cloud.example.com --kiosk
sudo umount ./rootfs/proc
```

Copying `deploy/` rather than the script alone lets it install `deploy/edge/pos-edge.service` from
beside itself. On its own it installs the copy it carries, which the build keeps identical.

- **On Ubuntu with `--kiosk`**, the Chromium snap cannot be installed in a chroot. Install it on
  first boot instead: a cloud-init `runcmd` of `snap install chromium`, or `virt-customize
  --firstboot-command 'snap install chromium'`. The kiosk waits for it.
- **Before you capture the image**, clear what makes a machine itself, as for any cloned image: empty
  `/etc/machine-id` so that each box generates its own (boxes sharing one can end up sharing a DHCP
  lease), remove the SSH host keys, and run whatever else your tool's generalise step does
  (`virt-sysprep`, for example). Nothing the script writes is per-machine: there is no
  `config.toml`, no credential, and an `env` file whose every line is commented out.

## Updating

- **The edge updates itself.** An appliance has the same slots and the same signed over-the-air
  updates as every Linux store ([`deploy/edge/README.md`](../../deploy/edge/README.md)), and nothing
  about them is appliance-specific. Re-running `provision.sh` is safe and never replaces the binary
  the edge is running: with `bin/current` present it refreshes only the rescue copy.
- **The OS, cage and Chromium update through the distribution**, not through the edge's updater
  (ADR-0150). Keep unattended upgrades on. Ubuntu has them on by default; on Debian, install
  `unattended-upgrades`, and Debian's Chromium arrives through the security archive it follows. The
  Ubuntu snap refreshes itself; `sudo snap set system refresh.timer=<window>` keeps that outside
  trading hours.
- **Reboots cost more than they should**, for the reason in *What is not proven yet*. Schedule the
  ones kernel updates ask for outside trading hours.

## Troubleshooting

**`provision.sh` refuses: "supports Debian 12 and Ubuntu 24.04 only".** It read `/etc/os-release` and
found something else. Install one of those two.

**"no pos-edge is installed here yet".** The first run on a box needs `--binary`. Later runs do not,
because the edge keeps its own binary up to date.

**"this box is already store X, not Y".** A box's event log belongs to the store that recorded it,
so the script will not point it at another store. A box moving stores is wiped and provisioned
afresh, since store boxes are cattle ([ADR-0003](../adr/0003-cattle-not-pets.md)). *Replacing the
machine* in [Bring a store online](bring-a-store-online.md) says what the old one takes with it.

**`pos-edge-claim` fails every 30 seconds with an error reading `/var/lib/pos-edge/config.toml`.**
The binary has no `claim` command, so it started as a store server and found no config. Use a release
that has `claim`, or provision with `--store`.

**`systemctl status pos-edge-claim` says `ConditionPathExists=!/var/lib/pos-edge/config.toml was not met`.**
That is correct. The box has a `config.toml`, because it has been claimed or was provisioned with
`--store`, so there is nothing to claim.

**The kiosk screen stays black.** Read `journalctl -u pos-kiosk`:

- `waiting for http://127.0.0.1:8787/healthz` means the edge is not answering. Check
  `systemctl status pos-edge` and `journalctl -u pos-edge`.
- `waiting for http://127.0.0.1:8080/` means the box has not been claimed, and nothing is serving the
  claim page. See the `claim` entry above.
- `no Chromium at /usr/bin/chromium or /snap/bin/chromium` means the browser is not installed yet.

**The kiosk restarts over and over, and its journal mentions the seat, logind or a session.** Cage
could not take the screen. Check that no display manager or desktop is installed, that
`libpam-systemd` is, and that tty1 is the console on the screen. `loginctl` should list a session for
`pos-kiosk` on `seat0`.

**After a reboot, the kiosk shows the activation screen.** The box lost its credential. See the
keyring below.

**`pos-edge` fails with `status=203/EXEC`.** The binary cannot run on this machine, which usually
means it was built for the other architecture.

**The kiosk works but tills on the LAN time out.** A packet filter is in the way. The script opens
the port only on a filter that is running when it runs, the same rule as the console's installer. A
filter switched on later needs the port opened by hand: `sudo ufw allow 8787/tcp`.

**A USB printer shows as `Unavailable`.** The service user needs the printer's group. On every Linux
store that user is `pos`: `sudo usermod -aG lp pos`, then restart the edge.

## What is proven, and what is not proven yet

**Checked on every pull request**: `provision.sh` parses and passes shellcheck, and so do the kiosk
launcher inside it and the first-boot script inside `cloud-init.yaml`. The same check fails the build
if the embedded unit or `config.toml` drifts from its source.

**Exercised by hand**, on Ubuntu 24.04 in a chroot (the image-building path):

- claim mode with `--kiosk`, and store mode;
- a second run, which changes nothing;
- the refusal to change a box's store;
- the embedded unit, when the script runs outside a checkout;
- `systemd-analyze verify` on the three units;
- the launcher choosing the right page for each form of `bind`, and handing over from the claim page
  to the till;
- `cloud-init schema` accepting the user-data, and its first-boot script refusing a tampered binary
  and an unreviewed `provision.sh`.

**Not proven yet**:

- **A real boot.** None of this has run with systemd as PID 1, on hardware or in a VM: not the claim
  unit's retry and hand-over, not the kiosk's logind session, not cage on a real screen and touch.
- **Debian 12** has not been run at all. The distribution check accepts it, and the package names are
  Debian's own (`chromium`, `cage`).
- **The Chromium snap under cage** on Ubuntu 24.04 is described here, not tested.
- **The claim unit against the real `pos-edge claim`** under systemd. The command and its page are
  tested on their own (`crates/pos-edge/tests/claim.rs`), and the unit against a stand-in.
- **The keyring does not survive a reboot.** The device credential lives in the Linux kernel keyring,
  which a reboot empties ([ADR-0086](../adr/0086-edge-keyvault-and-activation.md); row P2 of the
  [gate register](../gate-register.md)). The store server keeps serving the counter without it,
  because the activation gate withholds cloud sync and never local trading. But the till sends a
  freshly opened page to `/setup` until the box is activated again, and the kiosk opens a fresh page
  at every boot. A claimed box is re-activated the same way: issue it a code under **Activation**.
  Until P2 closes, reboot an appliance rarely, and outside trading hours.
