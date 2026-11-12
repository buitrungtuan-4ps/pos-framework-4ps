// The console's preview of a designed screen, drawn by the till's own renderer
// (`packages/package-renderer`, ADR-0209). The components that draw from the package alone draw
// exactly as on the till; a region, whose contents are the till's (the order's lines, the tickets),
// draws as an outline that names it and holds its children, where the till's layout puts it
// (decision 3). Tests hold the preview and the till's mount to one committed fixture (decision 4).

import { Show, type JSX } from "solid-js";

import { BASE, isBase } from "../../../../../packages/package-renderer/src/base";
import { COMPONENTS, type ComponentId, type ScreenNode } from "../../../../../packages/package-renderer/src/components";
import { Children, NeedsUpdateSlot, PackageTree, type Implementation, type Implementations, type ScreenHost, type Words } from "../../../../../packages/package-renderer/src/render";
import { marked } from "../../../../../packages/package-renderer/src/shape";
import { locale, t } from "../../../i18n";
import { componentName, deviceName } from "./names";

/** A region in the preview: an outline that names its component, around the nodes it holds. */
function outline(id: ComponentId): Implementation {
  return function Outline(props) {
    return (
      <div class="flex min-h-12 flex-col gap-2 rounded-token border border-dashed border-line p-2" data-region={id}>
        <span class="text-xs font-medium text-ink-muted">{componentName(id)}</span>
        <Children {...props} />
      </div>
    );
  };
}

/** The preview's table: the till's own components where they draw from the package alone, else an outline. */
export const PREVIEW_IMPLEMENTATIONS: Implementations = marked(
  Object.fromEntries(
    COMPONENTS.map(({ component_id }) => {
      const id = component_id as ComponentId;
      return [id, isBase(id) ? BASE[id] : outline(id)];
    }),
  ) as Implementations,
);

/** A preview offers every act and runs none: a tap there changes nothing. */
const PREVIEW_HOST: ScreenHost = { subject: () => ({}), offers: () => true, run: () => undefined };

/** A tenant's translation grid: each key's text by language. */
export type Grid = Readonly<Record<string, Readonly<Record<string, string>>>>;

/** The preview's words: a key's text in the console's language, then English, else the key itself. */
export function previewWords(grid: () => Grid): Words {
  return {
    label: (key) => grid()[key]?.[locale()] ?? grid()[key]?.["en"] ?? key,
    needsUpdate: () => t("designer.screens.previewNeedsUpdate"),
    failure: () => "",
  };
}

/** How wide each device class's preview is drawn, as the till's breakpoints place it. */
const WIDTH: Readonly<Record<string, string>> = {
  DEVICE_CLASS_PHONE: "max-w-sm",
  DEVICE_CLASS_TABLET: "max-w-3xl",
};

/**
 * A tree drawn as the till draws it, for `words` and `host`. The renderer mounts a tree once, as the
 * till does, so each edit (a new tree) mounts a new one.
 */
export function PreviewTree(props: { root: ScreenNode; words: Words; host?: ScreenHost }): JSX.Element {
  return (
    <Show when={props.root} keyed>
      {(root) => (
        <PackageTree
          root={root}
          host={props.host ?? PREVIEW_HOST}
          implementations={PREVIEW_IMPLEMENTATIONS}
          words={props.words}
          fallback={<NeedsUpdateSlot text={t("designer.screens.previewTooLarge")} />}
        />
      )}
    </Show>
  );
}

/** The chosen variant's preview, framed at its device class's width. */
export function ScreenPreview(props: { root: ScreenNode; deviceClass: string | undefined; words: Words }): JSX.Element {
  return (
    <section class="flex flex-col gap-2" aria-label={t("designer.screens.preview")} data-outcome="screen-preview">
      <h3 class="text-sm font-semibold text-ink">
        {t("designer.screens.preview")}
        <Show when={props.deviceClass}>{(deviceClass) => <span class="font-normal text-ink-muted"> · {deviceName(deviceClass())}</span>}</Show>
      </h3>
      <p class="text-xs text-ink-muted">{t("designer.screens.previewHint")}</p>
      <div class={`w-full overflow-hidden rounded-token border border-line bg-surface ${WIDTH[props.deviceClass ?? ""] ?? ""}`}>
        <PreviewTree root={props.root} words={props.words} />
      </div>
    </section>
  );
}
