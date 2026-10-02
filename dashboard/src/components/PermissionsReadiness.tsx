// "Before you turn this on" (ADR-0158, Rollout): what each role held at one store does not grant.
//
// Shown wherever `permissions.enforced` is shown for one store — the Settings screen's switch for
// that store — whatever the switch says. Before it is on, this is what the owner checks; once it is
// on, it is what the till refuses. The read compares each role with the permission catalogue and
// nothing else: what anybody did at the till is never an input, because guessing what a person
// needs from what they did would be monitoring staff.
//
// Each role's gaps are grouped by risk, everyday ones first, each named in the console's words
// (`permission.<id>`) and by its id where this console has no words for it yet. The read names
// roles and counts people; no person is named here.

import { createEffect, createSignal, For, Match, on, Show, Switch } from "solid-js";

import { api } from "../api/client";
import type { MissingPermission, PermissionsReadiness, RoleReadiness } from "../api/types";
import { type MessageKey, t, tFromServer } from "../i18n";
import { LOADING, type Panel, panelOf } from "../lib/panel";
import { screenHref } from "../state/screens";
import { Banner, Skeleton } from "./ui";

/** The risk levels in the order a role's gaps are read, everyday first. */
const LEVELS: readonly { readonly risk: string; readonly label: MessageKey }[] = [
  { risk: "LOW", label: "readiness.risk.low" },
  { risk: "MEDIUM", label: "readiness.risk.medium" },
  { risk: "HIGH", label: "readiness.risk.high" },
];

/** A permission in the console's words, or its id where this console has none yet. */
const permissionName = (id: string) => tFromServer(`permission.${id}`, id);

/**
 * A role's gaps as lines of words, one per level that has any, everyday first. A level this console
 * does not know, from a newer cloud, goes last under "Other" rather than being dropped.
 */
function gapLines(missing: readonly MissingPermission[]): { label: string; names: string }[] {
  const known = new Set(LEVELS.map((level) => level.risk));
  const lines = LEVELS.map((level) => ({
    label: t(level.label),
    names: missing.filter((permission) => permission.risk === level.risk),
  }));
  lines.push({
    label: t("readiness.risk.other"),
    names: missing.filter((permission) => !known.has(permission.risk)),
  });
  return lines
    .filter((line) => line.names.length > 0)
    .map((line) => ({
      label: line.label,
      names: line.names.map((permission) => permissionName(permission.id)).join(", "),
    }));
}

/** The People screen with this role's editor open. */
function roleHref(tenant: string, role: RoleReadiness): string {
  return `${screenHref("people", tenant, "")}?role=${encodeURIComponent(role.role_template_id)}`;
}

/** What each role at `store` does not grant, read whenever the store changes. */
export function PermissionsReadinessPanel(props: { tenant: string; store: string }) {
  const [panel, setPanel] = createSignal<Panel<PermissionsReadiness> | null>(null);
  // Only the newest read may land: choosing two stores quickly must not show the first one's roles
  // under the second one's switch.
  let reads = 0;
  createEffect(
    on(
      () => [props.tenant, props.store] as const,
      ([tenant, store]) => {
        reads += 1;
        const mine = reads;
        if (!tenant || !store) {
          setPanel(null);
          return;
        }
        setPanel(LOADING);
        void panelOf(api.permissionsReadiness(tenant, store), (next) => {
          if (mine === reads) {
            setPanel(next);
          }
        });
      },
    ),
  );

  const ready = () => {
    const current = panel();
    return current?.state === "ready" ? current.value : null;
  };
  const failure = () => {
    const current = panel();
    return current?.state === "failed" ? current.message : "";
  };
  /** Whether some role lacks an everyday permission — the gap that stops a shift. */
  const everydayGap = (readiness: PermissionsReadiness) =>
    readiness.roles.some((role) => role.missing.some((permission) => permission.risk === "LOW"));

  return (
    <section
      class="flex flex-col gap-3 border-t border-line pt-3"
      aria-label={t("readiness.title")}
    >
      <h3 class="text-sm font-medium text-ink">{t("readiness.title")}</h3>
      <p class="text-sm text-ink-muted">{t("readiness.intro")}</p>
      <Switch>
        <Match when={panel()?.state === "loading"}>
          <Skeleton label={t("common.loading")} rows={2} />
        </Match>
        <Match when={failure()}>
          {(reason) => <Banner tone="danger" message={t("readiness.unread", { reason: reason() })} />}
        </Match>
        <Match when={ready()}>
          {(readiness) => (
            <div class="flex flex-col gap-3">
              <Show
                when={readiness().roles.length > 0}
                fallback={<p class="text-sm text-ink">{t("readiness.noRoles")}</p>}
              >
                <Show when={!everydayGap(readiness())}>
                  <p role="status" class="text-sm font-medium text-ink">
                    {t("readiness.everydayCovered")}
                  </p>
                </Show>
              </Show>
              <Show when={readiness().people_without_role > 0}>
                <p role="status" class="text-sm font-medium text-danger">
                  {t("readiness.peopleWithoutRole", { count: readiness().people_without_role })}
                </p>
              </Show>
              <ul class="flex flex-col gap-2">
                <For each={readiness().roles}>
                  {(role) => (
                    <li class="flex flex-col gap-1 rounded-token border border-line bg-surface-raised px-3 py-2">
                      <div class="flex flex-wrap items-baseline justify-between gap-2">
                        <p class="text-sm text-ink">
                          <span class="font-medium">{role.name}</span>
                          <span class="text-ink-muted">
                            {" · "}
                            {t("readiness.people", { count: role.people })}
                          </span>
                        </p>
                        <a class="text-sm text-accent underline" href={roleHref(props.tenant, role)}>
                          {t("readiness.editRole", { role: role.name })}
                        </a>
                      </div>
                      <Show
                        when={role.missing.length > 0}
                        fallback={
                          <p class="text-sm text-ink-muted">{t("readiness.grantsEverything")}</p>
                        }
                      >
                        <p class="text-sm text-ink-muted">{t("readiness.missing")}</p>
                        <For each={gapLines(role.missing)}>
                          {(line) => (
                            <p class="text-sm text-ink">
                              <span class="font-medium">{line.label}</span>
                              {`: ${line.names}`}
                            </p>
                          )}
                        </For>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </div>
          )}
        </Match>
      </Switch>
    </section>
  );
}
