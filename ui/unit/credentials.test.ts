// `api/credentials.ts` (ADR-0111, ADR-0173): the base and the token are written together, and a `401`
// clears the token and keeps the base.

import { describe, expect, it, vi } from "vitest";

import type { CredentialStore } from "../src/api/credentials";

/** A fresh copy of the module over an in-memory store, and the store's contents. */
async function fresh() {
  vi.resetModules();
  const held = new Map<string, string>();
  const store: CredentialStore = {
    read: (key) => held.get(key) ?? null,
    write: (key, value) => void held.set(key, value),
    clear: (key) => void held.delete(key),
  };
  const module = await import("../src/api/credentials");
  module.installCredentials(store);
  return { module, held };
}


describe("a pairing", () => {
  it("keeps the token and the base together, with a pairing id", async () => {
    const { module } = await fresh();
    module.rememberPairing("https://edge.example:8443", "token-1");
    expect(module.deviceToken()).toBe("token-1");
    expect(module.edgeBase()).toBe("https://edge.example:8443");
    expect(module.pairingId()).not.toBeNull();
  });

  it("keeps an in-store till's requests root-relative", async () => {
    const { module, held } = await fresh();
    module.rememberPairing("https://edge.example", "token-1");
    module.rememberPairing("", "token-2");
    expect(module.edgeBase()).toBe("");
    expect(held.has("pos-edge.base-url")).toBe(false);
  });

  it("starts a new pairing id with each pairing", async () => {
    const { module } = await fresh();
    module.rememberPairing("", "token-1");
    const first = module.pairingId();
    module.rememberPairing("", "token-2");
    expect(module.pairingId()).not.toBe(first);
  });
});

describe("clearDeviceToken", () => {
  it("drops the token and the pairing id and keeps the base", async () => {
    const { module } = await fresh();
    module.rememberPairing("https://edge.example", "token-1");
    module.clearDeviceToken();
    expect(module.deviceToken()).toBeNull();
    expect(module.pairingId()).toBeNull();
    expect(module.edgeBase()).toBe("https://edge.example");
  });

  it("tells every listener the pairing is forgotten", async () => {
    const { module } = await fresh();
    const listener = vi.fn();
    module.onPairingForgotten(listener);
    module.clearDeviceToken();
    expect(listener).toHaveBeenCalledOnce();
  });
});

describe("pairingId", () => {
  it("is null for a device that has not paired", async () => {
    const { module } = await fresh();
    expect(module.pairingId()).toBeNull();
  });

  it("gives a device paired before the id existed one, and keeps it", async () => {
    const { module, held } = await fresh();
    held.set("pos-edge.device-token", "token-from-before");
    const given = module.pairingId();
    expect(given).not.toBeNull();
    expect(module.pairingId()).toBe(given);
  });
});

describe("liveSocketUrl", () => {
  it("swaps the base's scheme for the socket's", async () => {
    const { module } = await fresh();
    module.rememberPairing("https://edge.example:8443", "t");
    expect(module.liveSocketUrl()).toBe("wss://edge.example:8443/ws");
    module.rememberPairing("http://10.0.0.2:8080", "t");
    expect(module.liveSocketUrl()).toBe("ws://10.0.0.2:8080/ws");
  });

  it("follows the page's own origin when the base is empty", async () => {
    const { module } = await fresh();
    expect(module.liveSocketUrl()).toBe(`ws://${window.location.host}/ws`);
  });
});
