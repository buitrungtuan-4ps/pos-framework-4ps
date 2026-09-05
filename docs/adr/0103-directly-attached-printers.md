# ADR-0103 — A directly-attached printer is a device file

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Closes production-readiness **P8** (`printer-escpos` has no USB or serial transport)
· Extends [ADR-0100](0100-receipt-and-ticket-printing.md)
· Relates to `docs/architecture.md` §5, [ADR-0007](0007-in-house-vs-dependency.md)

## The problem

`printer-escpos` shipped one transport, raw TCP on port 9100, and refused everything else:

> this build talks to network printers only; USB and serial need hardware bring-up

A store whose printer is on a USB cable could not print at all. Worse, the **cash drawer** is only
ever openable over USB — `docs/architecture.md` §5 forbids it on the network, because port 9100 has
no authentication and the drawer-kick rides the same channel as everything else — so the one
connection the drawer is allowed on was the one connection with no transport. The drawer was
unreachable by construction.

## The decision

**A USB or serial printer is reached by opening its OS device path and writing to it.**

`printer_escpos::device::DeviceTransport` takes the path the published device already carries in its
`address` field — `/dev/usb/lp0`, `/dev/ttyUSB0`, `\\.\COM3` — and writes bytes to it. The handle is
held between jobs and reopened once on a failed write, the same shape as the TCP transport's
reconnect, because a printer power-cycled or a cable replugged leaves a stale handle while the device
itself is fine.

### Why not a USB library

A USB thermal printer is a USB **printer-class** device, and every operating system that supports one
already exposes it as something you write bytes to. The kernel driver is doing this correctly
already.

Talking raw USB instead — `libusb` or `nusb`, claim the interface, push bulk transfers — would mean
detaching that driver to replace it with a worse one, and would gain a restaurant nothing. It would
also cost: `libusb` is a C library, and ADR-0007's whole posture on native dependencies is that a
thousand unattended store machines are not a place to maintain a C toolchain. `deny.toml` bans
`openssl-sys` for exactly this reason.

The file-handle approach adds **no dependency at all**, and it is testable without hardware: a device
node is a writable file, so a temporary file exercises the same code path.

### What is deliberately not configured here

**Serial baud rate.** Setting it in-process means `termios`, which means unsafe FFI in a crate that
has none, for a legacy connection type. It is a one-line deployment step instead —
`stty -F /dev/ttyUSB0 19200 raw`, or an `ExecStartPre` in the service unit — which
`deploy/edge/README.md` carries. USB printer-class devices have no baud rate at all, so the common
case needs nothing.

**Sensors.** `probe` reports paper and cover as `None`, the same as the network transport and for the
same reason: real-time status is `DLE EOT n`, a *read* whose reply format and timing vary by model,
and a wrong answer reads as "out of paper" and refuses to print on a printer that is fine.
Reachability *is* checked — on a directly-attached printer that is "is the device node there and
openable", which catches the two failures a shop actually hits: the cable is out, or the service user
is not in the `lp` group.

## What this does not decide

**Whether a cash drawer is attached.** The transport now exists, and `PrinterCapabilities`
`may_open_a_drawer()` already encodes the USB-only rule, but `PublishedDevice` carries no field
saying a drawer is wired to a given printer — so `assumed_capabilities` still reports
`kicks_drawer: false` and no drawer opens. Sending a drawer-kick to every USB printer on the chance
one is attached would be a behaviour nobody asked for.

Closing it is a console change: a "cash drawer attached" checkbox on the device, published in the
`devices` node, read here. That is a cloud schema slice, and it is flagged rather than guessed.

**Windows USB printers installed through the spooler.** `\\.\COM3` works as a file handle and is
covered; a printer installed as a Windows print queue needs the Win32 spooler API
(`OpenPrinter`/`StartDocPrinter`/`WritePrinter`), which is a separate transport and a separate
decision.

## Consequences

- A store can put its printer on a USB cable, which is what most single-till shops actually have.
- The drawer has a channel for the first time; whether it opens now waits on the console field above.
- No new dependency, and no C toolchain in the edge build.
- The bytes a real printer renders, and its sensors, remain a hardware-gate item
  (`docs/gate-register.md` §6). What is testable in CI — that bytes reach the device, that two jobs
  both arrive on one held handle, that an absent device is `Unreachable` rather than a panic — is
  tested.
