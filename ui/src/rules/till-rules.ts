// The till's rules module (ADR-0193 decision 2, ADR-0202 decision 1): `pos-rules` and `pos-core`'s
// flow engine, built from `crates/pos-till-rules` for WebAssembly by `pnpm rules`. The same engine
// the edge re-checks with, so the till and the edge cannot read a condition two ways.
//
// # When it runs
//
// It is instantiated once, at boot, after the first screen is drawn (`startTillRules`), and a
// package's flows are loaded into it, which compiles every condition, when the package is applied.
// A tap only asks: which steps a flow may offer now, and which prerequisite a step is missing. Each
// answer is a call into the module of a few tens of microseconds. Nothing here is asked on a tap
// before the module is ready: until then `tillRules()` is `undefined`, and the till offers what it
// offered before, while the edge still re-checks every command (ADR-0192 decision 4).
//
// # The boundary
//
// The module exports three functions and its memory, and imports nothing. A request is JSON,
// copied into a buffer the module reserves; the answer is JSON, copied out. Every error is an
// answer, `{ ok: false, error }`, never a trap.

import wasmUrl from "./pos_till_rules.wasm?url";

/** A step of a package flow, as `pos_proto::flow::FlowStep` writes it. */
export interface FlowStep {
  readonly step_id: string;
  readonly screen_id?: string;
  readonly action_id?: string;
  readonly kind?: "STEP_KIND_REQUIRED" | "STEP_KIND_OPTIONAL";
  readonly applies_when?: string;
  readonly prerequisites?: readonly string[];
  readonly done_when?: string;
}

/** A package flow, as `pos_proto::flow::Flow` writes it. */
export interface Flow {
  readonly flow_id: string;
  readonly start_action_id: string;
  readonly steps: readonly FlowStep[];
}

/** What the till knows of a flow in progress. */
export interface FlowProgress {
  readonly done?: readonly string[];
  readonly skipped?: readonly string[];
}

/** A value a condition reads: whole numbers only, an amount in minor units. Never `null`. */
export type Fact = boolean | number | string | readonly Fact[] | { readonly [field: string]: Fact };

/** The views a condition reads, by name (`order`, `line`, `bill`, `channel`, `device`, `role`, `now`). */
export type Facts = Readonly<Record<string, Fact>>;

/** An answer the module gives: what was asked for, or why not. */
export type Answer<T> = ({ readonly ok: true } & T) | { readonly ok: false; readonly error: string };

/** The exports the module has. */
interface Exports {
  readonly memory: WebAssembly.Memory;
  readonly till_rules_input: (len: number) => number;
  readonly till_rules_call: (len: number) => number;
  readonly till_rules_output: () => number;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** The till's rules module, instantiated. */
export class TillRules {
  readonly #exports: Exports;

  constructor(instance: WebAssembly.Instance) {
    this.#exports = instance.exports as unknown as Exports;
  }

  /** Asks the module one request. */
  ask<T>(request: object): Answer<T> {
    const bytes = encoder.encode(JSON.stringify(request));
    const exports = this.#exports;
    const at = exports.till_rules_input(bytes.length);
    if (at === 0 && bytes.length > 0) {
      return { ok: false, error: "the request is too large for the rules module" };
    }
    new Uint8Array(exports.memory.buffer, at, bytes.length).set(bytes);
    const length = exports.till_rules_call(bytes.length);
    const answer = new Uint8Array(exports.memory.buffer, exports.till_rules_output(), length);
    return JSON.parse(decoder.decode(answer)) as Answer<T>;
  }

  /** Loads a package's flows, every variant of each in the package's order, compiling every condition. */
  load(flows: readonly Flow[]): Answer<{ flows: number; refused: { condition: string; error: string }[] }> {
    return this.ask({ op: "load", flows });
  }

  /** The steps of a flow the till may offer now, in the flow's order. */
  available(flowId: string, progress: FlowProgress, facts: Facts, variant = 0): Answer<{ steps: string[] }> {
    return this.ask({ op: "available", flow_id: flowId, variant, progress, facts });
  }

  /** The prerequisite a step is missing, or `null` when it can run. */
  missing(
    flowId: string,
    stepId: string,
    progress: FlowProgress,
    facts: Facts,
    variant = 0,
  ): Answer<{ missing: string | null }> {
    return this.ask({ op: "missing", flow_id: flowId, variant, step_id: stepId, progress, facts });
  }

  /** Whether a condition holds over `facts`. */
  holds(condition: string, facts: Facts): Answer<{ holds: boolean }> {
    return this.ask({ op: "holds", condition, facts });
  }
}

/** Instantiates the module from its bytes. */
export async function instantiateTillRules(bytes: BufferSource): Promise<TillRules> {
  const { instance } = await WebAssembly.instantiate(bytes, {});
  return new TillRules(instance);
}

let ready: TillRules | undefined;
let starting: Promise<TillRules | undefined> | undefined;

/**
 * Fetches and instantiates the module, once. A module that cannot be loaded leaves the till as it
 * was before it had one: it offers every step, and the edge re-checks every command.
 */
export function startTillRules(): Promise<TillRules | undefined> {
  starting ??= fetch(wasmUrl)
    .then((response) => response.arrayBuffer())
    .then(instantiateTillRules)
    .then(
      (rules) => {
        ready = rules;
        return rules;
      },
      () => undefined,
    );
  return starting;
}

/** The module, once it is ready; `undefined` before. Read on a tap, never awaited there. */
export function tillRules(): TillRules | undefined {
  return ready;
}
