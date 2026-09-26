// The command palette (ADR-0060, Track F1): Cmd/Ctrl-K opens a quick switcher over the console's
// screens — type to filter, ↑/↓ to move, Enter to jump.
//
// Since Wave 4 · PR-8 it also searches the tenant's **entities** (roadmap-v3 F16): a shop by name,
// an item by name in any locale. That was the half the header of this file promised and F2 did not
// deliver — a console whose search box only finds its own screens sends an operator who knows the
// shop's name to a list to scroll.
//
// # What it does not search, and why
//
// The **employee roster**. A name in a roster is personal data (T1,
// [ADR-0076](../../../docs/adr/0076-subject-requests.md)), and a global search box is the one place
// in the console where it would be typed casually and read by whoever is standing there. People are
// found on the People screen, which is behind its own permission and its own audit. That is a
// deliberate exclusion, not an omission.

import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { api } from "../api/client";
import { t } from "../i18n";
import { SCREENS, type ScreenId, screenHref, specOf } from "../state/screens";
import { type IconName, Icon } from "./icons";
import { selectStore, storeId, tenantId } from "../state/session";

/** How long a keystroke rests before the palette asks the server. */
const SEARCH_DEBOUNCE_MS = 200;

/** Below this, a search would match most of the master and tell the operator nothing. */
const MIN_QUERY = 2;

/** How many of each kind to offer. The palette is a shortcut, not a table. */
const PER_KIND = 5;

/** One row in the palette: a screen to jump to, or an entity to open. */
interface Entry {
  /** Where Enter goes. */
  readonly href: string;
  /** What the row reads. */
  readonly label: string;
  readonly icon: IconName;
  /** The kind, said in the operator's words ("Shop", "Item") — absent for a screen. */
  readonly hint?: string;
  /** Run before navigating: a shop sets the working store, so the screen opens on it (ADR-0120). */
  readonly before?: () => void;
}

// The palette's entries come from the one screen table, filtered to those worth a quick jump — so a
// screen can never be in the palette under a path the router does not serve, which was possible when
// this file kept its own list of eleven hand-written hrefs.
const TARGETS: readonly ScreenId[] = (Object.keys(SCREENS) as ScreenId[]).filter(
  (id) => specOf(id).inPalette,
);

// Open state is module-level so the top-bar button (and touch devices, which have no Cmd-K) can open
// the palette alongside the keyboard shortcut.
const [open, setOpen] = createSignal(false);

/** Opens the command palette — wired to the top-bar search button. */
export function openPalette(): void {
  setOpen(true);
}

export function CommandPalette() {
  const navigate = useNavigate();
  const [query, setQuery] = createSignal("");
  const [active, setActive] = createSignal(0);
  let input: HTMLInputElement | undefined;
  const optionRefs: HTMLButtonElement[] = [];

  // Memoize search matching to prevent redundant TARGETS.map, t() translations, and array filter calls on every render / key event
  const screens = createMemo(() => {
    const needle = query().trim().toLowerCase();
    // Each target is resolved against the live context, so jumping from the palette keeps the
    // tenant the operator is working in rather than dropping them at a bare path.
    const all: Entry[] = TARGETS.map((id) => ({
      href: screenHref(id, tenantId(), storeId()),
      label: t(specOf(id).key),
      icon: specOf(id).icon,
    }));
    return needle ? all.filter((item) => item.label.toLowerCase().includes(needle)) : all;
  });

  // The entity half (F16). Held rather than derived because it is a round-trip: the query is
  // debounced, and an answer that arrives after a newer one was asked for is dropped, so a fast
  // typist never sees the results of a prefix they have already moved past.
  const [entities, setEntities] = createSignal<Entry[]>([]);
  let asked = 0;

  const findEntities = async (needle: string, tenant: string, token: number) => {
    const [stores, items] = await Promise.allSettled([
      api.listStores(tenant),
      api.listItemsPage(tenant, { limit: PER_KIND, offset: 0 }, { q: needle }),
    ]);
    if (token !== asked) {
      return;
    }
    const found: Entry[] = [];
    if (stores.status === "fulfilled") {
      // The store list is small and unpaged, so the match is made here; every other kind asks the
      // server, which is where the index is.
      found.push(
        ...stores.value
          .filter(
            (store) =>
              store.status === "active" && store.name.toLowerCase().includes(needle.toLowerCase()),
          )
          .slice(0, PER_KIND)
          .map((store) => ({
            href: screenHref("storeHub", tenant, store.store_id),
            label: store.name,
            icon: specOf("storeHub").icon,
            hint: t("palette.kindStore"),
            before: () => selectStore(store.store_id, store.name),
          })),
      );
    }
    if (items.status === "fulfilled") {
      found.push(
        ...items.value.items.map((item) => ({
          // The catalog opens on its items tab, with the search box carrying the same words — the
          // palette hands the screen the query rather than an id it has no route for.
          href: `${screenHref("catalog", tenant, storeId())}?q=${encodeURIComponent(item.name)}`,
          label: item.name,
          icon: specOf("catalog").icon,
          hint: t("palette.kindItem"),
        })),
      );
    }
    setEntities(found);
  };

  // A failure here is silence, not a toast: the palette is a shortcut over reads that have their own
  // screens, and a red box over the search box for a read that will be retried on the next keystroke
  // would be louder than the problem.
  createEffect(() => {
    const needle = query().trim();
    const tenant = tenantId();
    const token = (asked += 1);
    if (needle.length < MIN_QUERY || !tenant) {
      setEntities([]);
      return;
    }
    const timer = setTimeout(() => {
      void findEntities(needle, tenant, token).catch(() => setEntities([]));
    }, SEARCH_DEBOUNCE_MS);
    onCleanup(() => clearTimeout(timer));
  });

  const matches = createMemo(() => [...screens(), ...entities()]);

  const close = () => {
    setOpen(false);
    setQuery("");
    setActive(0);
  };

  const go = (entry: Entry) => {
    entry.before?.();
    close();
    navigate(entry.href);
  };

  const onGlobalKey = (event: KeyboardEvent) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      setOpen((value) => !value);
    } else if (event.key === "Escape" && open()) {
      close();
    }
  };

  onMount(() => window.addEventListener("keydown", onGlobalKey));
  onCleanup(() => window.removeEventListener("keydown", onGlobalKey));

  // Focus the field and reset the cursor each time the palette opens.
  createEffect(() => {
    if (open()) {
      setActive(0);
      queueMicrotask(() => input?.focus());
    }
  });

  // Ensure active option is scrolled into view when index changes
  createEffect(() => {
    const idx = active();
    if (open() && optionRefs[idx]) {
      optionRefs[idx]?.scrollIntoView?.({ block: "nearest" });
    }
  });

  const onFieldKey = (event: KeyboardEvent) => {
    const items = matches();
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((index) => Math.min(index + 1, items.length - 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((index) => Math.max(index - 1, 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      const chosen = items[active()];
      if (chosen) {
        go(chosen);
      }
    }
  };

  return (
    <Show when={open()}>
      <div
        role="dialog"
        aria-modal="true"
        aria-label={t("palette.placeholder")}
        class="fixed inset-0 z-50 flex items-start justify-center bg-black/40 p-4 pt-24"
        onClick={close}
      >
        <div
          class="w-full max-w-lg overflow-hidden rounded-token border border-line bg-surface shadow-overlay"
          onClick={(event) => event.stopPropagation()}
        >
          <input
            ref={input}
            type="text"
            role="combobox"
            aria-expanded="true"
            aria-controls="command-palette-results"
            aria-autocomplete="list"
            aria-activedescendant={
              matches().length > 0 ? `command-palette-option-${active()}` : undefined
            }
            aria-label={t("palette.placeholder")}
            placeholder={t("palette.placeholder")}
            value={query()}
            onInput={(event) => {
              setQuery(event.currentTarget.value);
              setActive(0);
            }}
            onKeyDown={onFieldKey}
            class="w-full border-b border-line bg-surface px-4 py-3 text-base text-ink outline-none"
          />
          <Show
            when={matches().length > 0}
            fallback={<p class="px-4 py-3 text-sm text-ink-muted">{t("palette.empty")}</p>}
          >
            <ul id="command-palette-results" role="listbox" class="max-h-72 overflow-y-auto p-2">
              <For each={matches()}>
                {(item, index) => (
                  <li>
                    <button
                      id={`command-palette-option-${index()}`}
                      ref={(el) => (optionRefs[index()] = el)}
                      type="button"
                      role="option"
                      aria-selected={index() === active()}
                      onMouseEnter={() => setActive(index())}
                      onClick={() => go(item)}
                      class={`flex w-full items-center gap-2 rounded-token px-3 py-2 text-left text-base text-ink transition-colors ${
                        index() === active() ? "bg-surface-raised" : ""
                      }`}
                    >
                      <Icon name={item.icon} />
                      <span class="flex-1 truncate">{item.label}</span>
                      <Show when={item.hint}>
                        {(hint) => <span class="text-sm text-ink-muted">{hint()}</span>}
                      </Show>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </div>
      </div>
    </Show>
  );
}
