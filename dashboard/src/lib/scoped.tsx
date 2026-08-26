// The working-context contract every scoped screen shares (F0).
//
// Most `/admin` screens need a tenant (and many a store) chosen in the top bar before their calls
// mean anything. Before F0 each screen guarded that inconsistently — some fired a request with an
// empty id and surfaced the raw `… is not a ULID` backend error, and every screen sat blank behind a
// manual "Load" button. These two helpers standardise both: `RequireContext` shows a friendly
// "choose it in the top bar" panel instead of failing, and `onScopedContext` loads on mount and
// whenever the context changes — but only once the scope is satisfied, so no empty-id request is
// ever sent.

import { createEffect, Show, type JSX } from "solid-js";

import { Card } from "../components/ui";
import { t } from "../i18n";
import { storeId, tenantId } from "../state/session";

/** What a screen needs in context: a tenant, or a tenant *and* a store. */
export type Scope = "tenant" | "store";

/** Whether the current context satisfies `scope` (a store always presupposes its tenant). */
export function contextReady(scope: Scope): boolean {
  return scope === "store" ? Boolean(tenantId() && storeId()) : Boolean(tenantId());
}

/**
 * Renders `children` only when the context the screen needs is set; otherwise a friendly panel
 * telling the operator to pick it in the top bar. This removes the whole `… is not a ULID` error
 * class at the UX layer — a scoped view is never rendered (and never fetches) without its context.
 */
export function RequireContext(props: { need: Scope; children: JSX.Element }): JSX.Element {
  return (
    <Show
      when={contextReady(props.need)}
      fallback={
        <Card title={t("context.chooseTitle")}>
          <p class="text-sm text-ink-muted">
            {props.need === "store" ? t("context.storeRequired") : t("context.tenantRequired")}
          </p>
        </Card>
      }
    >
      {props.children}
    </Show>
  );
}

/**
 * Runs `run` on mount and again whenever the working context changes — but only once `need` is
 * satisfied, so it never fires with an empty id. This is the auto-load that retires the manual
 * "Load" button: a screen shows its data as soon as it opens, and refreshes when the operator
 * switches tenant or store.
 */
export function onScopedContext(
  need: Scope,
  run: (tenantId: string, storeId: string) => void,
): void {
  createEffect(() => {
    const tid = tenantId();
    const sid = storeId();
    if (need === "store" ? tid && sid : tid) {
      run(tid, sid);
    }
  });
}
