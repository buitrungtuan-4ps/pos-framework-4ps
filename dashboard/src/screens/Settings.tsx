// Shared settings
// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
//
// Every value a store may run differently is a setting in one register, and this screen is drawn
// from that register (`GET /admin/settings/catalogue`) rather than written per setting. A setting
// added to the register appears here with nothing but its translations: its title and its one-line
// help, keyed `settings.item.<setting_key>.title` and `.help`, and each value's label, keyed
// `settings.value.<TOKEN>`. A key this console does not ship yet falls back to the raw key or token,
// as the integrations form does, so a register that has outrun the console still draws.
//
// # One value, many stores (decision 3)
//
// A value is written for every store, one brand, one store group or one store, and a store runs the
// value of the most specific of those that sets one. The scope picker says where the operator is
// writing; each setting then shows the value written there and, for one store, what that store
// actually runs and which level it comes from. A write publishes to every store it reaches in the
// same request and answers per store, so the screen lists each store's outcome and offers to
// publish again to the ones that failed. "Saved" alone would be the lie ADR-0122's batch report was
// written against.
//
// # Honour or hide (decision 5)
//
// A store on a release older than a setting's `since` ignores the value, so for one store the
// setting is hidden and one line says why. For a wider scope it stays, with a count of the stores it
// reaches that are too old to honour it. A store whose release is unknown is never treated as older
// (`lib/semver.ts`): it is shown, with a note. A setting honoured from `0.0.0`, which the register
// names for one the cloud applies itself, is honoured by every store whatever it runs, so it carries
// no note and names no release.
//
// # A new store's values (decision 1)
//
// The new-store wizard gives a store the owner's values as it creates it. If that failed, or the
// store's publish did, the store scope here does both again: one action writes the values the store
// does not set itself, and one publishes the store again.
//
// # Before you turn this on (ADR-0158, Rollout)
//
// For one store, `permissions.enforced` carries a panel that lists what each role held there does
// not grant (`components/PermissionsReadiness.tsx`), whatever the switch says. It reads the store's
// roles, so it needs `console.people.read`; a role without it is told who can see it.
//
// Nothing on this screen is personal data: setting values, store, brand and group names, the
// release each store runs, and role names with a count of the people holding each.

import { createEffect, createMemo, createSignal, For, Match, on, Show, Switch } from "solid-js";

import { ApiError, api } from "../api/client";
import type {
  EffectiveSetting,
  Json,
  SettingDefinition,
  SettingPresetsReport,
  SettingPublishOutcome,
  SettingPublishReport,
  SettingPublishResult,
  SettingScope,
} from "../api/types";
import { type MessageKey, t, tFromServer } from "../i18n";
import { useEntityCrud } from "../lib/entity-crud";
import { formatCount, formatInstant } from "../lib/format";
import { LOADING, type Panel, panelOf } from "../lib/panel";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { honouredByEveryRelease, releaseStanding } from "../lib/semver";
import { actingAdmin, storeId, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  ComboboxField,
  NumberField,
  PageHeader,
  SelectField,
  Skeleton,
  StatusBadge,
  SwitchField,
} from "../components/ui";
import { ConfirmDialog, EmptyState, TechnicalDetails } from "../components/kit";
import { PermissionsReadinessPanel } from "../components/PermissionsReadiness";
import { toast } from "../components/Toast";

const TENANT: SettingScope = "SETTING_SCOPE_TENANT";
const BRAND: SettingScope = "SETTING_SCOPE_BRAND";
const GROUP: SettingScope = "SETTING_SCOPE_STORE_GROUP";
const STORE: SettingScope = "SETTING_SCOPE_STORE";

/** The four scopes, widest first, as the picker offers them. */
const SCOPES: readonly { readonly scope: SettingScope; readonly label: MessageKey }[] = [
  { scope: TENANT, label: "settings.scope.tenant" },
  { scope: BRAND, label: "settings.scope.brand" },
  { scope: GROUP, label: "settings.scope.group" },
  { scope: STORE, label: "settings.scope.store" },
];

/** The kinds of value this console can edit; any other is shown and not offered. */
const CHOICE = "SETTING_KIND_CHOICE";
const INT = "SETTING_KIND_INT";
const BOOL = "SETTING_KIND_BOOL";

/**
 * What a whole number counts, in the operator's words: each unit's key takes the number as `count`.
 * A unit from a newer cloud is not here, and its number is shown bare rather than mislabelled.
 */
const UNIT_LABEL: Readonly<Record<string, MessageKey>> = {
  SETTING_UNIT_SECONDS: "settings.unit.seconds",
  SETTING_UNIT_MINUTES: "settings.unit.minutes",
  SETTING_UNIT_COUNT: "settings.unit.count",
  SETTING_UNIT_MINOR_UNITS: "settings.unit.minor_units",
  SETTING_UNIT_PERCENT: "settings.unit.percent",
  SETTING_UNIT_HOURS: "settings.unit.hours",
};

/**
 * The roles holding `console.config.publish`, which every write here needs because every write
 * publishes. Mirrored so the screen hides what a role cannot do; the server re-checks every route.
 */
const PUBLISHERS: ReadonlySet<string> = new Set(["owner", "admin", "ops"]);

/** The roles holding `console.people.read`, which the readiness panel's read needs (ADR-0158). */
const STAFF_READERS: ReadonlySet<string> = new Set(["owner", "admin"]);

/** The setting the readiness panel stands beside. */
const ENFORCED = "permissions.enforced";

/** How each outcome is drawn. `UNCHANGED` is neutral: the store already ran the value. */
const OUTCOME: Record<
  SettingPublishOutcome,
  { readonly label: MessageKey; readonly tone: "active" | "neutral" | "danger" }
> = {
  SETTING_PUBLISH_APPLIED: { label: "settings.outcome.applied", tone: "active" },
  SETTING_PUBLISH_UNCHANGED: { label: "settings.outcome.unchanged", tone: "neutral" },
  SETTING_PUBLISH_FAILED: { label: "settings.outcome.failed", tone: "danger" },
};

/**
 * Failures first, so the store that needs publishing again is the first one read; an outcome this
 * console does not know goes last.
 */
const OUTCOME_ORDER: readonly string[] = [
  "SETTING_PUBLISH_FAILED",
  "SETTING_PUBLISH_APPLIED",
  "SETTING_PUBLISH_UNCHANGED",
];

function outcomeRank(outcome: string): number {
  const rank = OUTCOME_ORDER.indexOf(outcome);
  return rank === -1 ? OUTCOME_ORDER.length : rank;
}

/**
 * What a report is about when it is not one setting: the store scope's two actions, the new-store
 * values and publishing the store again. A setting's key is always `node.field`, so this cannot
 * collide with one.
 */
const STORE_ACTION = "store";

/** What one write did, and where it is shown. */
interface Report {
  /**
   * The setting (or the store's actions) and the place it was written at, from `place`. A report
   * is shown only where its key matches, so a write that lands after the operator has moved on
   * cannot appear under another scope's card.
   */
  readonly key: string;
  readonly stores: readonly SettingPublishResult[];
  /** A sentence of its own, for the new-store values: how many it wrote. */
  readonly note?: string;
}

/** A setting's title: the console's words when it ships them, its key otherwise. */
function settingTitle(setting: SettingDefinition): string {
  return tFromServer(`settings.item.${setting.setting_key}.title`, setting.setting_key);
}

/** A setting's one-line explanation, or `""` when this console has none for it. */
function settingHelp(setting: SettingDefinition): string {
  return tFromServer(`settings.item.${setting.setting_key}.help`, "");
}

/**
 * A value's label, as its setting means it: a token in the console's words when it ships them (the
 * token otherwise), a whole number in its unit, a switch's value as On or Off.
 */
function valueLabel(setting: SettingDefinition, value: Json | undefined): string {
  if (typeof value === "string") {
    return tFromServer(`settings.value.${value}`, value);
  }
  if (typeof value === "number") {
    const unit = setting.unit === undefined ? undefined : UNIT_LABEL[setting.unit];
    return unit === undefined ? formatCount(value) : t(unit, { count: value });
  }
  if (typeof value === "boolean") {
    return value ? t("settings.on") : t("settings.off");
  }
  return value === undefined ? "" : JSON.stringify(value);
}

/** The bounds a whole number takes, when the catalogue gave both. */
function boundsOf(setting: SettingDefinition): { min: number; max: number } | null {
  return setting.kind === INT && typeof setting.min === "number" && typeof setting.max === "number"
    ? { min: setting.min, max: setting.max }
    : null;
}

/** Whether `value` is one `setting` takes — the register's rule, checked before the cloud does. */
function takes(setting: SettingDefinition, value: Json | undefined): boolean {
  switch (setting.kind) {
    case CHOICE:
      return typeof value === "string" && (setting.values ?? []).includes(value);
    case INT: {
      const bounds = boundsOf(setting);
      return (
        bounds !== null &&
        typeof value === "number" &&
        Number.isInteger(value) &&
        value >= bounds.min &&
        value <= bounds.max
      );
    }
    case BOOL:
      return typeof value === "boolean";
    default:
      return false;
  }
}

/** Whether this console can draw an editor for `setting`'s kind, with what the catalogue gave. */
function editable(setting: SettingDefinition): boolean {
  switch (setting.kind) {
    case CHOICE:
      return (setting.values ?? []).length > 0;
    case INT:
      return boundsOf(setting) !== null;
    case BOOL:
      return true;
    default:
      return false;
  }
}

/** Two ULIDs naming the same thing. They are case-insensitive, and a URL may carry either case. */
function sameId(left: string, right: string): boolean {
  return left.toUpperCase() === right.toUpperCase();
}

/**
 * A refusal in the operator's words, where the cloud named what was wrong; any other refusal passes
 * through as the server said it. Re-made as an `ApiError` so the lifecycle reads its message as-is.
 */
function explained(caught: unknown, clearing: boolean): unknown {
  if (!(caught instanceof ApiError)) {
    return caught;
  }
  const reasons = caught.details.map((detail) => detail.reason);
  let key: MessageKey | null = null;
  if (reasons.includes("SCOPE_NOT_ALLOWED")) {
    key = "settings.notAtScope";
  } else if (
    reasons.includes("INVALID_ENUM_VALUE") ||
    reasons.includes("OUT_OF_RANGE") ||
    reasons.includes("INVALID_VALUE")
  ) {
    key = "settings.refused.value";
  } else if (reasons.includes("UNKNOWN_SETTING")) {
    key = "settings.refused.unknownSetting";
  } else if (caught.status === 404) {
    key = clearing ? "settings.refused.nothingThere" : "settings.refused.notFound";
  } else if (caught.status === 409) {
    key = "settings.refused.archived";
  }
  return key === null ? caught : new ApiError(caught.status, t(key), caught.canonical, caught.details);
}

export function Settings() {
  // What the screen is drawn from, read together: the register, and the stores, brands and groups
  // a value can be written for. A register without them could not say what a value reaches.
  const layout = createAdminResource(
    async (tenant) => {
      const [catalogue, stores, brands, groups] = await Promise.all([
        api.settingsCatalogue(),
        api.listStores(tenant),
        api.listBrands(tenant),
        api.listStoreGroups(tenant),
      ]);
      return { catalogue, stores, brands, groups };
    },
    { scope: "tenant" },
  );
  // What the tenant has written. Its own read, because it is the one a write changes.
  const written = createAdminResource((tenant) => api.listSettingValues(tenant), {
    scope: "tenant",
  });
  // Which release each store runs. Its own read, so a fleet that cannot be read costs the release
  // notes and not the screen: every store then reads as unknown, which hides nothing.
  const fleet = createAdminResource((tenant) => api.listFleet(tenant), { scope: "tenant" });

  const [kind, setKind] = createSignal<SettingScope>(TENANT);
  // The brand, group or store the value is for; unused for every store, whose id is the tenant's.
  const [target, setTarget] = createSignal("");
  // A value picked and not yet saved, per setting: a token, a number (`null` for a number field left
  // empty) or a boolean.
  const [drafts, setDrafts] = createSignal<Readonly<Record<string, Json>>>({});
  const [report, setReport] = createSignal<Report | null>(null);
  // Which setting, or the store, at which place the last write was about: its refusal is shown there.
  const [subject, setSubject] = createSignal("");
  const [effective, setEffective] = createSignal<Panel<readonly EffectiveSetting[]> | null>(null);
  const [confirmingPresets, setConfirmingPresets] = createSignal(false);
  // One lifecycle for every write: a write publishes to every store it reaches, and two of them in
  // flight at once would race to the same stores.
  const writing = useEntityCrud<never>();

  const canWrite = () => {
    const role = actingAdmin()?.role;
    return role !== undefined && PUBLISHERS.has(role);
  };
  const readsStaff = () => {
    const role = actingAdmin()?.role;
    return role !== undefined && STAFF_READERS.has(role);
  };

  const catalogue = () => layout.value()?.catalogue ?? [];
  const stores = () => layout.value()?.stores ?? [];
  const brands = () => layout.value()?.brands ?? [];
  const groups = () => layout.value()?.groups ?? [];
  const storeName = (id: string) => stores().find((row) => sameId(row.store_id, id))?.name ?? id;
  const brandName = (id: string) => brands().find((row) => sameId(row.brand_id, id))?.name ?? id;
  const groupName = (id: string) => groups().find((row) => sameId(row.group_id, id))?.name ?? id;

  /** The id the value is written for: the tenant itself for every store. */
  const scopeId = () => (kind() === TENANT ? tenantId() : target());
  /** Whether the picker names a place: every store always does, the other three once one is chosen. */
  const chosen = () => kind() === TENANT || target() !== "";
  /** `about` — a setting's key, or the store's actions — at the place the picker names. */
  const place = (about: string) => `${about}@${kind()}:${scopeId().toUpperCase()}`;

  // A store chosen in the top bar is the store this screen opens on, so an operator who came to see
  // one shop sees what it runs; without one, it opens on every store. The context changing — a
  // shared link, the top bar — starts over from it.
  createEffect(
    on([tenantId, storeId], ([, store]) => {
      setKind(store ? STORE : TENANT);
      setTarget(store);
      setDrafts({});
      setReport(null);
      writing.close();
    }),
  );

  const chooseKind = (next: string) => {
    const scope = SCOPES.find((entry) => entry.scope === next)?.scope ?? TENANT;
    setKind(scope);
    setTarget(scope === STORE ? storeId() : "");
    setDrafts({});
    setReport(null);
    writing.close();
  };

  const chooseTarget = (next: string) => {
    setTarget(next);
    setDrafts({});
    setReport(null);
    writing.close();
  };

  /** The stores a value written here reaches — the same rule the cloud applies to a write. */
  const reached = createMemo(() => {
    const id = target();
    switch (kind()) {
      case TENANT:
        return stores();
      case BRAND:
        return id ? stores().filter((row) => row.brand_id !== null && sameId(row.brand_id, id)) : [];
      case GROUP: {
        const group = groups().find((row) => sameId(row.group_id, id));
        return group
          ? stores().filter((row) => group.store_ids.some((member) => sameId(member, row.store_id)))
          : [];
      }
      default:
        return id ? stores().filter((row) => sameId(row.store_id, id)) : [];
    }
  });

  /** The release each store last reported, by store. */
  const releases = createMemo(
    () => new Map((fleet.value() ?? []).map((row) => [row.store_id.toUpperCase(), row.installed_version])),
  );
  const releaseOf = (store: string) => releases().get(store.toUpperCase()) ?? null;

  /** How many of the stores reached are too old for `setting`, and how many cannot say. */
  const standingOf = (setting: SettingDefinition) => {
    let older = 0;
    let unknown = 0;
    for (const row of reached()) {
      const standing = releaseStanding(releaseOf(row.store_id), setting.since);
      if (standing === "older") {
        older += 1;
      } else if (standing === "unknown") {
        unknown += 1;
      }
    }
    return { total: reached().length, older, unknown };
  };

  /** Hidden for one store whose release is older than the one that honours it (decision 5). */
  const hiddenHere = (setting: SettingDefinition) =>
    kind() === STORE && chosen() && standingOf(setting).older > 0;

  /** The value written at the chosen scope, if there is one. */
  const writtenHere = (setting: SettingDefinition) =>
    (written.value() ?? []).find(
      (row) =>
        row.setting_key === setting.setting_key &&
        row.scope === kind() &&
        sameId(row.scope_id, scopeId()),
    );

  /** The settings with a new-store value that the chosen store does not set itself. */
  const pendingPresets = () =>
    kind() === STORE && target()
      ? catalogue().filter(
          (setting) =>
            setting.preset !== undefined &&
            !(written.value() ?? []).some(
              (row) =>
                row.setting_key === setting.setting_key &&
                row.scope === STORE &&
                sameId(row.scope_id, target()),
            ),
        )
      : [];

  // What one store runs. Read for the store the picker names, which need not be the one in the top
  // bar, so it follows the picker rather than the context. Only the newest read may land: choosing
  // two stores quickly must not leave the first one's values under the second one's name.
  let effectiveRead = 0;
  const loadEffective = () => {
    effectiveRead += 1;
    const mine = effectiveRead;
    const tenant = tenantId();
    const store = kind() === STORE ? target() : "";
    if (!tenant || !store) {
      setEffective(null);
      return;
    }
    setEffective(LOADING);
    void panelOf(api.effectiveSettings(tenant, store), (panel) => {
      if (mine === effectiveRead) {
        setEffective(panel);
      }
    });
  };
  createEffect(on([tenantId, kind, target], () => loadEffective()));

  const dropDraft = (key: string) =>
    setDrafts((current) => {
      const next = { ...current };
      delete next[key];
      return next;
    });

  /**
   * The one write path: run `call`, show what it did at every store under `key` (from `place`), and
   * re-read what is written. Re-read on a refusal too — a value somebody else cleared is a `404`
   * that has also changed what this screen should show.
   */
  const write = <R extends SettingPublishReport>(
    key: string,
    clearing: boolean,
    call: () => Promise<R>,
    done: (result: R) => void,
    note?: (result: R) => string | undefined,
  ) => {
    if (report()?.key !== key) {
      setReport(null);
    }
    setSubject(key);
    void writing
      .run(async () => {
        try {
          const result = await call();
          setReport({ key, stores: result.stores, note: note?.(result) });
          done(result);
        } catch (caught) {
          throw explained(caught, clearing);
        }
      })
      .then((ok) => {
        if (!ok) {
          toast.error(writing.error());
        }
        void written.refetch();
        loadEffective();
      });
  };

  const save = (setting: SettingDefinition) => {
    const value = drafts()[setting.setting_key];
    if (!takes(setting, value) || value === undefined) {
      return;
    }
    const scope = kind();
    const id = scopeId();
    write(
      place(setting.setting_key),
      false,
      () => api.putSetting(tenantId(), setting.setting_key, scope, id, value),
      () => {
        toast.ok(t("settings.saved", { setting: settingTitle(setting) }));
        dropDraft(setting.setting_key);
      },
    );
  };

  const clear = (setting: SettingDefinition) => {
    const scope = kind();
    const id = scopeId();
    write(
      place(setting.setting_key),
      true,
      () => api.clearSetting(tenantId(), setting.setting_key, scope, id),
      () => {
        toast.ok(t("settings.cleared", { setting: settingTitle(setting) }));
        dropDraft(setting.setting_key);
      },
    );
  };

  /** Publishes again to each store the last write failed at, one at a time, and folds the answers in. */
  const publishAgain = () => {
    const shown = report();
    if (!shown) {
      return;
    }
    const failed = shown.stores
      .filter((row) => row.outcome === "SETTING_PUBLISH_FAILED")
      .map((row) => row.store_id);
    if (failed.length === 0) {
      return;
    }
    write(
      shown.key,
      false,
      async () => {
        const again: SettingPublishResult[] = [];
        for (const store of failed) {
          again.push(...(await api.publishSettings(tenantId(), store)).stores);
        }
        return {
          stores: shown.stores.map(
            (row) => again.find((answer) => sameId(answer.store_id, row.store_id)) ?? row,
          ),
        };
      },
      () => toast.ok(t("settings.republished")),
      // The new-store count, when it was the presets that failed somewhere, still describes them.
      () => shown.note,
    );
  };

  /** Publishes the chosen store again — after a failed publish, or a move to another brand or group. */
  const republishStore = () => {
    const store = target();
    write(
      place(STORE_ACTION),
      false,
      () => api.publishSettings(tenantId(), store),
      () => toast.ok(t("settings.republished")),
    );
  };

  const applyPresets = () => {
    setConfirmingPresets(false);
    const store = target();
    write<SettingPresetsReport>(
      place(STORE_ACTION),
      false,
      () => api.applySettingPresets(tenantId(), store),
      (result) => toast.ok(t("settings.presetsApplied", { count: result.applied.length })),
      (result) => t("settings.presetsApplied", { count: result.applied.length }),
    );
  };

  /** What the value select is for, in words: every store, or the brand, group or store by name. */
  const targetName = () => {
    switch (kind()) {
      case TENANT:
        return t("settings.target.tenant");
      case BRAND:
        return brandName(target());
      case GROUP:
        return groupName(target());
      default:
        return storeName(target());
    }
  };

  /** What the chosen store runs for `setting`, and where it comes from, once that has been read. */
  const runs = (setting: SettingDefinition): string | null => {
    const panel = effective();
    if (panel === null || panel.state !== "ready") {
      return null;
    }
    const row = panel.value.find((entry) => entry.setting_key === setting.setting_key);
    const store = storeName(target());
    const value = valueLabel(setting, row?.value ?? setting.default);
    switch (row?.scope) {
      case undefined:
        return t("settings.runs.default", { store, value });
      case TENANT:
        return t("settings.runs.tenant", { store, value });
      case BRAND:
        return t("settings.runs.brand", { store, value, name: brandName(row.scope_id ?? "") });
      case GROUP:
        return t("settings.runs.group", { store, value, name: groupName(row.scope_id ?? "") });
      case STORE:
        return t("settings.runs.store", { store, value });
      default:
        return t("settings.runs.unknown", { store, value });
    }
  };

  /** What the releases of the stores reached say about `setting` at this scope; empty when nothing. */
  const releaseNotes = (setting: SettingDefinition): string[] => {
    const standing = standingOf(setting);
    if (kind() === STORE) {
      return standing.unknown > 0
        ? [t("settings.storeUnknown", { store: storeName(target()), since: setting.since })]
        : [];
    }
    const notes: string[] = [];
    if (standing.older > 0) {
      notes.push(
        t("settings.olderStores", {
          older: standing.older,
          total: standing.total,
          since: setting.since,
        }),
      );
    }
    if (standing.unknown > 0) {
      notes.push(t("settings.unknownStores", { unknown: standing.unknown, total: standing.total }));
    }
    return notes;
  };

  /** What the last write did at every store it reached, under the setting (or the store) it was about. */
  const ReportFor = (props: { about: string }) => (
    <>
      <Show when={subject() === place(props.about) && writing.error()}>
        {(message) => <Banner tone="danger" message={message()} />}
      </Show>
      <Show when={report()?.key === place(props.about) ? report() : null}>
        {(shown) => {
          const count = (outcome: SettingPublishOutcome) =>
            shown().stores.filter((row) => row.outcome === outcome).length;
          const ordered = () =>
            [...shown().stores].sort(
              (left, right) => outcomeRank(left.outcome) - outcomeRank(right.outcome),
            );
          return (
            <div class="flex flex-col gap-2 border-t border-line pt-3">
              <h3 class="text-sm font-medium text-ink">{t("settings.report")}</h3>
              <Show when={shown().note}>{(note) => <p class="text-sm text-ink">{note()}</p>}</Show>
              <Show
                when={shown().stores.length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("settings.reportEmpty")}</p>}
              >
                <p class="text-sm text-ink">
                  {t("settings.reportSummary", {
                    applied: count("SETTING_PUBLISH_APPLIED"),
                    unchanged: count("SETTING_PUBLISH_UNCHANGED"),
                    failed: count("SETTING_PUBLISH_FAILED"),
                  })}
                </p>
                <ul class="flex flex-col gap-1">
                  <For each={ordered()}>
                    {(row) => (
                      <li class="flex flex-wrap items-center gap-2 rounded-token border border-line bg-surface-raised px-3 py-2">
                        <StatusBadge
                          tone={OUTCOME[row.outcome]?.tone ?? "neutral"}
                          label={OUTCOME[row.outcome] ? t(OUTCOME[row.outcome].label) : row.outcome}
                        />
                        <span class="text-sm text-ink">{storeName(row.store_id)}</span>
                        <Show when={row.config_version_id}>
                          {(version) => (
                            <TechnicalDetails label={t("common.technicalDetails")}>
                              {version()}
                            </TechnicalDetails>
                          )}
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
              <Show when={canWrite() && count("SETTING_PUBLISH_FAILED") > 0}>
                <div class="flex flex-col gap-1">
                  <div>
                    <Button variant="secondary" disabled={writing.saving()} onClick={publishAgain}>
                      {t("settings.publishAgain")}
                    </Button>
                  </div>
                  <p class="text-sm text-ink-muted">{t("settings.publishAgainHint")}</p>
                </div>
              </Show>
            </div>
          );
        }}
      </Show>
    </>
  );

  /** A neutral notice: a fact the operator should know, not a failure. */
  const Notice = (props: { lines: readonly string[] }) => (
    <Show when={props.lines.length > 0}>
      <div
        role="status"
        class="flex flex-col gap-1 rounded-token border border-line bg-surface-raised px-3 py-2 text-sm text-ink"
      >
        <For each={props.lines}>{(line) => <p>{line}</p>}</For>
      </div>
    </Show>
  );

  /** What the chosen store runs for `setting`, as the node carries it, once that has been read. */
  const runsValue = (setting: SettingDefinition): Json | undefined => {
    const panel = effective();
    if (panel === null || panel.state !== "ready") {
      return undefined;
    }
    return panel.value.find((entry) => entry.setting_key === setting.setting_key)?.value;
  };

  /**
   * One setting, at the chosen scope, with the editor its kind draws: a select over a choice's
   * values, a number field bounded by a whole number's `min` and `max`, a switch for on or off.
   */
  const SettingCard = (props: { setting: SettingDefinition }) => {
    const setting = () => props.setting;
    const key = () => setting().setting_key;
    const here = () => writtenHere(setting());
    const hereValue = (): Json | undefined => here()?.value;
    const draft = (): Json | undefined => drafts()[key()];
    const pick = (value: Json) => setDrafts((current) => ({ ...current, [key()]: value }));
    /** A value picked that the setting takes and that is not already what is written here. */
    const changed = () => {
      const picked = draft();
      return picked !== undefined && takes(setting(), picked) && picked !== hereValue();
    };
    const settable = () => setting().scopes.includes(kind());
    const bounds = () => boundsOf(setting());
    const facts = () =>
      [
        t("settings.defaultIs", { value: valueLabel(setting(), setting().default) }),
        setting().preset === undefined
          ? null
          : t("settings.presetIs", { value: valueLabel(setting(), setting().preset) }),
        honouredByEveryRelease(setting().since)
          ? null
          : t("settings.sinceIs", { since: setting().since }),
      ]
        .filter((fact): fact is string => fact !== null)
        .join(" · ");
    const setOn = () => {
      const at = Date.parse(here()?.update_time ?? "");
      return Number.isFinite(at) ? t("settings.setOn", { when: formatInstant(at) }) : undefined;
    };
    /** A choice: the token picked, else the one written here, else none — "Not set here". */
    const choiceShown = () => {
      const picked = draft();
      if (typeof picked === "string") {
        return picked;
      }
      const written = hereValue();
      return typeof written === "string" ? written : "";
    };
    /** A whole number: the number picked, else the one written here, else an empty field. */
    const numberShown = (): number | null => {
      const picked = draft();
      if (picked !== undefined) {
        return typeof picked === "number" ? picked : null;
      }
      const written = hereValue();
      return typeof written === "number" ? written : null;
    };
    /** A number typed that the setting does not take, said beside the field before any write. */
    const numberRefused = () => {
      const picked = draft();
      return typeof picked === "number" && !takes(setting(), picked);
    };
    const rangeHint = () => {
      const limits = bounds();
      if (limits === null) {
        return undefined;
      }
      const range = t("settings.range", {
        min: valueLabel(setting(), limits.min),
        max: valueLabel(setting(), limits.max),
      });
      const written = setOn();
      return written === undefined ? range : `${range} ${written}`;
    };
    /**
     * A switch: the way it is picked, else the way it is written here — and with nothing written
     * here, the way that applies: what the chosen store runs, or the default. A switch has no
     * "not set" position, so the line under it says which of those it is showing.
     */
    const switchShown = (): boolean => {
      const picked = draft();
      if (typeof picked === "boolean") {
        return picked;
      }
      const written = hereValue();
      if (typeof written === "boolean") {
        return written;
      }
      const running = kind() === STORE ? runsValue(setting()) : undefined;
      if (typeof running === "boolean") {
        return running;
      }
      return setting().default === true;
    };
    const switchHint = () => {
      if (here()) {
        return setOn();
      }
      return kind() === STORE && typeof runsValue(setting()) === "boolean"
        ? t("settings.switchShowsRuns")
        : t("settings.switchShowsDefault");
    };
    return (
      <Card title={settingTitle(setting())}>
        <div class="flex max-w-xl flex-col gap-3">
          <Show when={settingHelp(setting())}>
            {(help) => <p class="text-sm text-ink">{help()}</p>}
          </Show>
          <p class="text-sm text-ink-muted">{facts()}</p>
          <Show when={kind() === STORE}>
            <Show
              when={effective()?.state !== "loading"}
              fallback={<Skeleton label={t("common.loading")} rows={1} />}
            >
              <Show when={runs(setting())}>
                {(line) => <p class="text-sm font-medium text-ink">{line()}</p>}
              </Show>
            </Show>
          </Show>
          <Notice lines={releaseNotes(setting())} />
          <Show
            when={settable()}
            fallback={<p class="text-sm text-ink-muted">{t("settings.notAtScope")}</p>}
          >
            <Show
              when={canWrite() && editable(setting())}
              fallback={
                <div class="flex flex-col gap-1">
                  <p class="text-sm text-ink">
                    {here()
                      ? t("settings.setHere", { value: valueLabel(setting(), hereValue()) })
                      : t("settings.notSet")}
                  </p>
                  <Show when={!editable(setting())}>
                    <p class="text-sm text-ink-muted">{t("settings.kindUnsupported")}</p>
                  </Show>
                </div>
              }
            >
              <Switch>
                <Match when={setting().kind === INT}>
                  <NumberField
                    label={t("settings.valueFor", { target: targetName() })}
                    value={numberShown()}
                    onChange={pick}
                    min={bounds()?.min}
                    max={bounds()?.max}
                    step={1}
                    placeholder={here() ? undefined : t("settings.notSet")}
                    disabled={writing.saving()}
                    hint={rangeHint()}
                  />
                  <Show when={numberRefused() ? bounds() : null}>
                    {(limits) => (
                      <p class="text-sm text-danger">
                        {t("settings.outOfRange", {
                          min: valueLabel(setting(), limits().min),
                          max: valueLabel(setting(), limits().max),
                        })}
                      </p>
                    )}
                  </Show>
                </Match>
                <Match when={setting().kind === BOOL}>
                  <SwitchField
                    label={t("settings.valueFor", { target: targetName() })}
                    checked={switchShown()}
                    onChange={pick}
                    onLabel={t("settings.on")}
                    offLabel={t("settings.off")}
                    disabled={writing.saving()}
                    hint={switchHint()}
                  />
                </Match>
                <Match when={setting().kind === CHOICE}>
                  <SelectField
                    label={t("settings.valueFor", { target: targetName() })}
                    value={choiceShown()}
                    options={(setting().values ?? []).map((value) => ({
                      value,
                      label: valueLabel(setting(), value),
                    }))}
                    placeholder={here() ? undefined : t("settings.notSet")}
                    onChange={pick}
                    disabled={writing.saving()}
                    hint={setOn()}
                  />
                </Match>
              </Switch>
              <div class="flex flex-wrap gap-2">
                <Button
                  disabled={writing.saving() || !changed()}
                  onClick={() => save(setting())}
                >
                  {t("settings.save")}
                </Button>
                <Show when={here()}>
                  <Button
                    variant="secondary"
                    disabled={writing.saving()}
                    onClick={() => clear(setting())}
                  >
                    {t("settings.clear")}
                  </Button>
                </Show>
              </div>
              <Show when={here()}>
                <p class="text-sm text-ink-muted">{t("settings.clearHint")}</p>
              </Show>
            </Show>
          </Show>
          <ReportFor about={setting().setting_key} />
          <Show when={key() === ENFORCED && kind() === STORE && target()}>
            {(store) => (
              <Show
                when={readsStaff()}
                fallback={<p class="text-sm text-ink-muted">{t("readiness.restricted")}</p>}
              >
                <PermissionsReadinessPanel tenant={tenantId()} store={store()} />
              </Show>
            )}
          </Show>
        </div>
      </Card>
    );
  };

  /** The settings hidden for the chosen store, each with the one line that says why. */
  const hiddenLines = () =>
    catalogue()
      .filter(hiddenHere)
      .map((setting) =>
        t("settings.hidden", {
          setting: settingTitle(setting),
          store: storeName(target()),
          installed: releaseOf(target()) ?? "",
          since: setting.since,
        }),
      );

  // The fleet read is waited for but not required: until it settles every store reads as unknown,
  // and a setting shown for a moment and then hidden from an old store is a flicker that says the
  // wrong thing first. Once it has failed, every store stays unknown, which hides nothing.
  const loaded = () =>
    layout.value() !== null && written.value() !== null && fleet.state().state !== "loading";
  const unreadable = () => failureOf(layout) || failureOf(written);
  /** Why what the chosen store runs could not be read, or `""`. */
  const effectiveFailure = () => {
    const panel = effective();
    return panel?.state === "failed" ? panel.message : "";
  };

  return (
    <div>
      <PageHeader title={t("settings.title")} description={t("settings.description")} />
      <RequireContext need="tenant">
        <Show when={unreadable()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show
          when={loaded()}
          fallback={
            <Show when={!unreadable()}>
              <Skeleton label={t("common.loading")} rows={4} />
            </Show>
          }
        >
          <div class="flex flex-col gap-4">
            <Card title={t("settings.scopeTitle")}>
              <div class="flex max-w-xl flex-col gap-4">
                <SelectField
                  label={t("settings.scopeKind")}
                  value={kind()}
                  options={SCOPES.map((entry) => ({ value: entry.scope, label: t(entry.label) }))}
                  onChange={chooseKind}
                  hint={t("settings.scopeHint")}
                  disabled={writing.saving()}
                />
                <Show when={kind() === BRAND}>
                  <ComboboxField
                    label={t("settings.brand")}
                    value={target()}
                    options={brands()
                      .filter((row) => row.status === "active" || row.brand_id === target())
                      .map((row) => ({ value: row.brand_id, label: row.name }))}
                    onChange={chooseTarget}
                    placeholder={t("settings.pickBrand")}
                    disabled={writing.saving()}
                    searchLabel={t("settings.searchBrands")}
                    emptyLabel={t("picker.noMatch")}
                  />
                </Show>
                <Show when={kind() === GROUP}>
                  <ComboboxField
                    label={t("settings.group")}
                    value={target()}
                    options={groups()
                      .filter((row) => row.status === "active" || row.group_id === target())
                      .map((row) => ({ value: row.group_id, label: row.name }))}
                    onChange={chooseTarget}
                    placeholder={t("settings.pickGroup")}
                    disabled={writing.saving()}
                    searchLabel={t("settings.searchGroups")}
                    emptyLabel={t("picker.noMatch")}
                  />
                </Show>
                <Show when={kind() === STORE}>
                  <ComboboxField
                    label={t("settings.store")}
                    value={target()}
                    options={stores()
                      .filter((row) => row.status === "active" || row.store_id === target())
                      .map((row) => ({
                        value: row.store_id,
                        label: row.name,
                        keywords: [row.store_id],
                      }))}
                    onChange={chooseTarget}
                    placeholder={t("settings.pickStore")}
                    disabled={writing.saving()}
                    searchLabel={t("settings.searchStores")}
                    emptyLabel={t("picker.noMatch")}
                  />
                </Show>
                <Show when={kind() !== STORE && chosen()}>
                  <p class="text-sm text-ink-muted">
                    {t("settings.reaches", { count: reached().length })}
                  </p>
                </Show>
                <Show when={failureOf(fleet)}>
                  {(reason) => (
                    <p class="text-sm text-ink-muted">
                      {t("settings.fleetUnread", { reason: reason() })}
                    </p>
                  )}
                </Show>
                <Show when={effectiveFailure()}>
                  {(reason) => (
                    <Banner
                      tone="danger"
                      message={t("settings.runsUnread", {
                        store: storeName(target()),
                        reason: reason(),
                      })}
                    />
                  )}
                </Show>
                <Show when={!canWrite()}>
                  <p class="text-sm text-ink-muted">{t("settings.readOnly")}</p>
                </Show>
                <Show when={canWrite() && kind() === STORE && target()}>
                  <div class="flex flex-col gap-3 border-t border-line pt-3">
                    <div class="flex flex-col gap-1">
                      <div>
                        <Button
                          variant="secondary"
                          disabled={writing.saving() || pendingPresets().length === 0}
                          onClick={() => setConfirmingPresets(true)}
                        >
                          {t("settings.presets")}
                        </Button>
                      </div>
                      <p class="text-sm text-ink-muted">
                        {pendingPresets().length === 0
                          ? t("settings.presetsNone")
                          : t("settings.presetsHint")}
                      </p>
                    </div>
                    <div class="flex flex-col gap-1">
                      <div>
                        <Button
                          variant="secondary"
                          disabled={writing.saving()}
                          onClick={republishStore}
                        >
                          {t("settings.republish")}
                        </Button>
                      </div>
                      <p class="text-sm text-ink-muted">{t("settings.republishHint")}</p>
                    </div>
                  </div>
                </Show>
                <ReportFor about={STORE_ACTION} />
              </div>
            </Card>

            <Notice lines={hiddenLines()} />

            <Show
              when={chosen()}
              fallback={<p class="text-sm text-ink-muted">{t("settings.chooseTarget")}</p>}
            >
              <Show
                when={catalogue().length > 0}
                fallback={
                  <EmptyState title={t("settings.empty")} description={t("settings.emptyHint")} />
                }
              >
                <For each={catalogue().filter((setting) => !hiddenHere(setting))}>
                  {(setting) => <SettingCard setting={setting} />}
                </For>
              </Show>
            </Show>
          </div>
        </Show>

        <ConfirmDialog
          open={confirmingPresets()}
          title={t("settings.presetsConfirmTitle", { store: storeName(target()) })}
          message={t("settings.presetsConfirmBody", {
            store: storeName(target()),
            list: pendingPresets()
              .map((setting) =>
                t("settings.pair", {
                  setting: settingTitle(setting),
                  value: valueLabel(setting, setting.preset),
                }),
              )
              .join("; "),
          })}
          confirmLabel={t("settings.presetsApply")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={writing.saving()}
          onConfirm={applyPresets}
          onCancel={() => setConfirmingPresets(false)}
        />
      </RequireContext>
    </div>
  );
}
