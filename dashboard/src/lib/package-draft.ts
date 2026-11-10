// A package version being designed (ADR-0189 decision 1): a draft the console holds until it is
// published. A published version never changes, so editing one means opening it as a draft and
// publishing the draft as the next version.
//
// The draft is the publish's own body without the tenant, so every part the cloud learns to take
// (screens, flows, rules, data) is a part of the draft, typed from the same generated schema. The
// cloud checks a draft with the publish's own checks (`dryRunPackageVersion`), and each part's
// designer shows the refusals that name it (`refusalsFor`).
//
// A draft is kept in this browser while it is edited, per tenant and package, so a reload does not
// lose it. It holds no personal data: a package carries ids and translation keys, never text a
// person typed about a guest. Storage can be missing or refuse a write; the draft then lives only
// as long as the page.

import type { PackageRefusal, PackageVersionRecord, PackageVersionRequest } from "../api/schemas";

/** A version being designed: what a publish would send, but for the tenant. */
export type PackageDraft = Omit<PackageVersionRequest, "tenant_id">;

/** A draft with nothing in it yet but its label. */
export function blankDraft(versionLabel = ""): PackageDraft {
  return { version_label: versionLabel, time_slots: [], data: {}, screens: [], flows: [], reactions: [] };
}

/** A published version, opened as the draft of the next one. Every part is copied as it is. */
export function draftFromVersion(version: PackageVersionRecord): PackageDraft {
  const content = version.content;
  return {
    version_label: content.version_label,
    time_slots: content.time_slots,
    data: content.data,
    screens: content.screens,
    flows: content.flows,
    reactions: content.reactions,
  };
}

/** The body a dry run and a publish send for `draft`. */
export function draftRequest(tenantId: string, draft: PackageDraft): PackageVersionRequest {
  return { ...draft, tenant_id: tenantId, version_label: draft.version_label.trim() };
}

/**
 * The refusals a part's designer shows: those whose field is the part's name or below it, so
 * `screens` takes `screens` and `data` takes `data.object_types`.
 */
export function refusalsFor(
  refusals: readonly PackageRefusal[],
  part: string,
): readonly PackageRefusal[] {
  return refusals.filter((refusal) => refusal.field === part || refusal.field.startsWith(`${part}.`));
}

const KEY_PREFIX = "pos.package-draft";

function storageKey(tenantId: string, packageId: string): string {
  return `${KEY_PREFIX}.${tenantId}.${packageId}`;
}

/** The draft of `packageId` this browser kept, if any. */
export function loadDraft(tenantId: string, packageId: string): PackageDraft | null {
  try {
    const text = localStorage.getItem(storageKey(tenantId, packageId));
    if (text === null) {
      return null;
    }
    const parsed: unknown = JSON.parse(text);
    return isDraft(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

/** Keeps `draft` in this browser. */
export function saveDraft(tenantId: string, packageId: string, draft: PackageDraft): void {
  try {
    localStorage.setItem(storageKey(tenantId, packageId), JSON.stringify(draft));
  } catch {
    // No storage, or full: the draft lives as long as the page.
  }
}

/** Forgets the kept draft, once it is published or discarded. */
export function dropDraft(tenantId: string, packageId: string): void {
  try {
    localStorage.removeItem(storageKey(tenantId, packageId));
  } catch {
    // Nothing kept, or nothing to keep it in.
  }
}

function isDraft(value: unknown): value is PackageDraft {
  return (
    typeof value === "object" &&
    value !== null &&
    "version_label" in value &&
    typeof value.version_label === "string"
  );
}
