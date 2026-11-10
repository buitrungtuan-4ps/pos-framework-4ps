// A condition HQ builds from choices and the console writes as CEL, so nobody types CEL
// ([ADR-0193](../../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md) decisions 3
// and 4, [ADR-0202](../../../docs/adr/0202-cel-as-the-crate-and-the-spike-found-it.md)).
//
// A condition is a list of clauses joined by "all of" or "any of". Each clause compares one fact the
// runtime fills to a value. `toCel` writes it in the restricted dialect: comparisons, `&&`, `||`,
// `!`, `in` and `has()`, integers only, no macro. `fromCel` reads back exactly what `toCel` writes,
// so a published condition opens in the builder again; anything else (a condition written by hand
// before the builder existed) reads as `null`, and the screen shows it as text that can only be
// replaced, never half-understood.
//
// The cloud compiles every condition at publish with the same evaluator the edge and the till run
// (`pos-rules`), so this module checks only what the builder itself can get wrong: a clause with no
// value, and the dialect's 1,024-character limit.

import { SALES_CHANNELS } from "./channels";
import type { MessageKey } from "../i18n";

/** What a fact holds: a whole number (a count, an amount in minor units) or a wire token. */
export type FactKind = "int" | "token";

/** One fact a condition can read, as the edge and the till fill it today. */
export type RuleFact = {
  /** The path a condition reads it by, such as `order.guest_count`. */
  readonly path: string;
  readonly kind: FactKind;
  /** Whether an order can lack it; a clause over it then says `has(…)` first. */
  readonly optional: boolean;
  /** Whether the edge holds it, so an edge guard (`done_when`) may read it (ADR-0192 decision 4). */
  readonly edge: boolean;
  /** The tokens a token fact takes, in the order a picker lists them. */
  readonly tokens: readonly string[];
  readonly labelKey: MessageKey;
};

/**
 * The facts the builder offers: those `pos-edge-app` (`order_facts`) and the till
 * (`ui/src/state/flows.ts`) fill for an order. A fact neither fills would make a condition that
 * never holds, so a new one joins this list when the runtime fills it.
 */
export const RULE_FACTS: readonly RuleFact[] = [
  {
    path: "order.channel",
    kind: "token",
    optional: false,
    edge: true,
    tokens: SALES_CHANNELS,
    labelKey: "rule.fact.orderChannel",
  },
  {
    path: "order.line_count",
    kind: "int",
    optional: false,
    edge: true,
    tokens: [],
    labelKey: "rule.fact.lineCount",
  },
  {
    path: "order.guest_count",
    kind: "int",
    optional: true,
    edge: true,
    tokens: [],
    labelKey: "rule.fact.guestCount",
  },
];

/** The dialect's limit on one condition (ADR-0193 decision 3). */
export const CONDITION_CHARS_MAX = 1024;

/** How a clause compares its fact. `set` and `unset` take no value and apply to an optional fact. */
export type RuleOp = "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "in" | "set" | "unset";

const COMPARISON: Readonly<Record<"eq" | "ne" | "lt" | "le" | "gt" | "ge", string>> = {
  eq: "==",
  ne: "!=",
  lt: "<",
  le: "<=",
  gt: ">",
  ge: ">=",
};

/** One comparison. `values` holds one value, several for `in`, none for `set` and `unset`. */
export type Clause = {
  readonly path: string;
  readonly op: RuleOp;
  readonly values: readonly string[];
};

/** A whole condition: its clauses, all of which or any of which must hold. */
export type RuleCondition = {
  readonly join: "all" | "any";
  readonly clauses: readonly Clause[];
};

/** The fact at `path`, if the builder offers it. */
export const factAt = (path: string): RuleFact | undefined =>
  RULE_FACTS.find((fact) => fact.path === path);

/** The operators a clause over `fact` may use, in the order a picker lists them. */
export function opsFor(fact: RuleFact): readonly RuleOp[] {
  const presence: RuleOp[] = fact.optional ? ["set", "unset"] : [];
  return fact.kind === "int"
    ? [...presence, "eq", "ne", "lt", "le", "gt", "ge"]
    : [...presence, "eq", "ne", "in"];
}

/** Why a condition cannot be written yet, as an i18n key, or `null` when it can. */
export function problemWith(condition: RuleCondition): MessageKey | null {
  if (condition.clauses.length === 0) {
    return "rule.problem.noClause";
  }
  for (const clause of condition.clauses) {
    const fact = factAt(clause.path);
    if (fact === undefined || !opsFor(fact).includes(clause.op)) {
      return "rule.problem.unknownFact";
    }
    if (clause.op === "set" || clause.op === "unset") {
      continue;
    }
    if (clause.values.length === 0 || clause.values.some((value) => value.trim() === "")) {
      return "rule.problem.noValue";
    }
    if (clause.op !== "in" && clause.values.length !== 1) {
      return "rule.problem.noValue";
    }
    if (fact.kind === "int" && clause.values.some((value) => !/^-?\d{1,15}$/u.test(value.trim()))) {
      return "rule.problem.notWhole";
    }
  }
  if (toCel(condition).length > CONDITION_CHARS_MAX) {
    return "rule.problem.tooLong";
  }
  return null;
}

/** The CEL literal for `value` of a fact of `kind`. */
function literal(kind: FactKind, value: string): string {
  const trimmed = value.trim();
  return kind === "int" ? String(Number.parseInt(trimmed, 10)) : JSON.stringify(trimmed);
}

/** One clause as CEL. An optional fact is guarded by `has()`, so a missing one reads as false. */
function clauseCel(clause: Clause): string {
  const fact = factAt(clause.path);
  const kind = fact?.kind ?? "token";
  const guard = fact?.optional === true ? `has(${clause.path}) && ` : "";
  switch (clause.op) {
    case "set":
      return `has(${clause.path})`;
    case "unset":
      return `!has(${clause.path})`;
    case "in":
      return `${guard}${clause.path} in [${clause.values.map((value) => literal(kind, value)).join(", ")}]`;
    default:
      return `${guard}${clause.path} ${COMPARISON[clause.op]} ${literal(kind, clause.values[0] ?? "")}`;
  }
}

/**
 * The condition as CEL. One clause stands alone; several are each parenthesised and joined by
 * `&&` or `||`, so a guarded clause keeps its guard whatever the join.
 */
export function toCel(condition: RuleCondition): string {
  const parts = condition.clauses.map(clauseCel);
  if (parts.length === 1) {
    return parts[0] ?? "";
  }
  return parts.map((part) => `(${part})`).join(condition.join === "all" ? " && " : " || ");
}

const PATH = String.raw`[a-z_]+(?:\.[a-z_]+)*`;
const VALUE = String.raw`-?\d+|"(?:[^"\\]|\\.)*"`;

/** One clause back from the CEL `clauseCel` wrote, or `null`. */
function clauseFrom(text: string): Clause | null {
  const presence = new RegExp(String.raw`^(!?)has\((${PATH})\)$`, "u").exec(text);
  if (presence !== null) {
    const fact = factAt(presence[2] ?? "");
    return fact?.optional === true
      ? { path: fact.path, op: presence[1] === "!" ? "unset" : "set", values: [] }
      : null;
  }
  const guarded = new RegExp(String.raw`^has\((${PATH})\) && (.*)$`, "u").exec(text);
  const body = guarded === null ? text : (guarded[2] ?? "");
  const list = new RegExp(String.raw`^(${PATH}) in \[((?:${VALUE})(?:, (?:${VALUE}))*)\]$`, "u").exec(body);
  const compare = new RegExp(String.raw`^(${PATH}) (==|!=|<=|>=|<|>) (${VALUE})$`, "u").exec(body);
  const path = list?.[1] ?? compare?.[1];
  const fact = path === undefined ? undefined : factAt(path);
  if (fact === undefined || (guarded !== null && guarded[1] !== path) || (guarded !== null) !== fact.optional) {
    return null;
  }
  const valueOf = (raw: string): string | null => {
    if (fact.kind === "int") {
      return /^-?\d+$/u.test(raw) ? raw : null;
    }
    if (!raw.startsWith('"')) {
      return null;
    }
    try {
      const parsed: unknown = JSON.parse(raw);
      return typeof parsed === "string" ? parsed : null;
    } catch {
      return null;
    }
  };
  if (list !== null) {
    const raws = (list[2] ?? "").match(new RegExp(VALUE, "gu")) ?? [];
    const values = raws.map(valueOf);
    return fact.kind === "token" && values.every((value) => value !== null)
      ? { path: fact.path, op: "in", values: values as string[] }
      : null;
  }
  if (compare !== null) {
    const op = (Object.keys(COMPARISON) as (keyof typeof COMPARISON)[]).find(
      (key) => COMPARISON[key] === compare[2],
    );
    const value = valueOf(compare[3] ?? "");
    if (op === undefined || value === null || !opsFor(fact).includes(op)) {
      return null;
    }
    return { path: fact.path, op, values: [value] };
  }
  return null;
}

/** Splits `text` at top-level ` && ` or ` || ` between parenthesised clauses, or `null`. */
function splitJoined(text: string): { join: "all" | "any"; parts: string[] } | null {
  if (!text.startsWith("(") || !text.endsWith(")")) {
    return null;
  }
  const parts: string[] = [];
  const joins = new Set<string>();
  let depth = 0;
  let inString = false;
  let start = 0;
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (inString) {
      if (character === "\\") {
        index += 1;
      } else if (character === '"') {
        inString = false;
      }
      continue;
    }
    if (character === '"') {
      inString = true;
    } else if (character === "(") {
      if (depth === 0) {
        start = index + 1;
      }
      depth += 1;
    } else if (character === ")") {
      depth -= 1;
      if (depth === 0) {
        parts.push(text.slice(start, index));
        const join = text.slice(index + 1, index + 5);
        if (index + 1 < text.length) {
          if (join !== " && " && join !== " || ") {
            return null;
          }
          joins.add(join);
          index += 4;
        }
      }
    } else if (depth === 0) {
      return null;
    }
  }
  if (joins.size > 1 || parts.length < 2) {
    return null;
  }
  return { join: joins.has(" || ") ? "any" : "all", parts };
}

/**
 * The condition `text` holds, when it is one `toCel` writes; `null` for anything else, which the
 * builder shows as text and offers to replace.
 */
export function fromCel(text: string): RuleCondition | null {
  const trimmed = text.trim();
  if (trimmed === "") {
    return null;
  }
  const joined = splitJoined(trimmed);
  const parts = joined?.parts ?? [trimmed];
  const clauses = parts.map(clauseFrom);
  if (clauses.some((clause) => clause === null)) {
    return null;
  }
  const condition: RuleCondition = { join: joined?.join ?? "all", clauses: clauses as Clause[] };
  // Only what this module would write back the same: anything else is someone's own CEL.
  return toCel(condition) === trimmed ? condition : null;
}

/** Whether `condition` reads only facts the edge holds, so it may be an edge guard. */
export const edgeReadable = (condition: RuleCondition): boolean =>
  condition.clauses.every((clause) => factAt(clause.path)?.edge === true);
