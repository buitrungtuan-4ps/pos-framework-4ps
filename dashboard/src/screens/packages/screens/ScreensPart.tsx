// The screen designer (ADR-0190, ADR-0203): the package's screens, each a tree of the catalogue's
// components, one variant per device class where the screen needs one (ADR-0190 decision 5). HQ
// drags components from the palette onto the tree, or adds them from the keyboard, and gives each
// its properties in the inspector. The draft's `screens` is written by these acts alone.
//
// The cloud's refusals of a screen name it and its node by position; the designer selects that
// variant's tree and outlines the node.

import { createSignal, For, Show } from "solid-js";

import { api } from "../../../api/client";
import type { PackageWarning, ScreenVariant } from "../../../api/schemas";
import { Button, SelectField, TextField } from "../../../components/ui";
import { t } from "../../../i18n";
import { COMPONENTS, type ScreenNode } from "../../../../../packages/package-renderer/src/components";
import { DEVICE_CLASSES, drawnOn, pathAtPreorder } from "../../../lib/screen-tree";
import { tenantId } from "../../../state/session";
import type { PartProps } from "../designer/parts";
import { useScreens } from "./use-screens";
import { refusalText } from "../designer/refusals";
import { Canvas, dragComponent } from "./Canvas";
import { Inspector } from "./Inspector";
import { actionName, componentName, deviceName } from "./names";
import { previewWords, ScreenPreview, type Grid } from "./Preview";

/** A screen id the cloud accepts: 1 to 64 of `a`–`z`, `0`–`9` and `_`. */
const SCREEN_ID = /^[a-z0-9_]{1,64}$/;

/** The tenant's translation grid, read once: its keys for the key fields, its text for the preview. */
function useGrid() {
  const [grid, setGrid] = createSignal<Grid>({});
  void api
    .getTranslations(tenantId())
    .then((read) => setGrid(read.value))
    .catch(() => setGrid({}));
  return grid;
}

/** The package's screens, a variant chosen, and the ways to add both. */
function ScreenList(props: {
  screens: readonly ScreenVariant[];
  chosen: number;
  onChoose: (index: number) => void;
  onAddScreen: (screenId: string) => void;
  onAddVariant: (deviceClass: string) => void;
  onRemove: () => void;
}) {
  const [screenId, setScreenId] = createSignal("");
  const valid = () => SCREEN_ID.test(screenId()) && !props.screens.some((v) => v.body.screen_id === screenId());
  const current = () => props.screens[props.chosen];
  const missing = () =>
    DEVICE_CLASSES.filter(
      (deviceClass) =>
        !props.screens.some(
          (v) => v.body.screen_id === current()?.body.screen_id && v.selector?.device_class === deviceClass,
        ),
    );
  const addScreen = () => {
    props.onAddScreen(screenId());
    setScreenId("");
  };
  return (
    <div class="flex flex-col gap-3">
      <ul class="flex flex-col gap-1" aria-label={t("designer.screens.list")}>
        <For each={props.screens}>
          {(variant, at) => (
            <li>
              <button
                type="button"
                class="w-full rounded-token px-2 py-1 text-left text-sm"
                classList={{ "bg-surface-raised font-medium": at() === props.chosen }}
                aria-pressed={at() === props.chosen}
                onClick={() => props.onChoose(at())}
              >
                {variant.body.screen_id} ·{" "}
                {variant.selector?.device_class ? deviceName(variant.selector.device_class) : t("designer.screens.everyDevice")}
              </button>
            </li>
          )}
        </For>
      </ul>
      <TextField label={t("designer.screens.newId")} hint={t("designer.screens.newIdHint")} value={screenId()} onInput={setScreenId} data-step="setScreenId" />
      <Button size="sm" variant="secondary" disabled={!valid()} onClick={addScreen} data-step="addScreen">
        {t("designer.screens.addScreen")}
      </Button>
      <Show when={current()}>
        <SelectField
          label={t("designer.screens.addVariant")}
          value=""
          placeholder={t("designer.screens.pickDevice")}
          options={missing().map((deviceClass) => ({ value: deviceClass, label: deviceName(deviceClass) }))}
          onChange={(deviceClass) => deviceClass !== "" && props.onAddVariant(deviceClass)}
        />
        <Button size="sm" variant="danger-ghost" onClick={() => props.onRemove()}>
          {t("designer.screens.removeVariant")}
        </Button>
      </Show>
    </div>
  );
}

/**
 * The screen a till opens after sign-in (ADR-0210 decision 1): the base home, one of the version's
 * screens, or none for the release's own start. A home for one role or device class, which a draft
 * may carry, is kept as it is.
 */
function HomeField(props: PartProps) {
  const homes = () => props.draft().home_screens ?? [];
  const base = () => homes().find((home) => !home.selector || Object.keys(home.selector).length === 0);
  const ids = () => [...new Set((props.draft().screens ?? []).map((variant) => variant.body.screen_id))];
  const choose = (screenId: string) =>
    props.change((draft) => {
      const others = (draft.home_screens ?? []).filter((home) => home.selector && Object.keys(home.selector).length > 0);
      return { ...draft, home_screens: screenId === "" ? others : [{ selector: {}, body: { screen_id: screenId } }, ...others] };
    });
  return (
    <SelectField
      label={t("designer.screens.home")}
      hint={t("designer.screens.homeHint")}
      value={base()?.body.screen_id ?? ""}
      options={[{ value: "", label: t("designer.screens.homeRelease") }, ...ids().map((id) => ({ value: id, label: id }))]}
      onChange={choose}
    />
  );
}

/**
 * The notes beside each node of the variant at `screenIndex`: its acts over their tap budget
 * (ADR-0210 decision 5), one line per act, role and cost, naming the device classes it holds on.
 */
export function tapNotes(
  warnings: readonly PackageWarning[],
  screenIndex: number,
  root: ScreenVariant["body"]["root"] | undefined,
): ReadonlyMap<string, readonly string[]> {
  const grouped = new Map<string, { path: string; warning: PackageWarning; devices: string[] }>();
  for (const warning of warnings) {
    const path = root && warning.screen_index === screenIndex ? pathAtPreorder(root as ScreenNode, warning.node_index) : undefined;
    if (path === undefined) {
      continue;
    }
    const key = [path.join("."), warning.action_id, warning.role_id ?? "", warning.taps].join("|");
    const held = grouped.get(key) ?? { path: path.join("."), warning, devices: [] };
    held.devices.push(deviceName(warning.device_class));
    grouped.set(key, held);
  }
  const notes = new Map<string, string[]>();
  for (const { path, warning, devices } of grouped.values()) {
    const args = { act: actionName(warning.action_id), taps: warning.taps, devices: devices.join(", "), role: warning.role_id ?? "" };
    const note = warning.role_id === undefined ? t("designer.screens.tapWarning", args) : t("designer.screens.tapWarningRole", args);
    notes.set(path, [...(notes.get(path) ?? []), note]);
  }
  return notes;
}

/** The components a variant may hold, each dragged or added. */
function Palette(props: { deviceClass: string | undefined; onAdd: (componentId: string) => void }) {
  const offered = () => COMPONENTS.filter((component) => drawnOn(component, props.deviceClass));
  return (
    <ul class="flex max-h-[60vh] flex-col gap-1 overflow-y-auto" aria-label={t("designer.screens.palette")}>
      <For each={offered()}>
        {(component) => (
          <li
            draggable={true}
            class="flex cursor-grab items-center justify-between gap-2 rounded-token border border-line bg-surface px-2 py-1 text-sm"
            onDragStart={(event) => dragComponent(event, component.component_id)}
            data-component-id={component.component_id}
          >
            <span>{componentName(component.component_id)}</span>
            <Button
              size="sm"
              variant="ghost"
              aria-label={t("designer.screens.addNamed", { name: componentName(component.component_id) })}
              onClick={() => props.onAdd(component.component_id)}
            >
              {t("designer.screens.add")}
            </Button>
          </li>
        )}
      </For>
    </ul>
  );
}

/** The screens tab of the package designer. */
export function ScreensPart(props: PartProps) {
  const grid = useGrid();
  const keys = () => Object.keys(grid()).sort();
  const words = previewWords(grid);
  const edit = useScreens(props);
  return (
    <div class="flex flex-col gap-3">
      <Show when={props.refusals.length > 0}>
        <ul role="alert" class="flex flex-col gap-1 rounded-token border border-danger p-3 text-sm text-danger" data-outcome="draft-refused">
          <For each={props.refusals}>
            {(refusal) => (
              <li>
                <button type="button" class="text-left underline" onClick={() => edit.choose(refusal.screen_index ?? 0)}>
                  {refusalText(refusal, props.draft())}
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <div class="grid gap-4 terminal:grid-cols-[14rem_1fr_18rem]">
        <div class="flex flex-col gap-4">
          <ScreenList
            screens={edit.screens()}
            chosen={edit.chosen()}
            onChoose={edit.choose}
            onAddScreen={edit.addScreen}
            onAddVariant={edit.addVariant}
            onRemove={edit.removeVariant}
          />
          <HomeField {...props} />
        </div>
        <Show when={edit.root()} fallback={<p class="text-sm text-ink-muted">{t("designer.screens.empty")}</p>}>
          {(tree) => (
            <div class="flex min-w-0 flex-col gap-4">
              <Canvas
                root={tree()}
                selected={edit.selected()}
                faulty={edit.faulty()}
                notes={tapNotes(props.warnings ?? [], edit.chosen(), edit.variant()?.body.root)}
                onSelect={edit.setSelected}
                onDrop={edit.drop}
              />
              <ScreenPreview root={tree()} deviceClass={edit.variant()?.selector?.device_class} words={words} />
            </div>
          )}
        </Show>
        <div class="flex flex-col gap-4">
          <Show when={edit.node()}>
            {(shown) => (
              <section class="flex flex-col gap-2">
                <h3 class="text-sm font-semibold text-ink">{componentName(shown().component_id)}</h3>
                <Inspector
                  node={shown()}
                  keys={keys()}
                  screenIds={[...new Set(edit.screens().map((entry) => entry.body.screen_id))]}
                  isRoot={edit.selected().length === 0}
                  onSet={edit.set}
                  onMove={edit.move}
                  onRemove={edit.remove}
                />
              </section>
            )}
          </Show>
          <Show when={edit.variant()}>
            <section class="flex flex-col gap-2">
              <h3 class="text-sm font-semibold text-ink">{t("designer.screens.palette")}</h3>
              <Palette deviceClass={edit.variant()?.selector?.device_class} onAdd={edit.addComponent} />
            </section>
          </Show>
        </div>
      </div>
    </div>
  );
}
