// What the Custom data screen loads and asks (ADR-0194 decision 7): the tenant's package versions
// and translation grid, the question being built, and the server's answer.

import { createMemo, createSignal } from "solid-js";

import { api } from "../../api/client";
import { customValueSummary } from "../../api/custom-values";
import { listPackages, listPackageVersions } from "../../api/packages";
import type { PackageVersionRecord, TranslationGrid, ValueSummary } from "../../api/schemas";
import { locale, t } from "../../i18n";
import { gridLabel, listsById, type ReportTarget, reportTargets } from "../../lib/custom-data";
import { apiMessage } from "../../lib/errors";
import { storeId, tenantId } from "../../state/session";

type Loaded = { readonly versions: PackageVersionRecord[]; readonly grid: TranslationGrid };

/** The question's four choices, each a signal. */
function useQuestion() {
  const [targetId, setTargetId] = createSignal("");
  const [fieldId, setFieldId] = createSignal("");
  const [filterFieldId, setFilterFieldId] = createSignal("");
  const [filterValue, setFilterValue] = createSignal("");
  return { targetId, setTargetId, fieldId, setFieldId, filterFieldId, setFilterFieldId, filterValue, setFilterValue };
}

export function useCustomData() {
  const [loaded, setLoaded] = createSignal<Loaded | null>(null);
  const [error, setError] = createSignal("");
  const [summary, setSummary] = createSignal<ValueSummary | null>(null);
  const [busy, setBusy] = createSignal(false);
  const question = useQuestion();

  const load = async () => {
    setError("");
    try {
      const packages = await listPackages(tenantId());
      const versions = await Promise.all(
        packages.map((record) => listPackageVersions(tenantId(), record.package_id)),
      );
      setLoaded({ versions: versions.flat(), grid: (await api.getTranslations(tenantId())).value });
    } catch (caught) {
      setError(apiMessage(caught));
    }
  };

  const targets = createMemo(() => reportTargets(loaded()?.versions ?? []));
  const lists = createMemo(() => listsById(loaded()?.versions ?? []));
  const label = (key: string) => gridLabel(loaded()?.grid ?? {}, key, locale());
  const targetLabel = (each: ReportTarget) =>
    each.messageKey ? t(each.messageKey) : label(each.labelKey ?? each.id);
  const target = (): ReportTarget | undefined =>
    targets().find((each) => each.id === question.targetId());

  const run = async () => {
    const chosen = target();
    if (!chosen || !question.fieldId()) {
      return;
    }
    setBusy(true);
    setError("");
    try {
      const request = {
        object: chosen.object,
        objectTypeId: chosen.objectTypeId,
        customFieldId: question.fieldId(),
        filterFieldId: question.filterFieldId() || undefined,
        filterValue: question.filterValue() || undefined,
      };
      setSummary(await customValueSummary(tenantId(), storeId(), request));
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  return { loaded, error, summary, setSummary, busy, question, targets, lists, label, targetLabel, target, load, run };
}

export type CustomDataState = ReturnType<typeof useCustomData>;
