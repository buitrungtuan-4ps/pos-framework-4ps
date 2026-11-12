// The Floor screen's writes to the area tree (ADR-0211): moving a branch, and showing the cloud's
// refusal of a tree write on the row it is about rather than in a banner above the screen. Kept out
// of `Floor.tsx`, whose one function is already at its size baseline.

import { createSignal } from "solid-js";

import { api, ApiError } from "../../api/client";
import type { Area } from "../../api/schemas";
import { toast } from "../../components/Toast";
import { t } from "../../i18n";
import { treeRefusal, type TreeRefusal } from "../../lib/area-tree";

/** The operator's words for a tree refusal. */
export function refusalMessage(reason: TreeRefusal): string {
  switch (reason) {
    case "AREA_PARENT_NOT_FOUND":
      return t("floor.refusalParentNotFound");
    case "AREA_PARENT_ARCHIVED":
      return t("floor.refusalParentArchived");
    case "AREA_CYCLE":
      return t("floor.refusalCycle");
    case "AREA_TOO_DEEP":
      return t("floor.refusalTooDeep");
    case "AREA_HAS_LIVE_CHILDREN":
      return t("floor.refusalHasLiveChildren");
  }
}

/** A caught failure in the operator's words when it is a tree refusal, and as it came otherwise. */
export function inOperatorWords(caught: unknown): unknown {
  const reason = caught instanceof ApiError ? treeRefusal(caught.details) : null;
  return reason && caught instanceof ApiError
    ? new ApiError(caught.status, refusalMessage(reason), caught.canonical, caught.details)
    : caught;
}

type Deps = {
  readonly tenantId: () => string;
  readonly setBusy: (busy: boolean) => void;
  /** Reloads the plan after a write that landed. */
  readonly saved: () => Promise<void>;
  /** Handles any failure that is not a tree refusal. */
  readonly fail: (caught: unknown) => Promise<void>;
};

/** The tree's refusals, by area, and the move that can earn one. */
export function useAreaTreeWrites(deps: Deps) {
  const [refusals, setRefusals] = createSignal<ReadonlyMap<string, string>>(new Map());
  const set = (areaId: string, message: string | null) => {
    const next = new Map(refusals());
    if (message === null) {
      next.delete(areaId);
    } else {
      next.set(areaId, message);
    }
    setRefusals(next);
  };

  /** Shows a tree refusal on `areaId`'s row and answers true, or hands anything else to `fail`. */
  const settle = async (areaId: string, caught: unknown): Promise<boolean> => {
    const reason = caught instanceof ApiError ? treeRefusal(caught.details) : null;
    if (reason) {
      set(areaId, refusalMessage(reason));
      return true;
    }
    await deps.fail(caught);
    return false;
  };

  /** Moves `area`, with what sits inside it, inside `parentId` or to the top for `null`. */
  const move = async (area: Area, parentId: string | null) => {
    deps.setBusy(true);
    set(area.area_id, null);
    try {
      await api.updateArea(area.area_id, deps.tenantId(), { name: area.name, status: area.status, parentAreaId: parentId }, area.etag);
      toast.ok(t("floor.areaMoved"));
      await deps.saved();
    } catch (caught) {
      await settle(area.area_id, caught);
    } finally {
      deps.setBusy(false);
    }
  };

  return { refusal: (areaId: string) => refusals().get(areaId), clear: (areaId: string) => set(areaId, null), settle, move };
}
