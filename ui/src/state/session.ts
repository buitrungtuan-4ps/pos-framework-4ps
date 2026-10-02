// Who is signed in on this device, and how long it may sit untouched before it locks (ADR-0160).
//
// Held apart from the store's projection in `./store` because it is about the device and the person
// at it, not about the floor: the idle lock reads it, the sign-in screen and the lock write it, and
// nothing in it is folded from events. It is also the one reader of `GET /api/session`, which hands
// what the person may do to `./permissions` (ADR-0158 decision 6), so the two cannot disagree about
// who is signed in.
//
// The staff code a person signed in with is kept in memory, and nowhere else, so the lock can reopen
// with their PIN alone (`docs/pos-spec.md` §16). It is never stored, logged or shown. A reload
// forgets it, and the lock then asks for the code as well.

import { createSignal } from "solid-js";

import { api, type SessionState } from "../api/client";
import { adoptSession, forgetPermissions } from "./permissions";

const [signedIn, setSignedIn] = createSignal(false);
const [employeeId, setEmployeeId] = createSignal<string | null>(null);
const [idleLockSeconds, setIdleLockSeconds] = createSignal(0);
const [locked, setLocked] = createSignal(false);
let staffCode: string | null = null;

export { employeeId, idleLockSeconds, locked, signedIn };

// Takes what a session read says: who is signed in, the store's idle lock, and what the person may
// do. An edge too old to send `idle_lock_seconds` never locks, which is what every till did before
// the setting.
export function takeSession(session: SessionState): void {
  setSignedIn(session.signed_in);
  setEmployeeId(session.employee_id ?? null);
  setIdleLockSeconds(session.idle_lock_seconds ?? 0);
  adoptSession(session);
}

// Reads the session again: whenever the store's configuration is read, and when a PIN opens the
// lock. Forgiving like the store's loaders: a blip leaves what is held.
export async function loadSession(): Promise<void> {
  let session;
  try {
    session = await api.session();
  } catch {
    return;
  }
  takeSession(session);
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
  forgetPermissions();
}
