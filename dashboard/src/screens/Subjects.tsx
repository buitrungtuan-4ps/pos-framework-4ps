// Subject requests (ADR-0076, Track M5) — the Data Protection contact's instrument for a PDPD/GDPR
// access, portability, or erasure request. Per-subject only: look one up by id, export its record for a
// portability/access request, or erase it (mask, irreversible) for a right-to-erasure request. Every
// action is owner-only (console.subjects.manage) and audited server-side; the audit records who acted
// on which subject, never the field values.
//
// This is the console's most sensitive T1 surface. It records and enables a human decision — it does not
// make one: the operator must confirm the lawful basis and the subject's identity, and escalate an
// EU-resident request to the Data Protection contact. That reminder is on the screen, always.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { SubjectExport, SubjectMeta } from "../api/types";
import { locale, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { actingAdmin, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import { ConfirmDialog } from "../components/kit";
import { toast } from "../components/Toast";

export function Subjects() {
  const [subjectId, setSubjectId] = createSignal("");
  const [meta, setMeta] = createSignal<SubjectMeta | null>(null);
  const [notFound, setNotFound] = createSignal(false);
  const [exported, setExported] = createSignal<SubjectExport | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [pendingErase, setPendingErase] = createSignal(false);

  // console.subjects.manage → owner only (mirrors the backend; the server re-checks every route).
  const canManage = () => actingAdmin()?.role === "owner";

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  // Reset the result panel when the tenant changes — a subject id is tenant-scoped.
  onScopedContext("tenant", () => {
    setMeta(null);
    setExported(null);
    setNotFound(false);
    setError("");
  });

  const lookup = async () => {
    const id = subjectId().trim();
    if (!id) {
      return;
    }
    setError("");
    setExported(null);
    setNotFound(false);
    setBusy(true);
    try {
      setMeta(await api.lookupSubject(tenantId(), id));
    } catch (caught) {
      setMeta(null);
      if (caught instanceof ApiError && caught.status === 404) {
        setNotFound(true);
      } else {
        fail(caught);
      }
    } finally {
      setBusy(false);
    }
  };

  const doExport = async () => {
    const id = meta()?.subject_id;
    if (!id) {
      return;
    }
    setBusy(true);
    try {
      setExported(await api.exportSubject(tenantId(), id));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // The portability payload as a downloaded JSON file — a client-side blob of what the server returned.
  const downloadExport = (data: SubjectExport) => {
    const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    try {
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = `subject-${data.subject_id}.json`;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
    } finally {
      URL.revokeObjectURL(url);
    }
  };

  const doErase = async () => {
    const id = meta()?.subject_id;
    if (!id) {
      return;
    }
    setBusy(true);
    try {
      await api.eraseSubject(tenantId(), id);
      setPendingErase(false);
      setExported(null);
      toast.ok(t("subjects.erased"));
      await lookup();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const collectedAt = (ms: number) =>
    new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeStyle: "short" }).format(
      new Date(ms),
    );

  return (
    <div>
      <PageHeader title={t("subjects.title")} description={t("subjects.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <div class="rounded-token border border-danger p-3 text-sm text-ink">
            <span aria-hidden="true">⚠️ </span>
            {t("subjects.guardrail")}
          </div>

          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          <Show
            when={canManage()}
            fallback={<Banner tone="danger" message={t("subjects.ownerOnly")} />}
          >
            <Card title={t("subjects.lookupTitle")}>
              <div class="flex flex-col gap-3">
                <TextField
                  label={t("subjects.subjectId")}
                  value={subjectId()}
                  onInput={setSubjectId}
                  placeholder={t("subjects.subjectIdPlaceholder")}
                />
                <div>
                  <Button disabled={busy() || !subjectId().trim()} onClick={() => void lookup()}>
                    {t("subjects.lookup")}
                  </Button>
                </div>
              </div>
            </Card>

            <Show when={notFound()}>
              <Banner tone="danger" message={t("subjects.notFound")} />
            </Show>

            <Show when={meta()}>
              {(found) => (
                <Card title={t("subjects.result")}>
                  <div class="flex flex-col gap-4">
                    <dl class="grid grid-cols-2 gap-2 text-sm">
                      <dt class="text-ink-muted">{t("subjects.subjectId")}</dt>
                      <dd class="break-all font-mono text-ink">{found().subject_id}</dd>
                      <dt class="text-ink-muted">{t("subjects.collectedAt")}</dt>
                      <dd class="text-ink">{collectedAt(found().collected_at_ms)}</dd>
                      <dt class="text-ink-muted">{t("subjects.status")}</dt>
                      <dd class="text-ink">
                        {found().masked ? t("subjects.masked") : t("subjects.holdsData")}
                      </dd>
                      <dt class="text-ink-muted">{t("subjects.fieldCount")}</dt>
                      <dd class="text-ink">{String(found().field_count)}</dd>
                    </dl>

                    <div class="flex flex-wrap gap-2">
                      <Button variant="secondary" disabled={busy()} onClick={() => void doExport()}>
                        {t("subjects.export")}
                      </Button>
                      <Show when={!found().masked}>
                        <Button
                          variant="danger"
                          disabled={busy()}
                          onClick={() => setPendingErase(true)}
                        >
                          {t("subjects.erase")}
                        </Button>
                      </Show>
                    </div>

                    <Show when={exported()}>
                      {(data) => (
                        <div class="flex flex-col gap-2">
                          <div class="flex items-center justify-between">
                            <p class="text-sm font-medium text-ink">{t("subjects.fields")}</p>
                            <Button
                              variant="secondary"
                              disabled={busy()}
                              onClick={() => downloadExport(data())}
                            >
                              {t("subjects.downloadJson")}
                            </Button>
                          </div>
                          <div class="overflow-x-auto">
                            <table class="w-full text-left text-sm">
                              <thead>
                                <tr class="border-b border-line text-ink-muted">
                                  <th class="py-2 pr-4 font-medium">{t("subjects.field")}</th>
                                  <th class="py-2 font-medium">{t("subjects.value")}</th>
                                </tr>
                              </thead>
                              <tbody>
                                <For each={Object.entries(data().fields)}>
                                  {([key, value]) => (
                                    <tr class="border-b border-line align-top text-ink">
                                      <td class="py-2 pr-4 font-mono text-xs">{key}</td>
                                      <td class="py-2 break-all">{value}</td>
                                    </tr>
                                  )}
                                </For>
                              </tbody>
                            </table>
                          </div>
                        </div>
                      )}
                    </Show>
                  </div>
                </Card>
              )}
            </Show>
          </Show>
        </div>
      </RequireContext>

      <ConfirmDialog
        open={pendingErase()}
        title={t("subjects.eraseTitle")}
        message={t("subjects.eraseMessage")}
        confirmLabel={t("subjects.erase")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        busy={busy()}
        danger
        typeToConfirm={meta()?.subject_id ?? ""}
        typePrompt={t("subjects.eraseConfirmPrompt")}
        onConfirm={() => void doErase()}
        onCancel={() => setPendingErase(false)}
      />
    </div>
  );
}
