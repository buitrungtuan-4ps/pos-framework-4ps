// The settings history in words, with a rollback scoped by what and where (ADR-0246).
//
// Who changed which setting, from what to what, and where it applied, read from the audit trail.
// "Undo this change" puts one setting back at its place; "Roll back to before this" puts back every
// setting the chosen *what* takes (one setting, one group, or every setting) at the chosen *where*
// (every store, a brand, a store group or one store), and nothing else. Each value goes back through
// the ordinary setting write, so it is checked against the register now, published, and audited as
// a new change: a later publish keeps it.

import { createMemo, createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import type { Json, SettingScope } from "../api/types";
import { t, tFromServer } from "../i18n";
import { apiMessage } from "../lib/errors";
import { formatCount, formatInstant } from "../lib/format";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import {
  type Restore,
  type SettingChange,
  type What,
  type Where,
  at,
  changeOf,
  nodeOf,
  restoresFor,
  takes,
} from "../lib/settings-history";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, SelectField } from "../components/ui";
import { ConfirmDialog, EmptyState } from "../components/kit";
import { toast } from "../components/Toast";

/** A value in words: a token in the console's words, a switch as On or Off, "not set" for none. */
export function valueWords(value: Json | undefined): string {
  if (value === undefined || value === null) {
    return t("settingsHistory.notSet");
  }
  if (typeof value === "string") {
    return tFromServer(`settings.value.${value}`, value);
  }
  if (typeof value === "boolean") {
    return value ? t("settings.on") : t("settings.off");
  }
  return typeof value === "number" ? formatCount(value) : JSON.stringify(value);
}

function titleOf(key: string): string {
  return tFromServer(`settings.item.${key}.title`, key);
}

/** A `what` as the picker's token, and back. */
function whatOf(token: string): What {
  if (token.startsWith("group:")) {
    return { kind: "group", node: token.slice(6) };
  }
  return token.startsWith("setting:") ? { kind: "setting", key: token.slice(8) } : { kind: "all" };
}

/** The tenant's setting changes, and every place a value is written for, by name. */
async function loadHistory(tenant: string) {
  const [entries, stores, brands, groups] = await Promise.all([
    api.listAudit({ tenantId: tenant, entityType: "setting", limit: 500 }),
    api.listStores(tenant),
    api.listBrands(tenant),
    api.listStoreGroups(tenant),
  ]);
  const changes = entries.map(changeOf).filter((change): change is SettingChange => change !== null);
  const places = new Map<string, string>([
    [`SETTING_SCOPE_TENANT:${tenant.toUpperCase()}`, t("settingsHistory.everyStore")],
  ]);
  for (const brand of brands)
    places.set(`SETTING_SCOPE_BRAND:${brand.brand_id.toUpperCase()}`, t("settingsHistory.brand", { name: brand.name }));
  for (const group of groups)
    places.set(
      `SETTING_SCOPE_STORE_GROUP:${group.group_id.toUpperCase()}`,
      t("settingsHistory.group", { name: group.name }),
    );
  for (const store of stores)
    places.set(`SETTING_SCOPE_STORE:${store.store_id.toUpperCase()}`, t("settingsHistory.store", { name: store.name }));
  return { changes, places };
}

/** One change in words, with its two ways back. */
function HistoryRow(props: {
  change: SettingChange;
  where: string;
  busy: boolean;
  onUndo: () => void;
  onRollBack: () => void;
}) {
  return (
    <li class="rounded-token border border-line p-3">
      <p class="text-sm" data-testid="history-words">
        {t("settingsHistory.words", {
          who: props.change.actorEmail,
          setting: titleOf(props.change.key),
          before: valueWords(props.change.before),
          after: valueWords(props.change.after),
          where: props.where,
        })}
      </p>
      <p class="text-xs text-ink-muted">{formatInstant(props.change.atMs)}</p>
      <div class="mt-2 flex flex-wrap gap-2">
        <Button variant="secondary" onClick={() => props.onUndo()} disabled={props.busy}>
          {t("settingsHistory.undo")}
        </Button>
        <Button variant="secondary" onClick={() => props.onRollBack()} disabled={props.busy}>
          {t("settingsHistory.rollBack")}
        </Button>
      </div>
    </li>
  );
}

/** What a rollback will write, in words, for the confirmation. */
function confirmMessage(pending: { where: string; restores: readonly Restore[] } | null): string {
  if (!pending || pending.restores.length === 0) {
    return t("settingsHistory.confirmNothing");
  }
  const list = pending.restores.map((restore) => `${titleOf(restore.key)} → ${valueWords(restore.value)}`).join("; ");
  return t("settingsHistory.confirmList", { where: pending.where, list });
}

/** The picker's choices for *what*: every setting, each group, each setting changed. */
function whatOptionsOf(changes: readonly SettingChange[]) {
  const keys = [...new Set(changes.map((change) => change.key))].sort();
  return [
    { value: "all", label: t("settingsHistory.whatAll") },
    ...[...new Set(keys.map(nodeOf))].map((node) => ({
      value: `group:${node}`,
      label: t("settingsHistory.whatGroup", { group: node }),
    })),
    ...keys.map((key) => ({ value: `setting:${key}`, label: titleOf(key) })),
  ];
}

/** Writes each restore through the ordinary setting write; answers the refusals, by setting. */
async function applyRestores(restores: readonly Restore[]): Promise<string[]> {
  const failed: string[] = [];
  for (const restore of restores) {
    try {
      await (restore.value === undefined
        ? api.clearSetting(tenantId(), restore.key, restore.scope, restore.scopeId)
        : api.putSetting(tenantId(), restore.key, restore.scope, restore.scopeId, restore.value));
    } catch (caught) {
      failed.push(`${titleOf(restore.key)}: ${apiMessage(caught)}`);
    }
  }
  return failed;
}

function Filters(props: {
  places: ReadonlyMap<string, string>;
  place: string;
  onPlace: (value: string) => void;
  what: string;
  whatOptions: readonly { value: string; label: string }[];
  onWhat: (value: string) => void;
}) {
  return (
    <Card title={t("settingsHistory.filterTitle")}>
      <div class="grid max-w-xl gap-3">
        <SelectField
          label={t("settingsHistory.where")}
          value={props.place}
          options={[
            { value: "", label: t("settingsHistory.everywhere") },
            ...[...props.places].map(([value, label]) => ({ value, label })),
          ]}
          onChange={props.onPlace}
        />
        <SelectField
          label={t("settingsHistory.what")}
          value={props.what}
          options={props.whatOptions}
          onChange={props.onWhat}
          hint={t("settingsHistory.whatHint")}
        />
      </div>
    </Card>
  );
}

type Pending = { readonly from: SettingChange; readonly restores: Restore[] };

export function SettingsHistory() {
  const read = createAdminResource(loadHistory, { scope: "tenant" });
  const [place, setPlace] = createSignal("");
  const [what, setWhat] = createSignal("all");
  const [pending, setPending] = createSignal<Pending | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [refused, setRefused] = createSignal<string[]>([]);
  const changes = () => read.value()?.changes ?? [];
  const placeName = (scope: SettingScope, scopeId: string) =>
    read.value()?.places.get(`${scope}:${scopeId.toUpperCase()}`) ?? scopeId;
  const shown = createMemo(() => {
    const [scope, scopeId] = place().split(":");
    const chosen: Where | null = scope && scopeId ? { scope: scope as SettingScope, scopeId } : null;
    return changes()
      .filter((change) => (chosen === null || at(chosen, change)) && takes(whatOf(what()), change))
      .sort((a, b) => b.atMs - a.atMs);
  });
  const ask = (from: SettingChange, scoped: boolean) => {
    const chosen: What = scoped ? whatOf(what()) : { kind: "setting", key: from.key };
    setRefused([]);
    setPending({ from, restores: restoresFor(changes(), from, chosen, { scope: from.scope, scopeId: from.scopeId }) });
  };
  const apply = async (restores: readonly Restore[]) => {
    setBusy(true);
    const failed = await applyRestores(restores);
    setBusy(false);
    setPending(null);
    setRefused(failed);
    toast.ok(t("settingsHistory.rolledBack", { count: restores.length - failed.length }));
    await read.refetch();
  };
  const confirming = () => {
    const now = pending();
    return now && { where: placeName(now.from.scope, now.from.scopeId), restores: now.restores };
  };

  return (
    <>
      <PageHeader title={t("settingsHistory.title")} description={t("settingsHistory.description")} />
      <RequireContext need="tenant">
        <Filters
          places={read.value()?.places ?? new Map<string, string>()}
          place={place()}
          onPlace={setPlace}
          what={what()}
          whatOptions={whatOptionsOf(changes())}
          onWhat={setWhat}
        />
        <Show when={failureOf(read)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show when={refused().length > 0}>
          <Banner tone="danger" message={t("settingsHistory.refused", { list: refused().join("; ") })} />
        </Show>
        <Show when={shown().length > 0} fallback={<EmptyState title={t("settingsHistory.empty")} />}>
          <ol class="mt-4 flex flex-col gap-2" data-testid="settings-history">
            <For each={shown()}>
              {(change) => (
                <HistoryRow
                  change={change}
                  where={placeName(change.scope, change.scopeId)}
                  busy={busy()}
                  onUndo={() => ask(change, false)}
                  onRollBack={() => ask(change, true)}
                />
              )}
            </For>
          </ol>
        </Show>
        <ConfirmDialog
          open={pending() !== null}
          title={t("settingsHistory.confirmTitle")}
          message={confirmMessage(confirming() ?? null)}
          confirmLabel={t("settingsHistory.confirm")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={busy()}
          onConfirm={() => void apply(pending()?.restores ?? [])}
          onCancel={() => setPending(null)}
        />
      </RequireContext>
    </>
  );
}
