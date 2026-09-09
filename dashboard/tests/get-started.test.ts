// The activation path has to be legible before it is walked (Wave 3 · Stage 2).
//
// Signing into a fresh console lands on a store hub with no store, whose only advice is "choose it
// in the top bar" — which cannot be followed, because there is nothing to choose. The work is a
// chain: a tenant, a store under it, a key that store's machine can present, a published
// configuration, the machine installed and a till admitted, and then the shop reports. Every link
// has a screen in this console and nothing anywhere said what the chain was.
//
// What is pinned here is not the list — a list is not a defect risk — but the three ways a
// checklist starts lying: reporting work as undone when the read was refused, reporting it as
// undone when nothing was read at all, and declaring the setup finished on either.

import { describe, expect, it } from "vitest";

import {
  requiredProgress,
  setupComplete,
  STEPS,
  statuses,
  type Progress,
  type StepId,
} from "../src/lib/get-started";
import { LOADING, type Panel } from "../src/lib/panel";

const ready = <T,>(value: T): Panel<T> => ({ state: "ready", value });
const failed = <T,>(): Panel<T> => ({ state: "failed", message: "the caller is not permitted" });

/** A console signed into for the first time: one read has answered, and it answered zero. */
const FRESH: Progress = {
  tenantChosen: false,
  storeChosen: false,
  tenants: ready(0),
  brands: LOADING,
  stores: LOADING,
  keys: LOADING,
  versions: LOADING,
  devices: LOADING,
  reporting: LOADING,
};

/** A store that is fully set up and has reported. */
const FINISHED: Progress = {
  tenantChosen: true,
  storeChosen: true,
  tenants: ready(1),
  brands: ready(1),
  stores: ready(1),
  keys: ready(1),
  versions: ready(3),
  devices: ready(2),
  reporting: ready(true),
};

describe("a fresh console", () => {
  it("asks for a tenant and holds every later step as unasked", () => {
    const by = statuses(FRESH);
    expect(by.tenant).toBe("todo");
    // Not "todo": with no tenant chosen, whether a brand exists was never asked. A cross here would
    // be a claim about data the console has not looked at.
    for (const id of ["brand", "store", "apiKey", "config", "device", "reporting"] as StepId[]) {
      expect(by[id]).toBe("waiting");
    }
  });

  it("is not complete", () => {
    expect(setupComplete(statuses(FRESH))).toBe(false);
  });
});

describe("a read that was refused", () => {
  it("says so instead of reporting the step undone", () => {
    const by = statuses({ ...FINISHED, devices: failed() });
    expect(by.device).toBe("unknown");
  });

  it("never counts as done, so the checklist stays up", () => {
    // An admin whose role cannot read the fleet must not be told the setup is finished on the
    // strength of a request that failed — nor that it is unfinished. The list stays, saying which
    // step it could not check.
    const by = statuses({ ...FINISHED, reporting: failed() });
    expect(by.reporting).toBe("unknown");
    expect(setupComplete(by)).toBe(false);
  });
});

describe("a read still in flight", () => {
  it("is checking, and does not complete the setup", () => {
    const by = statuses({ ...FINISHED, versions: LOADING });
    expect(by.config).toBe("checking");
    expect(setupComplete(by)).toBe(false);
  });
});

describe("the store's key", () => {
  it("is not done by a tenant-wide key alone", () => {
    // The count reaching `statuses` is already narrowed to keys bound to this store: S1 refuses a
    // tenant-wide key on a store's own /sync routes, so a tenant holding one integration key and no
    // store key has not finished this step. Zero is zero however many keys the tenant has.
    expect(statuses({ ...FINISHED, keys: ready(0) }).apiKey).toBe("todo");
  });
});

describe("a finished setup", () => {
  it("is complete, and stays complete without the optional step", () => {
    expect(setupComplete(statuses(FINISHED))).toBe(true);
    // A brand is optional because POST /admin/stores does not require one. A checklist that
    // demanded it would be demanding work the server does not.
    expect(setupComplete(statuses({ ...FINISHED, brands: ready(0) }))).toBe(true);
    expect(statuses({ ...FINISHED, brands: ready(0) }).brand).toBe("todo");
  });
});

describe("the progress line", () => {
  it("counts only the required steps", () => {
    const optional = STEPS.filter((step) => step.optional === true).length;
    expect(optional).toBe(1);
    expect(requiredProgress(statuses(FINISHED))).toEqual({
      done: STEPS.length - optional,
      total: STEPS.length - optional,
    });
    expect(requiredProgress(statuses(FRESH))).toEqual({
      done: 0,
      total: STEPS.length - optional,
    });
  });
});

describe("the step list", () => {
  it("gives every step a distinct id and every link its own label", () => {
    expect(new Set(STEPS.map((step) => step.id)).size).toBe(STEPS.length);
    const labels = STEPS.flatMap((step) => (step.go ? [step.go.label] : []));
    expect(new Set(labels).size).toBe(labels.length);
  });

  it("covers every id the status table answers, in the order the work happens", () => {
    expect(STEPS.map((step) => step.id)).toEqual([
      "tenant",
      "brand",
      "store",
      "apiKey",
      "config",
      "device",
      "reporting",
    ]);
    expect(Object.keys(statuses(FRESH)).sort()).toEqual([...STEPS.map((s) => s.id)].sort());
  });
});
