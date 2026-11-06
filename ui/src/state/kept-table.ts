// The kept order screen's table, and the values an order screen keeps with a table
// (`../screens/kept.tsx`).

import {
  createContext,
  createRenderEffect,
  createSignal,
  on,
  useContext,
  type Accessor,
} from "solid-js";

/** The table a kept order screen shows, and whether it is on screen. */
export interface KeptTable {
  /** The table, held while the screen is hidden. */
  readonly id: string;
  readonly shown: Accessor<boolean>;
  /**
   * Shows `id` now, ahead of the router: for an act that changes the table and its order in one
   * tap, so both are drawn in one update. The address follows, and the router's change then draws
   * nothing new.
   */
  readonly follow: (id: string) => void;
}

export const KeptTableContext = createContext<KeptTable>();

/** The kept order screen's table, or `undefined` for an order screen the router mounted. */
export function useKeptTable(): KeptTable | undefined {
  return useContext(KeptTableContext);
}

/** A value kept with the table it belongs to. */
export interface TableScoped<T> {
  /** The value, if it belongs to the table shown now. */
  readonly get: Accessor<T | null>;
  /**
   * `null` starts an act on the table shown now and clears the value; any other value is that
   * act's result, kept with the act's table even when it lands after the operator has moved on.
   */
  readonly set: (value: T | null) => void;
  /** A value for a named table. */
  readonly setFor: (key: string, value: T) => void;
  /** Clears the value, whichever table it belongs to. */
  readonly clear: () => void;
}

/**
 * A refusal or a result an async act reports, kept with its table. Every act on the order screen
 * reports `null` when it starts, synchronously, and its result when it lands: the table shown at the
 * start is the act's, and its result shows only there.
 */
export function createTableScoped<T>(key: Accessor<string>): TableScoped<T> {
  const [value, setValue] = createSignal<{
    readonly key: string;
    readonly value: T;
  } | null>(null);
  let actKey = key();
  return {
    get: () => {
      const held = value();
      return held !== null && held.key === key() ? held.value : null;
    },
    set: (next) => {
      if (next === null) {
        actKey = key();
        setValue(null);
      } else {
        setValue({ key: actKey, value: next });
      }
    },
    setFor: (forKey, next) => setValue({ key: forKey, value: next }),
    clear: () => setValue(null),
  };
}

/**
 * Starts each table on a kept order screen as a fresh screen would: when it hides, and when its
 * table changes, `panels` closes every panel and drops what was typed or picked. The `results` of
 * the last acts go when it hides; one that lands later shows only on its own table.
 */
export function resetPerTable(
  shown: Accessor<boolean>,
  key: Accessor<string>,
  reset: {
    readonly panels: () => void;
    readonly results: readonly Pick<TableScoped<unknown>, "clear">[];
  },
): void {
  createRenderEffect(
    on(
      [shown, key],
      ([isShown]) => {
        reset.panels();
        if (!isShown) {
          reset.results.forEach((result) => result.clear());
        }
      },
      { defer: true },
    ),
  );
}
