// The CRUD kit (ADR-0060, Track F2): the reusable table / form / dialog / status primitives the
// master-data screens are built from, so each screen stops hand-rolling a table, a delete
// confirmation, and an empty state. Like `components/ui.tsx`, every component is built from the
// design tokens and carries NO user-visible text of its own — labels, headers and messages are
// passed in already translated by the caller, so the no-hardcoded-strings lint (ADR-0020) has
// nothing to flag here. Write outcomes are surfaced through the F1 `toast` primitive by the caller.

import {
  createEffect,
  createMemo,
  createSignal,
  For,
  type JSX,
  onCleanup,
  type ParentProps,
  Show,
} from "solid-js";

import type { NodePreview } from "../api/types";
import { t } from "../i18n";
import type { EntityCrud } from "../lib/entity-crud";
import { useEscape } from "../lib/escape";
import { Banner, Button, TextField } from "./ui";

// --- Pager --------------------------------------------------------------------------------------

/**
 * "1–25 of 812", with prev/next.
 *
 * Extracted from {@link DataTable} because a table is not the only thing worth paging: the media
 * library is a grid of thumbnails, and a second pager written by hand beside the first is how two
 * pagers come to disagree about whether the last page is reachable. Purely presentational — it owns
 * no page state, derives everything from what it is handed, and asks the caller for a new `offset`.
 *
 * Renders nothing when the whole set fits in one page, so a caller can mount it unconditionally.
 */
export function Pager(props: {
  /** Where this page starts in the set. */
  offset: number;
  /** The page size that was asked for. */
  limit: number;
  /** How many rows the set holds in total, across every page. */
  total: number;
  /** How many rows *this* page actually carries — the last page is usually short. */
  shown: number;
  /** Asks the caller to load the page starting at `offset`. */
  onOffset: (offset: number) => void;
}) {
  const from = () => (props.total === 0 ? 0 : props.offset + 1);
  const to = () => Math.min(props.total, props.offset + props.shown);
  const hasPrev = () => props.offset > 0;
  const hasNext = () => props.offset + props.limit < props.total;

  return (
    <Show when={props.limit > 0 && props.total > props.limit}>
      <div class="flex items-center justify-between gap-2 text-sm text-ink-muted">
        <span>{t("table.range", { from: from(), to: to(), total: props.total })}</span>
        <div class="flex gap-2">
          <Button
            variant="secondary"
            disabled={!hasPrev()}
            onClick={() => props.onOffset(Math.max(0, props.offset - props.limit))}
          >
            {t("table.prev")}
          </Button>
          <Button
            variant="secondary"
            disabled={!hasNext()}
            onClick={() => props.onOffset(props.offset + props.limit)}
          >
            {t("table.next")}
          </Button>
        </div>
      </div>
    </Show>
  );
}

// --- DataTable ----------------------------------------------------------------------------------

/**
 * One column of a {@link DataTable}.
 *
 * A column is sortable in whichever mode the table is in, and the two are declared separately
 * because they are different capabilities: `sortValue` reads the value out of a row *here*, while
 * `sortField` names a field the *server* knows how to order by. A column can have one, both or
 * neither — and a column whose value is resolved from another table (a tax class's name, say) can
 * only ever have `sortValue`, which is why the distinction is a prop and not an inference.
 */
export type Column<T> = {
  /** Stable key, also the sort identity. */
  key: string;
  /** Already-translated header text. */
  header: string;
  /** Renders the cell for a row. */
  cell: (row: T) => JSX.Element;
  /** When present, the column is sortable client-side on this value. Ignored in server mode. */
  sortValue?: (row: T) => string | number;
  /**
   * The server's own name for this column's sort field, when it has one.
   *
   * Server mode sorts by handing this token back through `onSort`; the route validates it against
   * its own closed set of sortable fields. A column without one is not sortable in server mode, even
   * if it has a `sortValue` — because sorting the page would order 25 rows as if they were the set.
   */
  sortField?: string;
  /** Extra classes for the header and cells (e.g. `font-mono`). */
  class?: string;
};

/**
 * A table over `rows` with client-side column sort and, optionally, a search box (`searchText`) and
 * pagination (`pageSize`).
 *
 * # Two paging modes, and the difference matters
 *
 * **Client-side** (the default): `rows` holds everything, and `pageSize` slices it here. Right-sized
 * for the admin lists' volumes — tens to hundreds of rows per tenant, which is most of them.
 *
 * **Server-side**: pass `serverTotal` and `onPage` as well, and `rows` is understood to be *one page
 * already*. The pager then renders from `serverTotal` and asks the caller for the next page; nothing
 * is sliced or re-sorted locally, because a slice of a slice is wrong and a sort of one page is a lie
 * about the set ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md)). Handed 25 of 812 rows in
 * client mode this component would render "1–25 of 25", which is worse than showing no pager at all
 * — so the two modes are distinguished by a prop rather than inferred.
 *
 * In server mode a header sorts only if the column names a `sortField` *and* `onSort` is given; the
 * click then asks the caller to re-read the set in that order. Without both, headers are inert
 * rather than quietly sorting the page — a bug this component shipped with, since its own docstring
 * promised no local re-sort while `visible()` returned the sorted rows regardless of mode.
 *
 * Its own generic controls — the search box and pager — read their labels from `t()` (every caller
 * would pass the identical strings); the columns' headers and cells are still supplied by the caller.
 */
/**
 * The page size a client-side table takes when its row count has no other ceiling.
 *
 * Measured, not guessed (`bench/measure.mjs`, Chromium, this machine): an unpaged five-column table
 * paints 1,000 rows in 161 ms, 2,000 in 322 ms, 5,000 in 1.6 s and 10,000 in 4.0 s — and 10,000
 * rows behind a page size of 25 in 37 ms, the same as 100 rows unpaged. So a page size is a
 * complete answer to volume here, and virtualising this component would be a large change to the
 * one table every list screen renders in order to solve a problem this constant already solves at a
 * hundred times the expected load.
 *
 * 25 matches the size the server-paged screens already ask for. It is invisible at the volumes these
 * lists actually hold: the pager renders only when the set exceeds the page, so a table of three
 * routing rules looks exactly as it did — this is a ceiling, not a redesign.
 */
export const CLIENT_PAGE_SIZE = 25;

/** The `md` breakpoint, in the one place the table needs to know about it. */
const WIDE_QUERY = "(min-width: 768px)";

/**
 * Whether the viewport is at least `md` wide, as a signal.
 *
 * The table renders **either** its `<table>` **or** its cards, never both behind a `hidden` class.
 * Two renders of the same cell would be two DOM subtrees for one row: duplicate `id`s, duplicate
 * labels for a screen reader walking the whole document, and — for the grids whose cells hold an
 * uncontrolled input — two edit boxes for one value, of which the user can only see one.
 *
 * Falls back to wide when `matchMedia` is missing. That is jsdom under Vitest, where there is no
 * viewport to ask about and the table is the shape the assertions are written against; it is also
 * the safe direction generally, since the table degrades to a horizontal scroll while the cards
 * would simply be the wrong layout on a desktop.
 */
function createIsWide() {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => true;
  }
  const query = window.matchMedia(WIDE_QUERY);
  const [wide, setWide] = createSignal(query.matches);
  const onChange = (event: MediaQueryListEvent) => setWide(event.matches);
  query.addEventListener("change", onChange);
  onCleanup(() => query.removeEventListener("change", onChange));
  return wide;
}

export function DataTable<T>(props: {
  columns: readonly Column<T>[];
  rows: readonly T[];
  empty: JSX.Element;
  actions?: (row: T) => JSX.Element;
  actionsHeader?: string;
  /**
   * When present, a search box appears above the table.
   *
   * Without `onQuery` it filters `rows` here, case-insensitively, on this text. With `onQuery` the
   * box is the caller's to answer and this function is not consulted — filtering locally in server
   * mode would narrow the *page* rather than the set, the same lie the local sort would tell. Pass
   * it anyway: it is what makes the box appear, and a caller that later drops `onQuery` gets the
   * local behaviour back rather than a box that does nothing.
   */
  searchText?: (row: T) => string;
  /** When greater than 0, rows are paginated at this page size, with a pager below the table. */
  pageSize?: number;
  /**
   * Total rows in the whole set, when the server paged it. Switches the pager to server mode.
   *
   * Requires `onPage`, and `pageSize` to be the limit that was asked for.
   */
  serverTotal?: number;
  /** Asks the caller to load the page starting at `offset`. Server mode only. */
  onPage?: (offset: number) => void;
  /**
   * Asks the caller to re-read the set ordered by `field`, which is a column's `sortField`.
   *
   * Its presence is what makes headers sort in server mode. The caller is expected to reset to the
   * first page: a page-four offset means nothing once the order changes.
   */
  onSort?: (field: string, descending: boolean) => void;
  /**
   * Asks the caller to re-read the set matching `text`, which is ADR-0098's `?q=`.
   *
   * Its presence is what makes the search box server-backed. The caller is expected to reset to the
   * first page — a page-four offset means nothing once the set it indexed into has changed — and to
   * debounce: this fires on every keystroke, because the component cannot know what a request costs
   * the caller.
   */
  onQuery?: (text: string) => void;
  /**
   * Row height. `comfortable` (the default) is what every screen has rendered until now.
   *
   * `compact` is for a **reference grid** — a table whose cells are values to read across, like the
   * translation grid or the tax-rate matrix, where fitting one more row on screen is worth more than
   * the breathing room. It is not a way to shrink a row that holds controls: the kit's `Button`
   * carries its own 44 px touch minimum, so a row with actions in it does not get shorter, it just
   * loses the padding around the buttons.
   *
   * (The roadmap's V18 line asks for "compact at 44 px". That describes a *comfortable* row; this
   * table's default is already 36 px, so the density worth adding was the tighter one, and the
   * touch floor stayed where it belongs — on the controls, not on the row.)
   */
  density?: "comfortable" | "compact";
}) {
  const [sortKey, setSortKey] = createSignal<string | null>(null);
  const [ascending, setAscending] = createSignal(true);
  const [query, setQuery] = createSignal("");
  const [page, setPage] = createSignal(0);

  /**
   * Whether the search box is answered by the server rather than by filtering `rows` here.
   *
   * Keyed on `onQuery` alone, the same way `serverSorted` keys on `onSort`: a caller that offers it
   * has taken responsibility for the box, and one that does not gets the local filter.
   */
  const serverQueried = () => props.onQuery !== undefined;

  const filtered = createMemo(() => {
    const needle = query().trim().toLowerCase();
    const text = props.searchText;
    // In server mode `rows` is already what matched. Filtering it again would narrow the page rather
    // than the set — the same lie re-sorting a page would tell.
    if (serverQueried() || !needle || !text) {
      return props.rows;
    }
    return props.rows.filter((row) => text(row).toLowerCase().includes(needle));
  });

  /**
   * Whether a header click is answered by the server rather than by re-sorting `rows` here.
   *
   * Keyed on `onSort` alone: a caller that paged server-side but has no server sort to offer gets
   * inert headers, which is the honest rendering of "this table cannot sort the set".
   *
   * **Declared before `sorted`, and it has to stay there.** `createMemo` runs its body once at
   * creation, so a `const` the body calls must already be initialised by then. This lived below
   * `sorted` and every table in the console threw `Cannot access 'serverSorted' before
   * initialization` on its first render — no list loaded a row, and `tsc` cannot see it because
   * the reference is legal TypeScript and only the runtime order is wrong.
   */
  const serverSorted = () => props.onSort !== undefined;

  const sorted = createMemo(() => {
    const key = sortKey();
    const base = filtered();
    // The server already ordered the set this page came out of. Re-sorting here would reorder the
    // window as if it were the whole thing — the lie this component's docstring disclaims.
    if (serverSorted() || key === null) {
      return base;
    }
    const value = props.columns.find((column) => column.key === key)?.sortValue;
    if (!value) {
      return base;
    }
    const direction = ascending() ? 1 : -1;
    return [...base].sort((a, b) => {
      const av = value(a);
      const bv = value(b);
      if (typeof av === "number" && typeof bv === "number") {
        return (av - bv) * direction;
      }
      return String(av).localeCompare(String(bv)) * direction;
    });
  });

  // Server mode needs both halves: a total to count with and a way to ask for the next page. One
  // without the other is a caller mistake, and treating it as server mode would render a pager whose
  // buttons do nothing.
  const serverPaged = () => props.serverTotal !== undefined && props.onPage !== undefined;
  const perPage = () => props.pageSize ?? 0;
  const total = () => (serverPaged() ? (props.serverTotal ?? 0) : sorted().length);
  const pageCount = () => (perPage() > 0 ? Math.max(1, Math.ceil(total() / perPage())) : 1);
  // Clamp defensively: the row set can shrink under a page (a delete, a tighter search).
  const current = () => Math.min(page(), pageCount() - 1);
  const visible = createMemo(() => {
    // In server mode `rows` *is* the page. Slicing it again would drop rows the server already
    // selected, and sorting it would order one page as if it were the set.
    if (serverPaged() || perPage() <= 0) {
      return sorted();
    }
    const start = current() * perPage();
    return sorted().slice(start, start + perPage());
  });
  /**
   * Moves the pager, and in server mode asks the caller to fetch that page.
   *
   * The local `page` signal advances either way, because it is what the range text and the buttons'
   * disabled states read. What differs is whether anything is sliced locally — see `visible`.
   */
  const goToPage = (next: number) => {
    setPage(next);
    if (serverPaged()) {
      props.onPage?.(next * perPage());
    }
  };

  const isWide = createIsWide();
  /** Vertical padding for a header cell and a body cell, from `density`. */
  const rowPadding = () => (props.density === "compact" ? "py-1" : "py-2");

  /** Whether this column's header is a control at all, in the mode the table is in. */
  const sortable = (column: Column<T>) =>
    serverSorted() ? column.sortField !== undefined : column.sortValue !== undefined;

  const toggleSort = (column: Column<T>) => {
    if (!sortable(column)) {
      return;
    }
    const flip = sortKey() === column.key;
    const descending = flip ? ascending() : false;
    if (flip) {
      setAscending((value) => !value);
    } else {
      setSortKey(column.key);
      setAscending(true);
    }
    // The local signals drive the caret only; in server mode the order itself comes back with the
    // rows. `column.sortField` is defined here because `sortable` gates on it in this mode.
    if (serverSorted() && column.sortField !== undefined) {
      props.onSort?.(column.sortField, descending);
    }
  };

  return (
    <div class="flex flex-col gap-3">
      <Show when={props.searchText}>
        <input
          type="search"
          aria-label={t("table.search")}
          placeholder={t("table.search")}
          value={query()}
          onInput={(event) => {
            const text = event.currentTarget.value;
            setQuery(text);
            // Back to the first page either way: an offset indexed into the set the previous search
            // matched, and that set has just changed.
            setPage(0);
            props.onQuery?.(text);
          }}
          class="min-h-touch w-full max-w-xs rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
        />
      </Show>
      <Show when={props.rows.length > 0} fallback={props.empty}>
        <Show
          when={total() > 0}
          fallback={<p class="py-4 text-sm text-ink-muted">{t("table.noMatch")}</p>}
        >
          <Show
            when={isWide()}
            fallback={
              /* Below `md` a table cannot be read: eight of them clipped at phone width with no
                 scroll container at all (V9), and a scroll container would only have made the
                 clipping swipeable. A row becomes a card of label/value pairs — the header text is
                 the label, which is why `Column.header` is a plain translated string and not a
                 node. Sorting and the search box stay above it; the pager stays below. */
              <ul class="flex list-none flex-col gap-2">
                <For each={visible()}>
                  {(row) => (
                    <li class="flex flex-col gap-1 rounded-token border border-line bg-surface p-3 text-sm text-ink">
                      <For each={props.columns}>
                        {(column) => (
                          <div class="flex items-baseline justify-between gap-3">
                            <span class="shrink-0 text-xs font-medium text-ink-muted">
                              {column.header}
                            </span>
                            <span class={`min-w-0 text-right ${column.class ?? ""}`}>
                              {column.cell(row)}
                            </span>
                          </div>
                        )}
                      </For>
                      <Show when={props.actions}>
                        <div class="flex justify-end pt-1">{props.actions?.(row)}</div>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            }
          >
            <div class="overflow-x-auto">
              <table class="w-full text-left text-sm">
                <thead>
                  <tr class="border-b border-line text-ink-muted">
                    <For each={props.columns}>
                      {(column) => (
                        <th class={`${rowPadding()} pr-4 font-medium ${column.class ?? ""}`}>
                          <Show when={sortable(column)} fallback={<span>{column.header}</span>}>
                            <button
                              type="button"
                              class="inline-flex items-center gap-1 font-medium transition-colors hover:text-ink"
                              onClick={() => toggleSort(column)}
                            >
                              <span>{column.header}</span>
                              <Show when={sortKey() === column.key}>
                                <span aria-hidden="true">{ascending() ? "▲" : "▼"}</span>
                              </Show>
                            </button>
                          </Show>
                        </th>
                      )}
                    </For>
                    <Show when={props.actions}>
                      <th class={`${rowPadding()} font-medium`}>{props.actionsHeader}</th>
                    </Show>
                  </tr>
                </thead>
                <tbody>
                  <For each={visible()}>
                    {(row) => (
                      <tr class="border-b border-line text-ink">
                        <For each={props.columns}>
                          {(column) => (
                            <td class={`${rowPadding()} pr-4 ${column.class ?? ""}`}>
                              {column.cell(row)}
                            </td>
                          )}
                        </For>
                        <Show when={props.actions}>
                          <td class={rowPadding()}>{props.actions?.(row)}</td>
                        </Show>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </Show>
          <Pager
            offset={current() * perPage()}
            limit={perPage()}
            total={total()}
            shown={visible().length}
            onOffset={(offset) => goToPage(perPage() > 0 ? Math.floor(offset / perPage()) : 0)}
          />
        </Show>
      </Show>
    </div>
  );
}

// --- Overlays: Modal, Drawer --------------------------------------------------------------------

/** A centred modal dialog. Backdrop click and Escape both close it. */
export function Modal(
  props: ParentProps<{
    open: boolean;
    title: string;
    closeLabel: string;
    onClose: () => void;
    footer?: JSX.Element;
  }>,
) {
  useEscape(
    () => props.open,
    () => props.onClose(),
  );
  return (
    <Show when={props.open}>
      <div
        class="fixed inset-0 z-50 flex items-start justify-center bg-black/40 p-4 pt-24"
        onClick={() => props.onClose()}
      >
        <div
          role="dialog"
          aria-modal="true"
          class="w-full max-w-lg rounded-token border border-line bg-surface shadow-overlay"
          onClick={(event) => event.stopPropagation()}
        >
          <header class="flex items-center justify-between gap-4 border-b border-line px-4 py-3">
            <h2 class="text-lg font-semibold text-ink">{props.title}</h2>
            <button
              type="button"
              aria-label={props.closeLabel}
              class="rounded-token text-ink-muted transition-colors hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
              onClick={() => props.onClose()}
            >
              <span aria-hidden="true">✕</span>
            </button>
          </header>
          <div class="p-4">{props.children}</div>
          <Show when={props.footer}>
            <footer class="flex justify-end gap-2 border-t border-line px-4 py-3">
              {props.footer}
            </footer>
          </Show>
        </div>
      </div>
    </Show>
  );
}

/** A right-hand drawer, for longer edit forms. Backdrop click and Escape both close it. */
export function Drawer(
  props: ParentProps<{
    open: boolean;
    title: string;
    closeLabel: string;
    onClose: () => void;
    footer?: JSX.Element;
  }>,
) {
  useEscape(
    () => props.open,
    () => props.onClose(),
  );
  return (
    <Show when={props.open}>
      <div class="fixed inset-0 z-50 flex justify-end bg-black/40" onClick={() => props.onClose()}>
        <div
          role="dialog"
          aria-modal="true"
          class="flex h-full w-full max-w-md flex-col border-l border-line bg-surface shadow-overlay"
          onClick={(event) => event.stopPropagation()}
        >
          <header class="flex items-center justify-between gap-4 border-b border-line px-4 py-3">
            <h2 class="text-lg font-semibold text-ink">{props.title}</h2>
            <button
              type="button"
              aria-label={props.closeLabel}
              class="rounded-token text-ink-muted transition-colors hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
              onClick={() => props.onClose()}
            >
              <span aria-hidden="true">✕</span>
            </button>
          </header>
          <div class="flex-1 overflow-y-auto p-4">{props.children}</div>
          <Show when={props.footer}>
            <footer class="flex justify-end gap-2 border-t border-line px-4 py-3">
              {props.footer}
            </footer>
          </Show>
        </div>
      </div>
    </Show>
  );
}

// --- FormPanel ------------------------------------------------------------------------------------

/**
 * The one shell every create and edit form lives in
 * ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md) §1).
 *
 * It owns the chrome and nothing else: `Drawer` or `Modal`, the title chosen from the lifecycle's
 * mode, the footer's Cancel and submit, the submit's disabled-while-saving state, the refusal
 * banner, and the guard on closing with unsaved changes. The **fields stay with the screen**, as
 * ordinary `FormField` children — ADR-0121 §2 records why this is a shell rather than a declarative
 * field schema, and the short version is that the floor editor, the layout grid and the translation
 * matrix would all have had to escape a schema.
 *
 * Renders as a drawer by default. `as="modal"` suits a form of about three fields or fewer; a modal
 * does not scroll its body, so a long form in one is a form with a cut-off bottom.
 *
 * `dirty` is optional and, when given, is asked before any close the operator did not ask for by
 * name — Escape, the backdrop, Cancel. The prompt replaces the footer rather than stacking a second
 * dialog: a nested `role="dialog"` inside an `aria-modal` one is a focus-trap argument nobody wins,
 * and the question is small enough to ask in place.
 */
export function FormPanel<T>(
  props: ParentProps<{
    crud: EntityCrud<T>;
    createTitle: string;
    editTitle: string;
    submitLabel: string;
    onSubmit: () => void;
    as?: "drawer" | "modal";
    /** Whether the form holds unsaved input. Omitted means closing never asks. */
    dirty?: () => boolean;
  }>,
) {
  const [confirmingDiscard, setConfirmingDiscard] = createSignal(false);

  // Open for `creating` and `editing` only. `confirming` is the same lifecycle in a different shape
  // and belongs to `ConfirmDialog`, so a panel must not appear over it.
  const open = () => props.crud.mode() === "creating" || props.crud.mode() === "editing";

  const finish = () => {
    setConfirmingDiscard(false);
    props.crud.close();
  };

  // Every close the operator did not spell out goes through here. A save in flight ignores it: the
  // write is already gone to the server, and closing would leave them unable to see how it landed.
  const requestClose = () => {
    if (props.crud.saving()) {
      return;
    }
    if (props.dirty?.()) {
      setConfirmingDiscard(true);
      return;
    }
    finish();
  };

  const body = (
    <form
      class="flex flex-col gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        props.onSubmit();
      }}
    >
      {props.children}
      <Show when={props.crud.error()}>
        {(message) => <Banner tone="danger" message={message()} />}
      </Show>
      {/* A submit inside the form, so Enter in any field saves; the footer's button targets it by
          id rather than duplicating the handler, which would let the two drift. */}
      <button type="submit" class="hidden" aria-hidden="true" tabindex={-1} />
    </form>
  );

  const footer = (
    <Show
      when={confirmingDiscard()}
      fallback={
        <>
          <Button variant="secondary" onClick={requestClose} disabled={props.crud.saving()}>
            {t("action.cancel")}
          </Button>
          <Button onClick={() => props.onSubmit()} disabled={props.crud.saving()}>
            {props.crud.saving() ? t("common.saving") : props.submitLabel}
          </Button>
        </>
      }
    >
      <div class="flex w-full items-center justify-between gap-4">
        <p class="text-sm text-ink">{t("form.discardPrompt")}</p>
        <div class="flex gap-2">
          <Button variant="secondary" onClick={() => setConfirmingDiscard(false)}>
            {t("form.keepEditing")}
          </Button>
          <Button variant="danger" onClick={finish}>
            {t("form.discard")}
          </Button>
        </div>
      </div>
    </Show>
  );

  const title = () => (props.crud.mode() === "editing" ? props.editTitle : props.createTitle);

  return (
    <Show
      when={props.as === "modal"}
      fallback={
        <Drawer
          open={open()}
          title={title()}
          closeLabel={t("action.close")}
          onClose={requestClose}
          footer={footer}
        >
          {body}
        </Drawer>
      }
    >
      <Modal
        open={open()}
        title={title()}
        closeLabel={t("action.close")}
        onClose={requestClose}
        footer={footer}
      >
        {body}
      </Modal>
    </Show>
  );
}

// --- ConfirmDialog --------------------------------------------------------------------------------

/**
 * A confirm/cancel dialog for a mutating action. For a high-risk action (delete, revoke), pass
 * `typeToConfirm` (e.g. the entity's name) and `typePrompt`: the confirm button stays disabled until
 * the operator types the value exactly, so a destructive click is never a single slip.
 *
 * `typePrompt` is not optional in practice — omitting it ships a silently unlabelled input, and no
 * lint catches that, because the i18n lint only sees string literals.
 */
export function ConfirmDialog(props: {
  open: boolean;
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
  closeLabel: string;
  busy?: boolean;
  danger?: boolean;
  typeToConfirm?: string;
  typePrompt?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [typed, setTyped] = createSignal("");
  // Cleared every time the dialog opens. The input is rendered inside `Modal`, which mounts its
  // children only while open — but this signal lives in the component body, and the component is
  // mounted permanently on the screen. So without this, cancelling after typing the name and then
  // reopening the dialog (for the same entity or a different one) finds the confirm button already
  // enabled, and the typed-name guard has bought nothing at all.
  createEffect(() => {
    if (props.open) {
      setTyped("");
    }
  });
  // The target is trimmed as well as the input: a store or device name that arrives with trailing
  // whitespace would otherwise be untypeable, and an operator cannot see the difference.
  const ready = () =>
    !props.typeToConfirm || typed().trim() === props.typeToConfirm.trim();
  return (
    <Modal
      open={props.open}
      title={props.title}
      closeLabel={props.closeLabel}
      onClose={props.onCancel}
      footer={
        <>
          <Button variant="secondary" onClick={() => props.onCancel()}>
            {props.cancelLabel}
          </Button>
          <Button
            variant={props.danger ? "danger" : "primary"}
            disabled={props.busy || !ready()}
            onClick={() => props.onConfirm()}
          >
            {props.confirmLabel}
          </Button>
        </>
      }
    >
      <p class="text-sm text-ink">{props.message}</p>
      <Show when={props.typeToConfirm}>
        <div class="mt-3">
          <TextField label={props.typePrompt ?? ""} value={typed()} onInput={setTyped} />
        </div>
      </Show>
    </Modal>
  );
}

// --- Small display primitives ---------------------------------------------------------------------

/** The friendly empty-list panel: a headline, an optional line of guidance, and an optional action. */
export function EmptyState(props: { title: string; description?: string; action?: JSX.Element }) {
  return (
    <div class="flex flex-col items-center gap-2 py-8 text-center">
      <p class="text-sm font-medium text-ink">{props.title}</p>
      <Show when={props.description}>
        <p class="text-sm text-ink-muted">{props.description}</p>
      </Show>
      <Show when={props.action}>
        <div class="mt-2">{props.action}</div>
      </Show>
    </div>
  );
}

/**
 * A collapsed disclosure for the technical identifiers (ULIDs) a normal operator never needs but a
 * support engineer sometimes does — present, copy-able, but out of the way (Track F2).
 */
export function TechnicalDetails(props: ParentProps<{ label: string }>) {
  return (
    <details class="text-xs text-ink-muted">
      <summary class="cursor-pointer select-none">{props.label}</summary>
      <div class="mt-1 break-all font-mono">{props.children}</div>
    </details>
  );
}

/** A labelled control with an optional field error, wired for assistive tech via `role="alert"`. */
export function FormField(props: ParentProps<{ label: string; error?: string }>) {
  return (
    <div>
      <span class="mb-1 block text-sm font-medium text-ink">{props.label}</span>
      {props.children}
      <Show when={props.error}>
        {(message) => <p role="alert" class="mt-1 text-xs text-danger">{message()}</p>}
      </Show>
    </div>
  );
}

/**
 * A reorder list with two equivalent controls: each row carries up/down buttons (the keyboard-
 * accessible path), and the whole row is a native drag source (the pointer path, layered on with the
 * Layout rebuild, F3). Both call `onReorder(from, to)`. The buttons stay as the guaranteed fallback,
 * so a keyboard-only or assistive-tech user is never dependent on the drag gesture.
 */
export function ReorderList<T>(props: {
  items: readonly T[];
  itemKey: (item: T) => string;
  renderItem: (item: T) => JSX.Element;
  onReorder: (from: number, to: number) => void;
  upLabel: string;
  downLabel: string;
}) {
  const [dragIndex, setDragIndex] = createSignal<number | null>(null);
  const [overIndex, setOverIndex] = createSignal<number | null>(null);

  const drop = (to: number) => {
    const from = dragIndex();
    if (from !== null && from !== to) {
      props.onReorder(from, to);
    }
    setDragIndex(null);
    setOverIndex(null);
  };

  return (
    <ul class="flex flex-col gap-1">
      <For each={props.items}>
        {(item, index) => (
          <li
            draggable={true}
            onDragStart={(event) => {
              setDragIndex(index());
              if (event.dataTransfer) {
                event.dataTransfer.effectAllowed = "move";
              }
            }}
            onDragOver={(event) => {
              event.preventDefault();
              setOverIndex(index());
            }}
            onDrop={(event) => {
              event.preventDefault();
              drop(index());
            }}
            onDragEnd={() => {
              setDragIndex(null);
              setOverIndex(null);
            }}
            class={`flex items-center gap-2 rounded-token border bg-surface-raised px-2 py-1 ${
              dragIndex() === index() ? "opacity-50" : ""
            } ${overIndex() === index() && dragIndex() !== null && dragIndex() !== index() ? "border-accent" : "border-line"}`}
          >
            <div class="flex flex-col">
              <button
                type="button"
                aria-label={props.upLabel}
                disabled={index() === 0}
                class="rounded-token text-ink-muted transition-colors hover:text-ink focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-30"
                onClick={() => props.onReorder(index(), index() - 1)}
              >
                <span aria-hidden="true">▲</span>
              </button>
              <button
                type="button"
                aria-label={props.downLabel}
                disabled={index() === props.items.length - 1}
                class="rounded-token text-ink-muted transition-colors hover:text-ink focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-30"
                onClick={() => props.onReorder(index(), index() + 1)}
              >
                <span aria-hidden="true">▼</span>
              </button>
            </div>
            <div class="flex-1">{props.renderItem(item)}</div>
          </li>
        )}
      </For>
    </ul>
  );
}

// --- The pieces the run found missing (Wave 4 · PR-3) --------------------------------------------

/**
 * A row of tabs that switches what is shown below it.
 *
 * Three screens had grown a hand-rolled version — the catalogue's sub-screens, the wizard's steps,
 * the alerts' active/recent split — and each one drew its selected state differently, so the same
 * control taught the operator three different rules about which tab they were on. Keyboard support
 * is the part a hand-rolled tab strip always misses: arrow keys move between tabs, which is what
 * `role="tablist"` promises a screen reader.
 *
 * Controlled: the caller owns `active`, because a tab is usually part of the URL.
 */
export function Tabs<K extends string>(props: {
  tabs: readonly { readonly key: K; readonly label: string }[];
  active: K;
  onSelect: (key: K) => void;
  label: string;
}) {
  const move = (from: number, step: number) => {
    const count = props.tabs.length;
    const next = props.tabs[(from + step + count) % count];
    if (next) {
      props.onSelect(next.key);
    }
  };
  return (
    <div role="tablist" aria-label={props.label} class="flex flex-wrap gap-1 border-b border-line">
      <For each={props.tabs}>
        {(tab, index) => (
          <button
            type="button"
            role="tab"
            aria-selected={props.active === tab.key}
            // The unselected tabs stay out of the tab order: one Tab press enters the strip, then
            // the arrows move within it. That is the ARIA pattern, and it is also what stops a
            // twelve-tab catalogue from costing twelve presses to step past.
            tabindex={props.active === tab.key ? 0 : -1}
            onClick={() => props.onSelect(tab.key)}
            onKeyDown={(event) => {
              if (event.key === "ArrowRight") {
                move(index(), 1);
              } else if (event.key === "ArrowLeft") {
                move(index(), -1);
              } else if (event.key === "Home") {
                const first = props.tabs[0];
                if (first) {
                  props.onSelect(first.key);
                }
              } else if (event.key === "End") {
                const last = props.tabs[props.tabs.length - 1];
                if (last) {
                  props.onSelect(last.key);
                }
              }
            }}
            class={`-mb-px border-b-2 px-3 py-2 text-sm font-medium transition-colors ${
              props.active === tab.key
                ? "border-accent text-ink"
                : "border-transparent text-ink-muted hover:text-ink"
            }`}
          >
            {tab.label}
          </button>
        )}
      </For>
    </div>
  );
}

/**
 * One headline figure with its label, and optionally a word of support beneath it.
 *
 * The store hub's six numbers were paragraphs — "Revenue today: 4,120,000 ₫" in body text, six
 * times — so nothing on the screen an operator opens first was readable at a glance. A KPI is a
 * number first and a label second, which is the whole difference.
 *
 * `tone` is the same vocabulary `lib/posture.ts` speaks, so a card's hue comes from the rules that
 * decide what the figure *means* rather than from the screen's own opinion.
 */
export function KpiTile(props: {
  label: string;
  value: string;
  support?: string;
  tone?: "ok" | "attention" | "idle" | "plain";
  action?: JSX.Element;
}) {
  const hue = () => {
    switch (props.tone) {
      case "ok":
        return "text-ok";
      case "attention":
        return "text-danger";
      case "idle":
        return "text-ink-muted";
      default:
        return "text-ink";
    }
  };
  return (
    <div class="flex flex-col gap-1 rounded-token border border-line bg-surface p-4 shadow-raised">
      <span class="text-sm text-ink-muted">{props.label}</span>
      <span class={`tabular text-xl font-semibold ${hue()}`}>{props.value}</span>
      <Show when={props.support}>
        <span class="text-sm text-ink-muted">{props.support}</span>
      </Show>
      <Show when={props.action}>
        <div class="mt-1">{props.action}</div>
      </Show>
    </div>
  );
}

/**
 * The filter row above a list: the controls that narrow it, and — trailing — the buttons that act on
 * what they narrowed it to.
 *
 * Written now rather than in PR-3 because until three screens had rolled the same row by hand there
 * was nothing to generalise from, and the kit-adoption gate is pointed at exactly that mistake. The
 * three are Audit (`mb-4 grid gap-3 sm:grid-cols-3`), Reports' X/Z panel and Catalog → Items
 * (`mb-4 flex flex-wrap items-end gap-4` and `…gap-2`) — the same intent at three spellings, which
 * is how a console stops looking like one console.
 *
 * The split is by *role*, not by position: a control that changes which rows are in the set goes in
 * `children`, and one that acts on the set as it stands (Search, Clear, View) goes in `trailing`.
 * That is why Items' Search button is trailing while its text box is not — pressing Search does not
 * narrow anything, it asks the server for what the box already says.
 */
export function Toolbar(props: ParentProps<{ trailing?: JSX.Element }>) {
  return (
    <div class="mb-4 flex flex-col gap-3 sm:flex-row sm:items-end">
      <div class="grid flex-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">{props.children}</div>
      <Show when={props.trailing}>
        <div class="flex flex-wrap items-end gap-2">{props.trailing}</div>
      </Show>
    </div>
  );
}

/**
 * A row's actions behind one kebab.
 *
 * The run counted rows carrying up to five inline buttons — Edit, Archive, Revoke, Details, Resend
 * — which is a table whose widest column is its verbs. Folded into a menu the row is its data
 * again, and the destructive entry can be named plainly rather than shortened to fit.
 *
 * Closes on Escape and on a click outside, both of which a menu that only closes on its own button
 * fails at the moment a second row's menu is opened.
 */
export function RowActions(props: { label: string; children: JSX.Element }) {
  const [open, setOpen] = createSignal(false);
  let container: HTMLDivElement | undefined;

  useEscape(open, () => setOpen(false));

  createEffect(() => {
    if (!open()) {
      return;
    }
    const dismiss = (event: MouseEvent) => {
      if (container && !container.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", dismiss);
    onCleanup(() => document.removeEventListener("mousedown", dismiss));
  });

  return (
    <div class="relative" ref={container}>
      <Button
        variant="ghost"
        size="sm"
        aria-label={props.label}
        aria-expanded={open()}
        aria-haspopup="true"
        onClick={() => setOpen((value) => !value)}
      >
        <span aria-hidden="true">⋯</span>
      </Button>
      <Show when={open()}>
        {/* `onClick` on the panel rather than on each entry: every entry closes the menu, and a
            caller that had to remember to close it would eventually forget on one row. */}
        <div
          role="menu"
          onClick={() => setOpen(false)}
          class="absolute right-0 z-20 mt-1 flex min-w-40 flex-col items-stretch gap-1 rounded-token border border-line bg-surface p-1 shadow-overlay"
        >
          {props.children}
        </div>
      </Show>
    </div>
  );
}

/**
 * A date field that says which clock it means.
 *
 * Every date control in the console was a bare `<input type="date">`, which reads in the browser's
 * timezone and says nothing about it. A report run from Tokyo for a Ho Chi Minh City store was a
 * different day's takings than the same date typed in the shop, and nothing on screen admitted
 * that. `zone` prints the store's IANA name beside the field when the caller knows it — the caller
 * knows it because it is the store's published `locale.timezone`.
 */
export function DateField(props: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  zone?: string;
  min?: string;
  max?: string;
}) {
  return (
    <label class="block">
      <span class="mb-1 block text-sm font-medium text-ink">{props.label}</span>
      <input
        type="date"
        value={props.value}
        min={props.min}
        max={props.max}
        onInput={(event) => props.onChange(event.currentTarget.value)}
        class="h-10 w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
      />
      <Show when={props.zone}>
        {(zone) => <span class="mt-1 block text-xs text-ink-muted">{zone()}</span>}
      </Show>
    </label>
  );
}

/**
 * A from/to pair that cannot be given backwards.
 *
 * Both Reports and Campaigns built one out of two `DateField`s and neither checked the order, so
 * "from the 30th to the 1st" was an accepted query that returned nothing and explained nothing. The
 * pair clamps: moving `from` past `to` carries `to` with it, and the reverse.
 */
export function DateRange(props: {
  fromLabel: string;
  toLabel: string;
  from: string;
  to: string;
  onChange: (from: string, to: string) => void;
  zone?: string;
}) {
  return (
    <div class="flex flex-wrap items-start gap-3">
      <DateField
        label={props.fromLabel}
        value={props.from}
        max={props.to || undefined}
        zone={props.zone}
        onChange={(from) => props.onChange(from, props.to && from > props.to ? from : props.to)}
      />
      <DateField
        label={props.toLabel}
        value={props.to}
        min={props.from || undefined}
        onChange={(to) => props.onChange(props.from && to < props.from ? to : props.from, to)}
      />
    </div>
  );
}

/**
 * Where a published node stands relative to the screen's own data.
 *
 * Three answers and no fourth: it has never been published; it is published and nothing has changed
 * since; or it is published and the authoring data has moved on. The last one is the whole point —
 * a screen that says "Published 3 June" and nothing else cannot tell an operator whether the edit
 * they made this morning is on the shop floor.
 */
export type PublishState = "never" | "published" | "stale";

/**
 * Which of the three a node is in.
 *
 * `editedAtMs` is the screen's own data — the newest `updated_at` of whatever it authors — and is
 * allowed to be null: a screen that cannot tell when its data last changed says so by passing null,
 * and gets `published` rather than a guess. Equal timestamps are **not** stale: a publish writes
 * its own node, so the two clocks agreeing means the publish is the newer event.
 */
export function publishState(
  publishedAtMs: number | null | undefined,
  editedAtMs: number | null | undefined,
): PublishState {
  if (publishedAtMs === null || publishedAtMs === undefined) {
    return "never";
  }
  if (editedAtMs === null || editedAtMs === undefined) {
    return "published";
  }
  return editedAtMs > publishedAtMs ? "stale" : "published";
}

/**
 * One publish control, with what it publishes to and where that stands (finding **F13**).
 *
 * Thirteen screens had written their own: a button, a sentence, sometimes a store name, sometimes a
 * warning, never the same two the same way, and not one of them said whether the thing in front of
 * the operator was already on the shop floor. The run found an operator pressing Publish twice
 * because the screen gave them no way to tell.
 *
 * The bar renders the target, the state, an optional preview, and the button. It does **not** make
 * the call: `onPublish` is the screen's, because a publish is a domain write with a domain's
 * arguments and putting thirteen of those behind one component means a switch on node keys inside
 * the kit. What is shared is the *rendering and the state machine*, which is what was inconsistent.
 *
 * `describe` turns a state into the screen's own words, so every message key stays in the screen and
 * every rule stays here.
 */
/**
 * **Preview**, beside Publish: what this publish would change, before it changes it (F11).
 *
 * The console had exactly one of these — campaigns — and every other publish button committed
 * blind. This is the generic one, and it is not exported: a bar is where a publish happens, so
 * {@link PublishBar} mounts it from its own `preview` prop and a screen adds a dry run by handing
 * over one function. The dialog's own **Publish** is the bar's, because an operator who has just
 * read the diff saying yes to it should not have to find the button again.
 *
 * `load` is the screen's own call, because only the screen knows the node's arguments. It returns
 * `null` when there is nothing to preview yet — no store chosen, no menu picked — and the dialog
 * stays shut. A failure is the screen's to report through `toast`, in its own words, for the same
 * reason its publish failures are: the kit carries no messages of its own beyond these generic ones.
 *
 * The diff is rendered as the merge patch the cloud computed, not prose. An operator reading
 * "menu.items.0.price: 45000" is reading the document the shop will receive, which is the thing
 * worth checking; a summary would be this component's opinion of it.
 */
function PublishPreview(props: {
  load: () => Promise<NodePreview | null>;
  /** Blocked for the same reasons Publish is — no store, no permission, a write in flight. */
  disabled?: boolean;
  /** Publish, from inside the dialog: the operator has just read the diff and is saying yes to it. */
  publishLabel: string;
  onPublish: () => void;
}) {
  const [preview, setPreview] = createSignal<NodePreview | null>(null);
  const [loading, setLoading] = createSignal(false);
  const run = async () => {
    setLoading(true);
    try {
      setPreview(await props.load());
    } finally {
      setLoading(false);
    }
  };
  return (
    <>
      <Button variant="secondary" disabled={props.disabled || loading()} onClick={() => void run()}>
        {loading() ? t("publish.previewLoading") : t("publish.preview")}
      </Button>
      <Modal
        open={preview() !== null}
        title={t("publish.previewTitle")}
        closeLabel={t("action.close")}
        onClose={() => setPreview(null)}
        footer={
          <Button
            disabled={props.disabled}
            onClick={() => {
              setPreview(null);
              props.onPublish();
            }}
          >
            {props.publishLabel}
          </Button>
        }
      >
        <Show when={preview()}>
          {(result) => (
            <div class="flex flex-col gap-3">
              <p class="text-sm text-ink-muted">
                {result().from_version_id
                  ? t("publish.previewFrom", { version: result().from_version_id ?? "" })
                  : t("publish.previewFirst")}
              </p>
              <Show
                when={!result().unchanged}
                fallback={<Banner tone="ok" message={t("publish.previewUnchanged")} />}
              >
                <div class="overflow-x-auto rounded-token border border-line bg-surface-raised p-3">
                  <pre class="whitespace-pre text-sm text-ink">
                    {JSON.stringify(result().diff, null, 2)}
                  </pre>
                </div>
              </Show>
            </div>
          )}
        </Show>
      </Modal>
    </>
  );
}

/**
 * A labelled group of fields inside a form (V16).
 *
 * A settings form of fifteen controls is not a list, it is three or four subjects — where the shop
 * is, how it counts a day, what prints on a receipt — and an operator looking for one of them should
 * not have to read the other twelve labels to find out which. The heading is the answer.
 *
 * Deliberately not a `Card`: these are divisions *within* one card, and a card per group would say
 * they can be saved separately, which they cannot — one publish writes the whole node.
 */
export function FormSection(props: ParentProps<{ title: string; hint?: string }>) {
  return (
    <section class="flex flex-col gap-4 border-t border-line pt-4 first:border-t-0 first:pt-0">
      <div class="flex flex-col gap-1">
        <h3 class="text-sm font-semibold text-ink">{props.title}</h3>
        <Show when={props.hint}>{(hint) => <p class="text-sm text-ink-muted">{hint()}</p>}</Show>
      </div>
      {props.children}
    </section>
  );
}

/**
 * The form's write control, kept reachable (V16).
 *
 * On a long form the publish bar sits below the fold, so an operator who has changed the second
 * field scrolls past thirteen they did not touch to reach it — and the scroll is where "did I
 * publish that?" comes from. This pins it to the bottom of the viewport while the form is on screen,
 * on the surface colour with a top rule, so it reads as belonging to the form rather than floating
 * over it.
 *
 * `position: sticky` rather than `fixed`: it stays inside its card, so it scrolls away with the card
 * it belongs to instead of hovering over the next one.
 */
export function StickyActions(props: ParentProps) {
  return (
    <div class="sticky bottom-0 -mx-4 mt-2 border-t border-line bg-surface px-4 py-3">
      {props.children}
    </div>
  );
}

export function PublishBar(props: {
  /** What is being published, in the operator's words — "Tax rates", "The menu". */
  label: string;
  /** When this node was last published, from `GET /admin/stores/{id}/config/nodes`. */
  publishedAtMs?: number | null;
  /** When the screen's own authored data last changed, or null when it cannot tell. */
  editedAtMs?: number | null;
  /** The state in the screen's words. */
  describe: (state: PublishState, publishedAtMs: number | null) => string;
  publishLabel: string;
  busy?: boolean;
  /** Set when publishing is not possible at all — no store chosen, no permission. */
  disabled?: boolean;
  /** Why it is disabled, said out loud rather than left to a greyed button. */
  disabledReason?: string;
  /**
   * The dry run this bar offers before the write (roadmap-v3 **F11**).
   *
   * The screen's own call, because only it knows the node's arguments; it returns `null` when there
   * is nothing to preview yet, and reports its own failures through `toast`. Set it and the bar
   * grows a **Preview changes** button beside Publish; leave it unset and the bar is what it was.
   */
  preview?: () => Promise<NodePreview | null>;
  /**
   * A way to put this node into a release instead of publishing it now
   * ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md), L1).
   *
   * A link rather than a second write, and the reason is the record's: a release carries a *moment*
   * and a *target*, and neither exists on this bar — it knows one store and one node. A button here
   * that scheduled would have to invent both, which is a worse version of the publish centre the
   * link goes to. So the bar hands the operator over with the node already chosen.
   *
   * Set only by screens whose node a release can carry. A node taken from a named source store
   * cannot be in a release at all (the source's value could move between the choosing and the
   * firing), and `locale` and `store_profile` are per-store by definition, so those bars leave it
   * unset and show one action, which is the honest count of what they can do.
   */
  addToRelease?: { href: string; label: string };
  onPublish: () => void;
  /**
   * The step gate's handle on the Publish button, for a flow that declares this click
   * (`scripts/step-tasks.mjs`).
   */
  "data-step"?: string;
  /**
   * The step gate's outcome mark, rendered on the freshness line **only once the node has been
   * published at least once**. That is what makes it an outcome rather than furniture: a store that
   * has never had this node has no such element, so a browser waiting on it is waiting for the
   * publish to have landed, not for the bar to have rendered.
   */
  "data-outcome"?: string;
}) {
  const state = () => publishState(props.publishedAtMs, props.editedAtMs);
  return (
    <div class="flex flex-col gap-2 rounded-token border border-line bg-surface-raised p-3">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <div class="flex flex-col gap-0.5">
          <span class="text-sm font-medium text-ink">{props.label}</span>
          <span
            data-outcome={
              props.publishedAtMs === null || props.publishedAtMs === undefined
                ? undefined
                : props["data-outcome"]
            }
            class={`text-sm ${state() === "stale" ? "text-danger" : "text-ink-muted"}`}
          >
            {props.describe(state(), props.publishedAtMs ?? null)}
          </span>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          <Show when={props.addToRelease}>
            {(release) => (
              <a
                class="min-h-touch inline-flex items-center rounded-token border border-line px-3 text-sm text-ink focus-visible:outline-2 focus-visible:outline-accent"
                href={release().href}
              >
                {release().label}
              </a>
            )}
          </Show>
          <Show when={props.preview}>
            {(load) => (
              <PublishPreview
                load={load()}
                disabled={props.busy || props.disabled}
                publishLabel={props.publishLabel}
                onPublish={() => props.onPublish()}
              />
            )}
          </Show>
          <Button
            data-step={props["data-step"]}
            disabled={props.busy || props.disabled}
            onClick={() => props.onPublish()}
          >
            {props.publishLabel}
          </Button>
        </div>
      </div>
      <Show when={props.disabled && props.disabledReason}>
        {(reason) => <p class="text-sm text-ink-muted">{reason()}</p>}
      </Show>
    </div>
  );
}
