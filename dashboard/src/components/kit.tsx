// The CRUD kit (ADR-0060, Track F2): the reusable table / form / dialog / status primitives the
// master-data screens are built from, so each screen stops hand-rolling a table, a delete
// confirmation, and an empty state. Like `components/ui.tsx`, every component is built from the
// design tokens and carries NO user-visible text of its own — labels, headers and messages are
// passed in already translated by the caller, so the no-hardcoded-strings lint (ADR-0020) has
// nothing to flag here. Write outcomes are surfaced through the F1 `toast` primitive by the caller.

import {
  createMemo,
  createSignal,
  For,
  type JSX,
  onCleanup,
  onMount,
  type ParentProps,
  Show,
} from "solid-js";

import { t } from "../i18n";
import { Button, TextField } from "./ui";

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

  /**
   * Whether a header click is answered by the server rather than by re-sorting `rows` here.
   *
   * Keyed on `onSort` alone: a caller that paged server-side but has no server sort to offer gets
   * inert headers, which is the honest rendering of "this table cannot sort the set".
   */
  const serverSorted = () => props.onSort !== undefined;

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
          <div class="overflow-x-auto">
            <table class="w-full text-left text-sm">
              <thead>
                <tr class="border-b border-line text-ink-muted">
                  <For each={props.columns}>
                    {(column) => (
                      <th class={`py-2 pr-4 font-medium ${column.class ?? ""}`}>
                        <Show when={sortable(column)} fallback={<span>{column.header}</span>}>
                          <button
                            type="button"
                            class="inline-flex items-center gap-1 font-medium hover:text-ink"
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
                    <th class="py-2 font-medium">{props.actionsHeader}</th>
                  </Show>
                </tr>
              </thead>
              <tbody>
                <For each={visible()}>
                  {(row) => (
                    <tr class="border-b border-line text-ink">
                      <For each={props.columns}>
                        {(column) => (
                          <td class={`py-2 pr-4 ${column.class ?? ""}`}>{column.cell(row)}</td>
                        )}
                      </For>
                      <Show when={props.actions}>
                        <td class="py-2">{props.actions?.(row)}</td>
                      </Show>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
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

function useEscape(isOpen: () => boolean, close: () => void): void {
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape" && isOpen()) {
      close();
    }
  };
  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));
}

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
          class="w-full max-w-lg rounded-token border border-line bg-surface shadow-lg"
          onClick={(event) => event.stopPropagation()}
        >
          <header class="flex items-center justify-between gap-4 border-b border-line px-4 py-3">
            <h2 class="text-lg font-semibold text-ink">{props.title}</h2>
            <button
              type="button"
              aria-label={props.closeLabel}
              class="text-ink-muted hover:text-ink"
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
          class="flex h-full w-full max-w-md flex-col border-l border-line bg-surface shadow-lg"
          onClick={(event) => event.stopPropagation()}
        >
          <header class="flex items-center justify-between gap-4 border-b border-line px-4 py-3">
            <h2 class="text-lg font-semibold text-ink">{props.title}</h2>
            <button
              type="button"
              aria-label={props.closeLabel}
              class="text-ink-muted hover:text-ink"
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

// --- ConfirmDialog --------------------------------------------------------------------------------

/**
 * A confirm/cancel dialog for a mutating action. For a high-risk action (delete, revoke), pass
 * `typeToConfirm` (e.g. the entity's name) and `typePrompt`: the confirm button stays disabled until
 * the operator types the value exactly, so a destructive click is never a single slip.
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
  const ready = () => !props.typeToConfirm || typed().trim() === props.typeToConfirm;
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

/** A coloured status pill. `label` is already-translated text. `danger` is for an active fault
 *  (a firing alert, a critical severity): `text-danger` clears AA on the card surface, and the label
 *  always rides with the hue, so meaning is never carried by colour alone. */
export function StatusBadge(props: {
  label: string;
  tone: "active" | "archived" | "disabled" | "neutral" | "danger";
}) {
  const palette = () => {
    switch (props.tone) {
      case "active":
        return "border-ok text-ok";
      case "danger":
        return "border-danger text-danger";
      case "archived":
      case "disabled":
        return "border-ink-muted text-ink-muted";
      default:
        return "border-line text-ink-muted";
    }
  };
  return (
    <span
      class={`inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium ${palette()}`}
    >
      {props.label}
    </span>
  );
}

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
                class="text-ink-muted hover:text-ink disabled:opacity-30"
                onClick={() => props.onReorder(index(), index() - 1)}
              >
                <span aria-hidden="true">▲</span>
              </button>
              <button
                type="button"
                aria-label={props.downLabel}
                disabled={index() === props.items.length - 1}
                class="text-ink-muted hover:text-ink disabled:opacity-30"
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
