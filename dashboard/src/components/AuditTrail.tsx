// A drop-in audit-trail panel for a Detail view (ADR-0069, Track G2 slice 6). Given an entity type
// (and optionally an id), it reads that entity's history from the same `GET /admin/audit` the Audit
// screen uses and shows who created it, who last changed it, and the entries in between. With no id
// it reads every entry of that type — e.g. "recently resolved" device proposals, with the resolver.

import { createResource, For, Show } from "solid-js";

import { api } from "../api/client";
import type { AuditEntry } from "../api/types";
import { locale, t } from "../i18n";
import { formatRelativeAge } from "../lib/format";
import { StatusBadge } from "./kit";

/** How many entries one panel pulls; a Detail view's history is short, so this is generous. */
const AUDIT_PANEL_LIMIT = 50;

function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

export function AuditTrail(props: { entityType: string; entityId?: string }) {
  const [entries] = createResource(
    () => (props.entityType ? ([props.entityType, props.entityId] as const) : null),
    ([entityType, entityId]) =>
      api.listAudit({ entityType, entityId, limit: AUDIT_PANEL_LIMIT }),
  );

  // The read is newest-first, so the last entry is the create and the first is the latest change.
  const created = () => {
    const list = entries();
    return list && list.length > 0 ? list[list.length - 1] : undefined;
  };
  const updated = () => entries()?.[0];

  const stamp = (entry: AuditEntry) =>
    t("audit.byAt", {
      who: entry.actor_email,
      age: formatRelativeAge(ageSeconds(entry.at_ms)),
    });

  return (
    <Show
      when={entries()}
      fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}
    >
      {(list) => (
        <Show
          when={list().length > 0}
          fallback={<p class="text-sm text-ink-muted">{t("audit.noHistory")}</p>}
        >
          <div class="flex flex-col gap-3">
            <Show when={props.entityId ? created() : undefined}>
              {(entry) => (
                <div class="flex flex-col gap-1 rounded-token border border-line px-3 py-2 text-sm">
                  <span class="text-ink-muted">
                    {t("audit.created")}: <span class="text-ink">{stamp(entry())}</span>
                  </span>
                  <Show when={updated() && updated()!.id !== entry().id}>
                    <span class="text-ink-muted">
                      {t("audit.updated")}: <span class="text-ink">{stamp(updated()!)}</span>
                    </span>
                  </Show>
                </div>
              )}
            </Show>
            <ul class="flex flex-col gap-2">
              <For each={list()}>
                {(entry) => (
                  <li class="flex flex-wrap items-center justify-between gap-2 border-b border-line pb-2 text-sm last:border-0">
                    <div class="flex flex-wrap items-center gap-2">
                      <StatusBadge tone="neutral" label={entry.action} />
                      <span class="text-ink">{entry.actor_email}</span>
                    </div>
                    <span
                      class="text-xs text-ink-muted"
                      title={new Date(entry.at_ms).toLocaleString(locale())}
                    >
                      {formatRelativeAge(ageSeconds(entry.at_ms))}
                    </span>
                  </li>
                )}
              </For>
            </ul>
          </div>
        </Show>
      )}
    </Show>
  );
}
