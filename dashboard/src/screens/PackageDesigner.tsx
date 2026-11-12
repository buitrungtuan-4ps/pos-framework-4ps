// The package designer (ADR-0189, ADR-0190 decision 8): HQ opens a package's version as a draft,
// edits its parts, has the cloud check it with the publish's own checks, and publishes it as the
// package's next version. Nobody writes JSON.
//
// The shell is this screen: which package, which version the draft starts from, the version's
// label, the parts' tabs, and check and publish. Each part's designer is a panel in
// `packages/designer/parts.tsx`. A refusal is shown beside what it refuses: the label's under the
// label, a part's in its panel, and every one counted on its tab.
//
// The draft is kept in this browser per package (`lib/package-draft.ts`), so a reload resumes it.

import { useSearchParams } from "@solidjs/router";
import { createSignal, For, Show } from "solid-js";

import { createPackage, listPackages, listPackageVersions } from "../api/packages";
import type { PackageRecord, PackageVersionRecord } from "../api/schemas";
import { ConfirmDialog, Modal, Tabs } from "../components/kit";
import { toast } from "../components/Toast";
import { Banner, Button, Card, PageHeader, SelectField, Skeleton, TextField } from "../components/ui";
import { t } from "../i18n";
import { apiMessage } from "../lib/errors";
import { blankDraft, draftFromVersion, dropDraft, loadDraft, refusalsFor } from "../lib/package-draft";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { DESIGNER_PARTS } from "./packages/designer/parts";
import { refusalText } from "./packages/designer/refusals";
import { useDraft, type DraftState } from "./packages/designer/use-draft";

/** A package and its published versions, oldest first. */
type Opened = { readonly record: PackageRecord; readonly versions: readonly PackageVersionRecord[] };

/** The value of the start-from picker that starts with nothing. */
const BLANK = "";

/** What the cloud last said of the draft, and the two acts that ask it. */
function CheckBar(props: { state: DraftState; onCheck: () => void; onPublish: () => void }) {
  const state = props.state;
  const checkDraft = () => props.onCheck();
  const publishDraft = () => props.onPublish();
  return (
    <div class="flex flex-wrap items-center justify-between gap-3 rounded-token border border-line bg-surface-raised p-3">
      <p class="text-sm text-ink" aria-live="polite">
        <Show when={state.verdict()} fallback={t("designer.unchecked")}>
          {(verdict) => (
            <Show
              when={verdict().accepted}
              fallback={t("designer.refused", { count: verdict().refusals.length })}
            >
              <span data-outcome="draft-accepted">{t("designer.accepted", { release: verdict().min_edge_version })}</span>
              <Show when={state.warnings().length > 0}>
                <span data-outcome="draft-warned"> {t("designer.tapWarnings", { count: state.warnings().length })}</span>
              </Show>
            </Show>
          )}
        </Show>
      </p>
      <div class="flex gap-2">
        <Button variant="secondary" disabled={state.busy()} onClick={checkDraft} data-step="checkDraft">
          {t("designer.check")}
        </Button>
        <Button disabled={state.busy()} onClick={publishDraft} data-step="publishDraft">
          {t("designer.publish")}
        </Button>
      </div>
    </div>
  );
}

/** The draft's label, its parts, and check and publish. */
function DraftEditor(props: { opened: Opened; state: DraftState; onPublished: (version: PackageVersionRecord) => void }) {
  const [tab, setTab] = createSignal(DESIGNER_PARTS[0]?.key ?? "");
  const state = props.state;
  const labelRefusal = () => state.refusals().find((refusal) => refusal.field === "version_label");
  const partRefusals = (field: string) => refusalsFor(state.refusals(), field);
  const placed = new Set(["version_label", ...DESIGNER_PARTS.map((part) => part.field)]);
  // A refusal no part shows (time slots, the size of the whole) is shown above the tabs.
  const unplaced = () =>
    state.refusals().filter((refusal) => ![...placed].some((field) => refusalsFor([refusal], field).length > 0));
  const tabLabel = (part: (typeof DESIGNER_PARTS)[number]) => {
    const count = partRefusals(part.field).length;
    return count === 0 ? t(part.label) : t("designer.tabRefused", { part: t(part.label), count });
  };
  const checkDraft = () => void state.check();
  const publishDraft = () =>
    void state.publish().then((version) => {
      if (version !== null) {
        props.onPublished(version);
      }
    });
  const active = () => DESIGNER_PARTS.find((part) => part.key === tab());
  return (
    <div class="flex flex-col gap-4">
      <Card title={t("designer.draftTitle", { name: props.opened.record.name })}>
        <TextField
          label={t("designer.versionLabel")}
          value={state.draft().version_label}
          data-step="setVersionLabel"
          onInput={(value) => state.change((draft) => ({ ...draft, version_label: value }))}
        />
        <Show when={labelRefusal()}>
          {(refusal) => (
            <p role="alert" class="mt-1 text-xs text-danger">
              {refusalText(refusal(), state.draft())}
            </p>
          )}
        </Show>
      </Card>
      <For each={unplaced()}>{(refusal) => <Banner tone="danger" message={refusalText(refusal, state.draft())} />}</For>
      <Tabs
        label={t("designer.partsLabel")}
        tabs={DESIGNER_PARTS.map((part) => ({ key: part.key, label: tabLabel(part) }))}
        active={tab()}
        onSelect={setTab}
      />
      <Show when={active()} keyed>
        {(part) => <part.Panel draft={state.draft} change={state.change} refusals={partRefusals(part.field)} warnings={state.warnings()} />}
      </Show>
      <CheckBar state={state} onCheck={checkDraft} onPublish={publishDraft} />
    </div>
  );
}

/** The opened package's draft: resumed from this browser, else its newest version, else blank. */
function OpenedPackage(props: { opened: Opened; onPublished: (version: PackageVersionRecord) => void }) {
  const kept = loadDraft(tenantId(), props.opened.record.package_id);
  const newest = props.opened.versions.at(-1);
  const state = useDraft(
    tenantId,
    () => props.opened.record.package_id,
    kept ?? (newest ? draftFromVersion(newest) : blankDraft()),
  );
  const [startFrom, setStartFrom] = createSignal<string | null>(null);
  const restart = () => {
    const chosen = props.opened.versions.find((v) => v.content.package_version_id === startFrom());
    dropDraft(tenantId(), props.opened.record.package_id);
    state.reset(chosen ? draftFromVersion(chosen) : blankDraft());
    setStartFrom(null);
  };
  const options = () => [
    { value: BLANK, label: t("designer.startBlank") },
    ...props.opened.versions.map((version) => ({
      value: version.content.package_version_id,
      label: version.content.version_label,
    })),
  ];
  return (
    <div class="flex flex-col gap-4">
      <Show when={kept !== null}>
        <p class="text-sm text-ink-muted">{t("designer.resumed")}</p>
      </Show>
      <Show when={state.error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <SelectField
        label={t("designer.startFrom")}
        hint={t("designer.startFromHint")}
        value=""
        options={options()}
        placeholder={t("designer.startFromPick")}
        onChange={(value) => setStartFrom(value)}
      />
      <DraftEditor opened={props.opened} state={state} onPublished={props.onPublished} />
      <ConfirmDialog
        open={startFrom() !== null}
        title={t("designer.restartTitle")}
        message={t("designer.restartMessage")}
        confirmLabel={t("designer.restart")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        onConfirm={restart}
        onCancel={() => setStartFrom(null)}
      />
    </div>
  );
}

/** Which package the draft is of, and the way to start a new one. */
function PackagePicker(props: {
  packages: readonly PackageRecord[];
  chosen: string;
  onChoose: (packageId: string) => void;
  onCreated: (record: PackageRecord) => void;
}) {
  const [naming, setNaming] = createSignal<string | null>(null);
  const name = () => naming()?.trim() ?? "";
  const newPackage = () => setNaming("");
  const createNamed = async () => {
    try {
      const record = await createPackage(tenantId(), name());
      setNaming(null);
      props.onCreated(record);
    } catch (caught) {
      toast.error(apiMessage(caught));
    }
  };
  return (
    <div class="flex flex-wrap items-end gap-3">
      <div class="min-w-64 grow">
        <SelectField
          label={t("designer.package")}
          value={props.chosen}
          placeholder={t("designer.packagePick")}
          options={props.packages.map((entry) => ({ value: entry.package_id, label: entry.name }))}
          onChange={(value) => props.onChoose(value)}
        />
      </div>
      <Button variant="secondary" onClick={newPackage} data-step="newPackage">
        {t("designer.newPackage")}
      </Button>
      <Modal
        open={naming() !== null}
        title={t("designer.newPackage")}
        closeLabel={t("action.close")}
        onClose={() => setNaming(null)}
        footer={
          <Button onClick={() => void createNamed()} disabled={name() === ""} data-step="createNamed">
            {t("designer.create")}
          </Button>
        }
      >
        <TextField label={t("designer.packageName")} value={naming() ?? ""} onInput={setNaming} data-step="setPackageName" />
      </Modal>
    </div>
  );
}

export function PackageDesigner() {
  const [search, setSearch] = useSearchParams<{ package?: string }>();
  const [packages, setPackages] = createSignal<readonly PackageRecord[] | null>(null);
  const [opened, setOpened] = createSignal<Opened | null>(null);
  const [error, setError] = createSignal("");
  const [published, setPublished] = createSignal<string | null>(null);

  const open = async (packageId: string | undefined) => {
    setOpened(null);
    const record = packages()?.find((entry) => entry.package_id === packageId);
    if (!record) {
      return;
    }
    try {
      setOpened({ record, versions: await listPackageVersions(tenantId(), record.package_id) });
    } catch (caught) {
      setError(apiMessage(caught));
    }
  };
  const load = async () => {
    setError("");
    try {
      setPackages(await listPackages(tenantId()));
      await open(search.package);
    } catch (caught) {
      setError(apiMessage(caught));
    }
  };
  onScopedContext("tenant", () => void load());

  const choosePackage = (packageId: string) => {
    setPublished(null);
    setSearch({ package: packageId || undefined });
    void open(packageId);
  };
  const onCreated = (record: PackageRecord) => {
    setPackages([...(packages() ?? []), record]);
    choosePackage(record.package_id);
  };
  const onPublished = (version: PackageVersionRecord) => {
    toast.ok(t("designer.published", { version: version.content.version_label }));
    setPublished(version.content.version_label);
    const current = opened();
    if (current) {
      setOpened({ ...current, versions: [...current.versions, version] });
    }
  };

  return (
    <div>
      <PageHeader title={t("nav.packageDesigner")} description={t("designer.description")} />
      <RequireContext need="tenant">
        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show when={packages()} fallback={<Skeleton label={t("common.loading")} rows={4} />}>
          {(list) => (
            <div class="flex flex-col gap-4">
              <PackagePicker packages={list()} chosen={search.package ?? ""} onChoose={choosePackage} onCreated={onCreated} />
              <Show when={published()}>
                {(label) => (
                  <p class="text-sm text-ink" data-outcome="package-version-published">
                    {t("designer.published", { version: label() })}
                  </p>
                )}
              </Show>
              <Show when={opened()} keyed>
                {(entry) => <OpenedPackage opened={entry} onPublished={onPublished} />}
              </Show>
            </div>
          )}
        </Show>
      </RequireContext>
    </div>
  );
}
