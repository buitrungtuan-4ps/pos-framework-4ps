// The package designer's state (ADR-0189, ADR-0190 decision 8): the draft being edited, the cloud's
// last verdict on it, and the two acts that ask the cloud, check and publish.
//
// Every edit clears the verdict, because a verdict is about the draft it was given. Publish checks
// first, with the publish's own checks, and publishes only a draft the check accepted; the cloud
// checks again when it stores the version, so a version published in between cannot slip past.

import { createSignal } from "solid-js";

import { dryRunPackageVersion, publishPackageVersion } from "../../../api/packages";
import type { PackageVersionDryRun, PackageVersionRecord } from "../../../api/schemas";
import { apiMessage } from "../../../lib/errors";
import {
  draftRequest,
  dropDraft,
  saveDraft,
  type PackageDraft,
} from "../../../lib/package-draft";

/** A change to the draft: given the draft as it is, the draft as it becomes. */
export type DraftChange = (draft: PackageDraft) => PackageDraft;

/** The draft of one package, and what the cloud said of it. */
export function useDraft(tenantId: () => string, packageId: () => string, initial: PackageDraft) {
  const [draft, setDraft] = createSignal<PackageDraft>(initial);
  const [verdict, setVerdict] = createSignal<PackageVersionDryRun | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  /** Applies `change`, keeps the result in this browser, and forgets the verdict. */
  const change = (next: DraftChange) => {
    const changed = next(draft());
    setDraft(changed);
    setVerdict(null);
    saveDraft(tenantId(), packageId(), changed);
  };

  /** Replaces the draft, as when it is started again from a version, without keeping it yet. */
  const reset = (next: PackageDraft) => {
    setDraft(next);
    setVerdict(null);
    setError("");
  };

  const ask = async (): Promise<PackageVersionDryRun | null> => {
    try {
      const found = await dryRunPackageVersion(packageId(), draftRequest(tenantId(), draft()));
      setVerdict(found);
      return found;
    } catch (caught) {
      setError(apiMessage(caught));
      return null;
    }
  };

  /** Asks the cloud whether the draft would publish. */
  const check = async () => {
    setBusy(true);
    setError("");
    try {
      await ask();
    } finally {
      setBusy(false);
    }
  };

  /** Checks the draft and, if the cloud accepts it, publishes it; the stored version, or `null`. */
  const publish = async (): Promise<PackageVersionRecord | null> => {
    setBusy(true);
    setError("");
    try {
      const found = await ask();
      if (found === null || !found.accepted) {
        return null;
      }
      const version = await publishPackageVersion(packageId(), draftRequest(tenantId(), draft()));
      dropDraft(tenantId(), packageId());
      return version;
    } catch (caught) {
      setError(apiMessage(caught));
      return null;
    } finally {
      setBusy(false);
    }
  };

  /** The refusals of the last check, none before one. */
  const refusals = () => verdict()?.refusals ?? [];

  return { draft, change, reset, verdict, refusals, busy, error, check, publish };
}

/** What `useDraft` gives the screen. */
export type DraftState = ReturnType<typeof useDraft>;
