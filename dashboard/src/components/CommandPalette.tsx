// The command palette (ADR-0060, Track F1): Cmd/Ctrl-K opens a quick switcher over the console's
// screens — type to filter, ↑/↓ to move, Enter to jump. Screens only for now; entity search (jump
// straight to a store, item, or key by name) arrives with the CRUD kit's data layer (F2).

import { createEffect, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { t } from "../i18n";
import { SCREENS, type ScreenId, screenHref, specOf } from "../state/screens";
import { storeId, tenantId } from "../state/session";

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

  const matches = () => {
    const needle = query().trim().toLowerCase();
    // Each target is resolved against the live context, so jumping from the palette keeps the
    // tenant the operator is working in rather than dropping them at a bare path.
    const all = TARGETS.map((id) => ({
      href: screenHref(id, tenantId(), storeId()),
      label: t(specOf(id).key),
    }));
    return needle ? all.filter((item) => item.label.toLowerCase().includes(needle)) : all;
  };

  const close = () => {
    setOpen(false);
    setQuery("");
    setActive(0);
  };

  const go = (href: string) => {
    close();
    navigate(href);
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
        go(chosen.href);
      }
    }
  };

  return (
    <Show when={open()}>
      <div
        class="fixed inset-0 z-50 flex items-start justify-center bg-black/40 p-4 pt-24"
        onClick={close}
      >
        <div
          class="w-full max-w-lg overflow-hidden rounded-token border border-line bg-surface shadow-lg"
          onClick={(event) => event.stopPropagation()}
        >
          <input
            ref={input}
            type="text"
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
            <ul class="max-h-72 overflow-y-auto p-2">
              <For each={matches()}>
                {(item, index) => (
                  <li>
                    <button
                      type="button"
                      onMouseEnter={() => setActive(index())}
                      onClick={() => go(item.href)}
                      class={`flex w-full items-center rounded-token px-3 py-2 text-left text-base text-ink ${
                        index() === active() ? "bg-surface-raised" : ""
                      }`}
                    >
                      {item.label}
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
