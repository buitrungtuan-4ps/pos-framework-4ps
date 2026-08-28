// The Catalog shell (ADR-0082, Track F3): one `/catalog` route, one nav entry, one breadcrumb — a tab
// bar over the five kit-based sub-screens the 1,898-line monolith split into (Items, Taxonomy, Tax
// classes, Modifiers, Menus). The shell owns the page header and the `RequireContext` tenant gate;
// each tab renders only when selected, so its `onScopedContext` load fires on first view and the five
// screens never all fetch at once. This replaces `screens/Catalog.tsx`.

import { createSignal, Match, Switch } from "solid-js";
import { For } from "solid-js";

import { t, type MessageKey } from "../../i18n";
import { RequireContext } from "../../lib/scoped";
import { PageHeader } from "../../components/ui";
import { CatalogItems } from "./Items";
import { CatalogMenus } from "./Menus";
import { CatalogModifiers } from "./Modifiers";
import { CatalogTaxClasses } from "./TaxClasses";
import { CatalogTaxonomy } from "./Taxonomy";

type TabKey = "items" | "taxonomy" | "taxClasses" | "modifiers" | "menus";

const TABS: readonly { key: TabKey; label: MessageKey }[] = [
  { key: "items", label: "catalog.items" },
  { key: "taxonomy", label: "catalog.tabTaxonomy" },
  { key: "taxClasses", label: "catalog.taxClasses" },
  { key: "modifiers", label: "catalog.tabModifiers" },
  { key: "menus", label: "catalog.menus" },
];

export function CatalogShell() {
  const [tab, setTab] = createSignal<TabKey>("items");

  return (
    <div>
      <PageHeader title={t("catalog.title")} description={t("catalog.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <div role="tablist" aria-label={t("catalog.title")} class="flex flex-wrap gap-1 border-b border-line">
            <For each={TABS}>
              {(entry) => (
                <button
                  type="button"
                  role="tab"
                  aria-selected={tab() === entry.key}
                  class={`min-h-touch rounded-t-token px-4 text-sm font-medium ${
                    tab() === entry.key
                      ? "border-b-2 border-accent text-ink"
                      : "text-ink-muted hover:text-ink"
                  }`}
                  onClick={() => setTab(entry.key)}
                >
                  {t(entry.label)}
                </button>
              )}
            </For>
          </div>

          <Switch>
            <Match when={tab() === "items"}>
              <CatalogItems />
            </Match>
            <Match when={tab() === "taxonomy"}>
              <CatalogTaxonomy />
            </Match>
            <Match when={tab() === "taxClasses"}>
              <CatalogTaxClasses />
            </Match>
            <Match when={tab() === "modifiers"}>
              <CatalogModifiers />
            </Match>
            <Match when={tab() === "menus"}>
              <CatalogMenus />
            </Match>
          </Switch>
        </div>
      </RequireContext>
    </div>
  );
}
