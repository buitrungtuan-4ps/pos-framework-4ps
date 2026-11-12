// Who is signed in on this device, and how long it may sit untouched before it locks (ADR-0160).
//
// Held apart from the store's projection in `./store` because it is about the device and the person
// at it, not about the floor: the idle lock reads it, the sign-in screen and the lock write it, and
// nothing in it is folded from events. It is also the one reader of `GET /api/session`, which hands
// what the person may do to `./permissions` (ADR-0158 decision 6), so the two cannot disagree about
// who is signed in.
//
// The ids of the roles the person holds (ADR-0213) choose a package screen's role variant and a
// role's home. They are kept in memory only, like the rest of the session, and never logged.
//
// The staff code a person signed in with is kept in memory, and nowhere else, so the lock can reopen
// with their PIN alone (`docs/pos-spec.md` §16). It is never stored, logged or shown. A reload
// forgets it, and the lock then asks for the code as well.

import { createSignal } from "solid-js";

import type { SessionState } from "../api/client";
import { publishDeviceClass } from "../lib/device-class";
import { load } from "./engine";
import { clearSnapshot } from "./snapshot";
import { adoptSession, forgetPermissions } from "./permissions";

const [signedIn, setSignedIn] = createSignal(false);
const [employeeId, setEmployeeId] = createSignal<string | null>(null);
const [idleLockSeconds, setIdleLockSeconds] = createSignal(0);
const [openingFloatMinor, setOpeningFloatMinor] = createSignal(0);
const [locked, setLocked] = createSignal(false);
// Equal by content, so a session read that brings the same roles back redraws no screen.
const [roleIds, setRoleIds] = createSignal<readonly string[]>([], {
  equals: (held, next) => held.length === next.length && held.every((id, index) => id === next[index]),
});
let staffCode: string | null = null;

export { employeeId, idleLockSeconds, locked, openingFloatMinor, roleIds, signedIn };

// Takes what a session read says: who is signed in, the store's idle lock and opening float, this
// device's class where its till names one (`../lib/device-class.ts`), the roles the person holds,
// and what the person may do. An edge too old to send `role_ids` reads as no role: the base. An edge too old to send `idle_lock_seconds` never locks, and one too old
// to send `opening_float_minor` fills in no float, which is what every till did before the settings.
export function takeSession(session: SessionState): void {
  setSignedIn(session.signed_in);
  setEmployeeId(session.employee_id ?? null);
  setIdleLockSeconds(session.idle_lock_seconds ?? 0);
  setOpeningFloatMinor(session.opening_float_minor ?? 0);
  setRoleIds(session.role_ids ?? []);
  publishDeviceClass(session.device_class);
  adoptSession(session);
}

// Reads the session again, through the engine (`./registry`): whenever the store's configuration
// is read, and when a PIN opens the lock. A blip leaves what is held.
export function loadSession(): Promise<void> {
  return load("session");
}

// A person has just signed in with `code` and become `employee`.
export function signedInAs(code: string, employee: string): void {
  staffCode = code;
  setEmployeeId(employee);
  setSignedIn(true);
  setLocked(false);
}

// The code the person signed in on this device typed, while this page has it.
export function rememberedCode(): string | null {
  return staffCode;
}

// The till has locked: the person is signed out, holds nothing, as after a sign-out, and the lock
// covers the screen until a PIN opens it.
export function lockTill(): void {
  setSignedIn(false);
  setLocked(true);
  setRoleIds([]);
  forgetPermissions();
}

// A person signed out on this device: they hold nothing, and the store's configuration goes with
// them (ADR-0173). The idle lock, which keeps the device in the same service, does not call this.
export async function signedOut(): Promise<void> {
  setRoleIds([]);
  forgetPermissions();
  await clearSnapshot();
}
