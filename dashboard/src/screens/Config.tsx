// The config-tree editor (ADR-0033, ADR-0060) — the missing half of config delivery: publish one
// level of a store's tree, and the store pulls it (ADR-0039). Left: the current effective (composed,
// validated) config for the tenant/store in context. Right: author one level as JSON and publish;
// the server composes + validates and either appends a new version or rejects with the violations,
// keeping the last good version current.
//
// The capability form offers every flag in the catalogue but QR ordering's, which is one switch with
// `qr.enabled` and the QR channel following it (ADR-0160 decision 5) and is offered on Channels &
// payments, beside its guardrails. Left out of the form, it is also left out of what a preset sets:
// a preset names no QR flag, so applying one here would have switched a store's QR ordering off.
//
// Nor does the form offer a flag no release reads, which the catalogue marks `offered: false`
// (ADR-0160 decision 5: every switch the console offers changes behaviour at the edge). Left out of
// the form, such a flag is left out of what a preset sets and of what a publish sends, so a store's
// stored value stays as it is. A store that has one on is told so in one muted line. One exception
// keeps table service reachable: table service excludes pay-first (§10), so a publish that leaves
// tables on for a store whose stored pay-first is on turns pay-first off too, and the line says so.
// A preset that turns on none of the offered switches, as Retail names only barcode entry, is not
// offered either: it would only turn every switch off.

import { createEffect, createMemo, createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import {
  CONFIG_LEVELS,
  QR_ORDERING_SWITCH,
  type CapabilityCatalogue,
  type CapabilityFlag,
  type CapabilityPreset,
  type ConfigLevel,
  type ConfigVersion,
  type Json,
} from "../api/types";
import { type MessageKey, t } from "../i18n";

// The level names are user-visible, so each maps to a static i18n key (a template-literal key would
// not be a MessageKey and would defeat the type check).
const LEVEL_KEY: Record<ConfigLevel, MessageKey> = {
  tenant: "config.level.tenant",
  brand: "config.level.brand",
  store: "config.level.store",
  device: "config.level.device",
};
import { formatInstant } from "../lib/format";
import { RequireContext } from "../lib/scoped";
import { createAdminResource, failureOf } from "../lib/resource";
import { usePublishedNodes } from "../lib/published";
import { actingAdmin, storeId, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  PageHeader,
  SelectField,
  StatusBadge,
  TextArea,
} from "../components/ui";
import { ConfirmDialog, EmptyState, PublishBar } from "../components/kit";
import { describePublish, previewNode } from "../lib/publish-copy";
import { toast } from "../components/Toast";
import { apiMessage, isStale } from "../lib/errors";

// The three §10 presets the catalogue serves, each to a static i18n label (a template-literal key
// would not be a MessageKey). An id the map does not cover falls back to its raw server id.
const PRESET_KEY: Record<string, MessageKey> = {
  full_service: "config.capabilities.preset.full_service",
  counter: "config.capabilities.preset.counter",
  retail: "config.capabilities.preset.retail",
};

// The two flags of the one rule a hidden switch takes part in: table service excludes pay-first.
const PAY_FIRST = "pay_first_enabled";
const TABLES = "tables_enabled";

// A client-side mirror of the §10 inter-flag rules (`pos-core`'s `RULES`), keyed by rule id, so the
// editor can preview a conflict the instant a toggle creates it. This is a UX convenience only — the
// server re-runs the real `conflicts` on publish and returns a 422, so it stays authoritative. A rule
// the server serves but this map does not know is treated as satisfied here (never a false block) and
// is still enforced on publish.
const CONFLICT_CHECKS: Record<string, (on: (key: string) => boolean) => boolean> = {
  // `pay_first_enabled` is not offered (ADR-0160 decision 5), so the form never holds it and this
  // never fires here: a publish that turns tables on clears a stored pay-first instead (see
  // `clearsPayFirst`). The cloud still applies the rule on publish.
  "pay_first.excludes.tables": (on) => !(on("pay_first_enabled") && on("tables_enabled")),
  "seats.requires.tables": (on) => !on("seats_enabled") || on("tables_enabled"),
};

export function Config() {

  // Whether this admin's role carries `console.reports.revenue`. Prices are T2, so the server strips
  // the priced nodes — the menu price book and the campaign amounts — from the effective document for
  // a role without it (production-readiness S8). Mirrored here only to *say so*: an operator who sees
  // a document with no `menu` key must not have to work out whether the store has no menu published
  // or whether their role hid it. The server is the authority; this never unlocks anything.
  const readsPrices = () => {
    const role = actingAdmin()?.role;
    return role === "owner" || role === "admin";
  };
  // Whether this admin's role carries `console.people.read`. A staff member's name and code are T1,
  // so the server takes both out of each member of the `permissions` node for a role without it
  // (ADR-0158). Mirrored for the same reason as prices: to say so, not to unlock anything.
  const readsStaff = () => {
    const role = actingAdmin()?.role;
    return role === "owner" || role === "admin";
  };
  const [level, setLevel] = createSignal<ConfigLevel>("store");
  const [document, setDocument] = createSignal("{\n}\n");
  const [error, setError] = createSignal("");
  const [ok, setOk] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [viewing, setViewing] = createSignal<string | null>(null);
  const [viewingDoc, setViewingDoc] = createSignal<Json | null>(null);
  const [compare, setCompare] = createSignal(false);
  const [rollbackTo, setRollbackTo] = createSignal<string | null>(null);
  const [flags, setFlags] = createSignal<Record<string, boolean>>({});
  const [capError, setCapError] = createSignal("");
  const [capOk, setCapOk] = createSignal("");

  // The §10 catalogue is static platform data, so it is read once per session rather than on every
  // re-read of the tree. Kept in a plain variable because nothing renders from it directly — it is
  // handed back inside the resource's value, which is what the screen reads.
  let catalogueCache: CapabilityCatalogue | null = null;

  /**
   * What this screen opens on: the store's effective (composed) config and its version history.
   *
   * One resource, because the two are read together and the second is a precondition for writing
   * the first — a screen that had the document but not the version it was read at could offer a
   * publish it has no safe way to make.
   *
   * The catalogue rides along but tolerates its own failure. It is static data, and a screen that
   * refused to draw the effective document because a *labels* read failed would be worse than one
   * whose toggles are missing while the JSON editor still works.
   */
  const read = createAdminResource(
    async (tenant, store) => {
      if (!store) {
        return null;
      }
      const [effective, versions] = await Promise.all([
        api.effectiveConfig(tenant, store),
        api.configVersions(tenant, store),
      ]);
      if (catalogueCache === null) {
        try {
          catalogueCache = await api.capabilityCatalogue();
        } catch {
          // Non-fatal: the toggles stay unseeded, the rest of the screen is unaffected.
        }
      }
      return { effective, versions, catalogue: catalogueCache };
    },
    { scope: "tenant" },
  );

  const effective = () => read.value()?.effective ?? null;
  const versions = (): ConfigVersion[] => read.value()?.versions ?? [];
  const catalogue = () => read.value()?.catalogue ?? null;
  /**
   * The flags the capability form offers: every one a release reads, but QR ordering's switch, which
   * Channels & payments offers.
   */
  const formFlags = (): readonly CapabilityFlag[] =>
    (catalogue()?.flags ?? []).filter(
      (flag) => flag.key !== QR_ORDERING_SWITCH && flag.offered !== false,
    );
  /**
   * The presets the form offers: those that turn on at least one switch the form offers. A preset that
   * names none, as Retail names only barcode entry, would only turn every switch off.
   *
   * Memoized using createMemo to avoid allocating a new Set and re-filtering flags for every preset
   * on every render / evaluation cycle.
   */
  const offeredPresets = createMemo(() => {
    const offered = new Set(formFlags().map((flag) => flag.key));
    return (catalogue()?.presets ?? []).filter((preset) =>
      preset.keys.some((key) => offered.has(key)),
    );
  });
  /**
   * Whether a publish now also turns the store's stored pay-first off.
   *
   * Table service excludes pay-first (§10's `pay_first.excludes.tables`), and the form no longer
   * offers pay-first. So a store that has it on, from a Counter preset, could not otherwise turn table
   * service back on from here: the cloud would refuse the publish and nothing would clear the flag.
   * Nothing reads the flag, so turning it off changes nothing else. Only in this case: an unread flag
   * stays unwritten otherwise, and once a release offers pay-first again the form holds it itself.
   */
  const clearsPayFirst = (): boolean => {
    const payFirst = catalogue()?.flags.find((flag) => flag.key === PAY_FIRST);
    return (
      payFirst !== undefined &&
      payFirst.offered === false &&
      flagInEffective(PAY_FIRST, payFirst.default_on) &&
      flags()[TABLES] === true
    );
  };
  /** What a publish sends: the form's switches, and pay-first off where `clearsPayFirst` says so. */
  const publishedFlags = (): Record<string, boolean> =>
    clearsPayFirst() ? { ...flags(), [PAY_FIRST]: false } : flags();
  /**
   * The flags no release reads that this store has on: set before the console stopped offering them,
   * usually by a preset that named them. Said in one muted line, so an operator who set one is not
   * left looking for it. Pay-first is said differently while a publish would turn it off.
   */
  const unusedFlagsOn = (): string[] =>
    (catalogue()?.flags ?? [])
      .filter((flag) => flag.offered === false && flagInEffective(flag.key, flag.default_on))
      .filter((flag) => !(flag.key === PAY_FIRST && clearsPayFirst()))
      .map((flag) => flag.key);

  /**
   * When the capability flags last reached this store.
   *
   * Capability flags are published as **top-level keys**, one node per flag, not as a single
   * `capabilities` node — so the honest answer is the newest of them. A flag the operator has never
   * touched contributes nothing, which is right: it has never been published.
   */
  const published = usePublishedNodes();
  const capabilitiesPublishedAtMs = (): number | null => {
    const keys = new Set(formFlags().map((flag) => flag.key));
    let newest: number | null = null;
    for (const row of published.nodes()) {
      if (keys.has(row.node) && row.at_ms !== null && (newest === null || row.at_ms > newest)) {
        newest = row.at_ms;
      }
    }
    return newest;
  };

  // Read a top-level boolean flag from the current effective (composed) config, falling to the flag's
  // declared default when the document does not name it — the same "unnamed falls to default" contract
  // the edge's `from_flags` reader keeps (ADR-0071).
  const flagInEffective = (key: string, defaultOn: boolean): boolean => {
    const doc = effective();
    if (doc !== null && typeof doc === "object" && !Array.isArray(doc)) {
      const value = (doc as { [key: string]: Json })[key];
      if (typeof value === "boolean") {
        return value;
      }
    }
    return defaultOn;
  };

  /**
   * Seed the toggles from the store's current effective profile, every time the read lands.
   *
   * Re-seeding on every read — a store change, a publish — keeps the toggles showing the live
   * baseline rather than what the last store happened to have on.
   */
  createEffect(() => {
    const value = read.value();
    if (value === null || value.catalogue === null) {
      return;
    }
    const seeded: Record<string, boolean> = {};
    for (const flag of formFlags()) {
      seeded[flag.key] = flagInEffective(flag.key, flag.default_on);
    }
    setFlags(seeded);
    setCapError("");
    setCapOk("");
  });

  // The precondition the two authored writes carry (ADR-0095): the version this screen last read the
  // tree at, or `null` for a store that has never been published to. A read that failed leaves the
  // list empty, so this reads `null` and the server refuses the write — the safe way round, because
  // the alternative is publishing over a version we never saw.
  const currentVersion = () => versions().find((version) => version.current)?.version_id ?? null;

  // A `412` means somebody published while this editor was open (ADR-0095). Reload and say so rather
  // than offering a retry: retrying would re-apply the overwrite the refusal exists to prevent, and
  // the operator needs to read what changed before deciding again.
  const fail = async (caught: unknown) => {
    if (isStale(caught)) {
      const message = t("config.stale");
      setError(message);
      toast.error(message);
      await reload();
      return;
    }
    const message = apiMessage(caught);
    setError(message);
    toast.error(message);
  };

  /**
   * Re-read the tree, and drop whatever version was being viewed.
   *
   * What a write calls when it succeeds. The viewed version is cleared because it is a snapshot of
   * a history the write just added to, and leaving it on screen beside a fresh document is how a
   * diff comes to compare two things that are no longer adjacent.
   */
  const reload = async () => {
    setError("");
    setOk("");
    setViewing(null);
    setViewingDoc(null);
    await Promise.all([read.refetch(), published.refresh()]);
  };

  const viewVersion = async (versionId: string) => {
    setError("");
    try {
      setViewingDoc(await api.configVersionEffective(tenantId(), storeId(), versionId));
      setViewing(versionId);
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    }
  };

  const rollback = async () => {
    const versionId = rollbackTo();
    setRollbackTo(null);
    if (versionId === null) {
      return;
    }
    setBusy(true);
    try {
      const result = await api.rollbackConfig(
        tenantId(),
        storeId(),
        versionId,
        currentVersion(),
      );
      toast.ok(t("config.rolledBack", { version: result.config_version_id }));
      await reload();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const versionTime = (version: ConfigVersion) => formatInstant(version.at_ms);

  // A crude but honest line diff for stable-key config JSON: pretty-print both and mark each line of
  // `doc` that differs from `other` at the same index. Enough to show what a version changed.
  const diffLines = (doc: Json | null, other: Json | null) => {
    const otherLines = JSON.stringify(other, null, 2).split("\n");
    return JSON.stringify(doc, null, 2)
      .split("\n")
      .map((line, index) => ({ line, changed: line !== (otherLines[index] ?? "") }));
  };

  // Load on open and whenever the tenant/store changes — never with an empty context (F0).

  const toggleFlag = (key: string, value: boolean) => {
    setFlags((prev) => ({ ...prev, [key]: value }));
    setCapOk("");
    setCapError("");
  };

  // A preset sets every flag: on for the keys it names, off for the rest.
  const applyPreset = (preset: CapabilityPreset) => {
    const on = new Set(preset.keys);
    const next: Record<string, boolean> = {};
    for (const flag of formFlags()) {
      next[flag.key] = on.has(flag.key);
    }
    setFlags(next);
    setCapOk("");
    setCapError("");
  };

  // The §10 rules the working toggle state violates — the inline preview. Server-served descriptions,
  // client-mirrored checks (see CONFLICT_CHECKS).
  const violatedRules = () => {
    const current = flags();
    const isOn = (key: string) => current[key] ?? false;
    return (catalogue()?.rules ?? []).filter((rule) => {
      const check = CONFLICT_CHECKS[rule.id];
      return check ? !check(isOn) : false;
    });
  };

  // The flags whose working value differs from the store's current effective profile — the
  // diff-before-publish, so the operator sees exactly what a publish will change.
  const flagChanges = () => [
    ...formFlags()
      .map((flag) => {
        const before = flagInEffective(flag.key, flag.default_on);
        const after = flags()[flag.key] ?? flag.default_on;
        return { key: flag.key, before, after };
      })
      .filter((row) => row.before !== row.after),
    ...(clearsPayFirst() ? [{ key: PAY_FIRST, before: true, after: false }] : []),
  ];

  const publishCapabilities = async () => {
    setCapError("");
    setCapOk("");
    setBusy(true);
    try {
      const result = await api.publishCapabilities(tenantId(), storeId(), publishedFlags());
      const message = t("config.capabilities.published", { version: result.config_version_id });
      setCapOk(message);
      toast.ok(message);
      await reload();
    } catch (caught) {
      const message = apiMessage(caught);
      setCapError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const publish = async () => {
    setError("");
    setOk("");
    let parsed: Json;
    try {
      parsed = JSON.parse(document()) as Json;
    } catch {
      setError(t("config.invalidJson"));
      return;
    }
    setBusy(true);
    try {
      const result = await api.publishConfig(
        tenantId(),
        storeId(),
        level(),
        parsed,
        currentVersion(),
      );
      setOk(t("config.published", { version: result.config_version_id }));
      await reload();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("config.title")} description={t("config.description")} />
      <RequireContext need="store">
        <Show when={catalogue()}>
          {(_cat) => (
            <div class="mb-6">
              <Card
                title={t("config.capabilities.title")}
                actions={
                  <div class="flex flex-wrap items-center gap-2">
                    <span class="text-sm text-ink-muted">{t("config.capabilities.presets")}</span>
                    <For each={offeredPresets()}>
                      {(preset) => {
                        const key = PRESET_KEY[preset.id];
                        return (
                          <Button
                            data-step="applyPreset"
                            variant="secondary"
                            disabled={busy()}
                            onClick={() => applyPreset(preset)}
                          >
                            {key ? t(key) : preset.id}
                          </Button>
                        );
                      }}
                    </For>
                  </div>
                }
              >
                <div class="flex flex-col gap-4">
                  <p class="text-sm text-ink-muted">{t("config.capabilities.hint")}</p>
                  <p class="text-sm text-ink-muted">{t("config.capabilities.qrElsewhere")}</p>
                  <div class="grid gap-3 sm:grid-cols-2">
                    <For each={formFlags()}>
                      {(flag) => (
                        <CheckboxField
                          label={flag.key}
                          checked={flags()[flag.key] ?? flag.default_on}
                          onChange={(on) => toggleFlag(flag.key, on)}
                          class="rounded-token border border-line px-3 py-2"
                          caption={
                            <span class="flex flex-col gap-1">
                              <span class="flex flex-wrap items-center gap-2">
                                <code class="text-sm font-medium text-ink">{flag.key}</code>
                                <Show when={flag.default_on}>
                                  <StatusBadge
                                    tone="active"
                                    label={t("config.capabilities.default")}
                                  />
                                </Show>
                              </span>
                              <span class="text-xs text-ink-muted">{flag.description}</span>
                            </span>
                          }
                        />
                      )}
                    </For>
                  </div>
                  <Show when={unusedFlagsOn().length > 0 || clearsPayFirst()}>
                    <p class="text-sm text-ink-muted">
                      {[
                        ...(unusedFlagsOn().length > 0
                          ? [
                              t("config.capabilities.setButUnused", {
                                keys: unusedFlagsOn().join(", "),
                              }),
                            ]
                          : []),
                        ...(clearsPayFirst() ? [t("config.capabilities.payFirstCleared")] : []),
                      ].join(" ")}
                    </p>
                  </Show>

                  <Show when={violatedRules().length > 0}>
                    <Banner
                      tone="danger"
                      message={t("config.capabilities.conflicts")}
                    />
                    <ul class="ml-4 list-disc text-sm text-danger">
                      <For each={violatedRules()}>{(rule) => <li>{rule.description}</li>}</For>
                    </ul>
                  </Show>

                  <div>
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("config.capabilities.changes")}
                    </span>
                    <Show
                      when={flagChanges().length > 0}
                      fallback={
                        <p class="text-sm text-ink-muted">{t("config.capabilities.noChanges")}</p>
                      }
                    >
                      <ul class="flex flex-col gap-1">
                        <For each={flagChanges()}>
                          {(change) => (
                            <li class="flex flex-wrap items-center gap-2 text-sm text-ink">
                              <code class="text-ink">{change.key}</code>
                              <span class="text-ink-muted">
                                {change.before
                                  ? t("config.capabilities.on")
                                  : t("config.capabilities.off")}
                                {" → "}
                                {change.after
                                  ? t("config.capabilities.on")
                                  : t("config.capabilities.off")}
                              </span>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                  </div>

                  <Show when={capError()}>
                    {(message) => <Banner tone="danger" message={message()} />}
                  </Show>
                  <Show when={capOk()}>{(message) => <Banner tone="ok" message={message()} />}</Show>
                  <PublishBar
                    label={t("config.capabilities.title")}
                    publishedAtMs={capabilitiesPublishedAtMs()}
                    describe={describePublish}
                    publishLabel={t("config.capabilities.publish")}
                    busy={busy()}
                    disabled={violatedRules().length > 0}
                    disabledReason={t("config.capabilities.conflicts")}
                    preview={previewNode("capabilities", tenantId(), storeId(), {
                      flags: publishedFlags(),
                    })}
                    onPublish={() => void publishCapabilities()}
                    data-step="publishCapabilities"
                    data-outcome="capabilities-published"
                  />
                </div>
              </Card>
            </div>
          )}
        </Show>

        <div class="grid gap-6 lg:grid-cols-2">
          <Card title={t("config.effective")}>
            <Show when={failureOf(read)}>
              {(message) => <Banner tone="danger" message={message()} />}
            </Show>
            <Show
              when={read.value() !== null}
              fallback={<p class="text-sm text-ink-muted">{t("config.loadHint")}</p>}
            >
              <Show
                when={effective() !== null}
                fallback={<p class="text-sm text-ink-muted">{t("config.effectiveEmpty")}</p>}
              >
                <Show when={!readsPrices()}>
                  <p class="mb-2 text-sm text-ink-muted">{t("config.pricesHidden")}</p>
                </Show>
                <Show when={!readsStaff()}>
                  <p class="mb-2 text-sm text-ink-muted">{t("config.staffHidden")}</p>
                </Show>
                <pre class="max-h-96 overflow-auto rounded-token border border-line bg-surface-raised p-3 text-xs text-ink">
                  {JSON.stringify(effective(), null, 2)}
                </pre>
              </Show>
            </Show>
          </Card>

          {/* Folded away: replacing a whole layer from pasted JSON is the sharpest tool on this
              screen, and it sat as an open card beside the capability form — on the screen the
              get-started used to send a brand-new store to. Still here, one click away. */}
          <details class="rounded-token border border-line bg-surface shadow-raised">
            <summary class="cursor-pointer select-none px-4 py-3 text-sm font-medium text-ink-muted">
              {t("config.advanced")}
            </summary>
            <div class="border-t border-line p-4">
              <p class="mb-3 text-sm text-ink-muted">{t("config.advancedHint")}</p>
              <div class="flex flex-col gap-4">
                <SelectField
                  label={t("config.level")}
                  value={level()}
                  options={CONFIG_LEVELS.map((name) => ({ value: name, label: t(LEVEL_KEY[name]) }))}
                  onChange={(value) => setLevel(value as ConfigLevel)}
                />
                <TextArea
                  label={t("config.document")}
                  value={document()}
                  onInput={setDocument}
                  rows={14}
                />
                <Show when={error()}>
                  {(message) => <Banner tone="danger" message={message()} />}
                </Show>
                <Show when={ok()}>{(message) => <Banner tone="ok" message={message()} />}</Show>
                <Button disabled={busy()} onClick={() => void publish()}>
                  {t("action.publish")}
                </Button>
              </div>
            </div>
          </details>
        </div>

        <div class="mt-6">
          <Card title={t("config.history")}>
            <Show
              when={versions().length > 0}
              fallback={<EmptyState title={t("config.noVersions")} description={t("config.noVersionsHint")} />}
            >
              <ul class="flex flex-col gap-2">
                <For each={versions()}>
                  {(version) => (
                    <li class="flex flex-wrap items-center justify-between gap-2 rounded-token border border-line px-3 py-2">
                      <div class="flex flex-wrap items-center gap-2">
                        <span class="text-sm text-ink">{versionTime(version)}</span>
                        <Show when={version.current}>
                          <StatusBadge tone="active" label={t("config.current")} />
                        </Show>
                      </div>
                      <div class="flex flex-wrap items-center gap-2">
                        <Button
                          variant="secondary"
                          onClick={() => {
                            setCompare(false);
                            void viewVersion(version.version_id);
                          }}
                        >
                          {t("config.view")}
                        </Button>
                        <Show when={!version.current}>
                          <Button
                            variant="secondary"
                            disabled={busy()}
                            onClick={() => setRollbackTo(version.version_id)}
                          >
                            {t("config.rollback")}
                          </Button>
                        </Show>
                      </div>
                    </li>
                  )}
                </For>
              </ul>

              <Show when={viewing()}>
                {(versionId) => (
                  <div class="mt-4">
                    <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
                      <span class="text-sm font-medium text-ink">
                        {t("config.viewingVersion", { version: versionId() })}
                      </span>
                      <CheckboxField
                        label={t("config.compareCurrent")}
                        checked={compare()}
                        onChange={setCompare}
                      />
                    </div>
                    <Show
                      when={compare()}
                      fallback={
                        <pre class="max-h-96 overflow-auto rounded-token border border-line bg-surface-raised p-3 text-xs text-ink">
                          {JSON.stringify(viewingDoc(), null, 2)}
                        </pre>
                      }
                    >
                      <div class="grid gap-3 lg:grid-cols-2">
                        <div>
                          <span class="mb-1 block text-xs font-medium text-ink-muted">
                            {t("config.thisVersion")}
                          </span>
                          <pre class="max-h-96 overflow-auto rounded-token border border-line bg-surface-raised p-3 text-xs text-ink">
                            <For each={diffLines(viewingDoc(), effective())}>
                              {(row) => (
                                <div class={row.changed ? "bg-danger/15 text-ink" : "text-ink"}>
                                  {row.line || " "}
                                </div>
                              )}
                            </For>
                          </pre>
                        </div>
                        <div>
                          <span class="mb-1 block text-xs font-medium text-ink-muted">
                            {t("config.currentVersion")}
                          </span>
                          <pre class="max-h-96 overflow-auto rounded-token border border-line bg-surface-raised p-3 text-xs text-ink">
                            <For each={diffLines(effective(), viewingDoc())}>
                              {(row) => (
                                <div class={row.changed ? "bg-accent/15 text-ink" : "text-ink"}>
                                  {row.line || " "}
                                </div>
                              )}
                            </For>
                          </pre>
                        </div>
                      </div>
                    </Show>
                  </div>
                )}
              </Show>
            </Show>
          </Card>
        </div>
      </RequireContext>

      <ConfirmDialog
        open={rollbackTo() !== null}
        title={t("config.rollbackConfirmTitle")}
        message={t("config.rollbackConfirmBody")}
        confirmLabel={t("config.rollback")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        busy={busy()}
        onConfirm={() => void rollback()}
        onCancel={() => setRollbackTo(null)}
      />
    </div>
  );
}
