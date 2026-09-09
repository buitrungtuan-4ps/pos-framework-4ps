// The lifecycle of authoring one kind of entity on a screen
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md) §3–4).
//
// Every screen that lets an operator add, change or remove something needs the same five things: a
// mode (nothing open, adding, changing this row, confirming something destructive about this row),
// the row in question, whether a write is in flight, the last refusal's message, and one function
// that turns a promise into all four. Before this the console hand-rolled that ~40 times, in 523
// `createSignal` under `screens/` and 20 differently-named `pending*` signals for the same idea.
//
// Two things this deliberately gets right that the hand-rolled versions did not:
//
// **The busy flag is per lifecycle, not per screen.** A screen holding one `busy` disables every
// button on the page during any single call — the audit's finding, and a genuine annoyance on a
// screen that authors two entity types. Each `useEntityCrud()` is its own instance, so `Stores`
// holds one for stores and one for brands, and saving a brand does not freeze the store table.
//
// **A refused write keeps the form open.** `run` closes only on success. Under
// [ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md) a save carries `If-Match` and
// can be refused as stale, and at that moment the operator's typing is the only copy of what they
// meant; throwing it away to show an error is the worst of the two failures.
//
// This owns writes only. Reads keep `Panel<T>` (`lib/panel.ts`), which answers a different question
// — did *this* read load — and the resource helper the plan's Stage 5 asks for is not here; ADR-0121
// says why the two are not one change.

import { createSignal } from "solid-js";

import { apiMessage } from "./errors";

/**
 * What is open, and about which row.
 *
 * `confirming` is separate from `editing` because a destructive action asks a different question and
 * renders differently (`ConfirmDialog`, not `FormPanel`); collapsing them would make "are you sure"
 * and "here is a form" the same state, which is how a screen ends up confirming into a text field.
 */
export type CrudMode = "idle" | "creating" | "editing" | "confirming";

/** The authoring lifecycle for one entity type. Create one per type, not one per screen. */
export type EntityCrud<T> = {
  /** What is open. */
  readonly mode: () => CrudMode;
  /** The row being edited or confirmed; `null` while idle or creating. */
  readonly subject: () => T | null;
  /** True while a `run` is in flight — scoped to this lifecycle, not the whole screen. */
  readonly saving: () => boolean;
  /** The last refusal's message, or `""`. Cleared when a new write starts and when the panel closes. */
  readonly error: () => string;
  /** Opens the create form. */
  readonly create: () => void;
  /** Opens the edit form for `row`. */
  readonly edit: (row: T) => void;
  /** Opens the confirmation for `row`. */
  readonly confirm: (row: T) => void;
  /** Closes whatever is open and forgets the last error. */
  readonly close: () => void;
  /** Replaces the shown message without opening or closing anything — for a client-side refusal. */
  readonly refuse: (message: string) => void;
  /**
   * The one write path: clears the last error, marks this lifecycle busy, awaits `write`, and then
   * closes on success or stays open carrying the refusal's own message. Resolves `true` when the
   * write succeeded, so a caller can refresh its list only when there is something new to show.
   */
  readonly run: (write: () => Promise<unknown>) => Promise<boolean>;
};

/** Creates an [`EntityCrud`]. See the module header for why one per entity type. */
export function useEntityCrud<T>(): EntityCrud<T> {
  const [mode, setMode] = createSignal<CrudMode>("idle");
  const [subject, setSubject] = createSignal<T | null>(null);
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");

  const open = (next: CrudMode, row: T | null) => {
    setError("");
    setSubject(() => row);
    setMode(next);
  };

  return {
    mode,
    subject,
    saving,
    error,
    create: () => open("creating", null),
    edit: (row: T) => open("editing", row),
    confirm: (row: T) => open("confirming", row),
    close: () => {
      setError("");
      setSubject(() => null);
      setMode("idle");
    },
    refuse: (message: string) => setError(message),
    run: async (write: () => Promise<unknown>) => {
      setError("");
      setSaving(true);
      try {
        await write();
        setSubject(() => null);
        setMode("idle");
        return true;
      } catch (caught: unknown) {
        // `apiMessage` is the console's one reading of a refusal — the AIP-193 envelope's own message
        // where there is one, the thrown value otherwise. It stays on screen beside the operator's
        // still-intact typing.
        setError(apiMessage(caught));
        return false;
      } finally {
        setSaving(false);
      }
    },
  };
}
