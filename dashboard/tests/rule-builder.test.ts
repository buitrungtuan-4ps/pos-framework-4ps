// The rule builder writes CEL so nobody types it (ADR-0193), and reads back what it wrote.
//
// Every condition here is also a line of `rule-conditions.txt`, which the
// evaluator the cloud, the edge and the till run compiles and answers over that line's facts. So a
// condition this builder writes is one the cloud's publish check accepts, and it means what the
// builder shows.

import { describe, expect, it } from "vitest";

import {
  CONDITION_CHARS_MAX,
  edgeReadable,
  fromCel,
  opsFor,
  problemWith,
  RULE_FACTS,
  type RuleCondition,
  toCel,
} from "../src/lib/rule-builder";

const files: Record<string, string> = import.meta.glob("./rule-conditions.txt", {
  query: "?raw",
  import: "default",
  eager: true,
});

const VECTORS = (files["./rule-conditions.txt"] ?? "")
  .split("\n")
  .filter((line) => line.trim() !== "" && !line.startsWith("#"))
  .map((line) => line.split("\t")[4]);

const one = (path: string, op: RuleCondition["clauses"][number]["op"], values: string[] = []): RuleCondition => ({
  join: "all",
  clauses: [{ path, op, values }],
});

const CASES: readonly [RuleCondition, string][] = [
  [one("order.channel", "eq", ["SALES_CHANNEL_DINE_IN"]), 'order.channel == "SALES_CHANNEL_DINE_IN"'],
  [
    one("order.channel", "in", ["SALES_CHANNEL_TAKEAWAY", "SALES_CHANNEL_DELIVERY"]),
    'order.channel in ["SALES_CHANNEL_TAKEAWAY", "SALES_CHANNEL_DELIVERY"]',
  ],
  [one("order.line_count", "lt", ["1"]), "order.line_count < 1"],
  [one("order.guest_count", "unset"), "!has(order.guest_count)"],
  [one("order.guest_count", "set"), "has(order.guest_count)"],
  [one("order.guest_count", "ge", ["6"]), "has(order.guest_count) && order.guest_count >= 6"],
  [
    {
      join: "all",
      clauses: [
        { path: "order.channel", op: "eq", values: ["SALES_CHANNEL_DINE_IN"] },
        { path: "order.guest_count", op: "ge", values: ["6"] },
      ],
    },
    '(order.channel == "SALES_CHANNEL_DINE_IN") && (has(order.guest_count) && order.guest_count >= 6)',
  ],
  [
    {
      join: "any",
      clauses: [
        { path: "order.channel", op: "ne", values: ["SALES_CHANNEL_DINE_IN"] },
        { path: "order.guest_count", op: "unset", values: [] },
      ],
    },
    '(order.channel != "SALES_CHANNEL_DINE_IN") || (!has(order.guest_count))',
  ],
];

describe("writing a condition as CEL", () => {
  it.each(CASES)("writes %j as the dialect's CEL, which the evaluator's vectors hold", (condition, cel) => {
    expect(toCel(condition)).toBe(cel);
    expect(VECTORS).toContain(cel);
    expect(problemWith(condition)).toBeNull();
  });

  it("reads back every condition it writes", () => {
    for (const [condition, cel] of CASES) {
      expect(fromCel(cel)).toEqual(condition);
    }
  });

  it("every vector the evaluator checks is one this builder writes", () => {
    for (const cel of VECTORS) {
      const condition = fromCel(cel ?? "");
      expect(condition, cel).not.toBeNull();
      expect(toCel(condition as RuleCondition)).toBe(cel);
    }
  });

  it("writes a token as a quoted string, whatever it holds", () => {
    const cel = toCel(one("order.channel", "eq", ['A "quoted" \\ token']));
    expect(cel).toBe('order.channel == "A \\"quoted\\" \\\\ token"');
    expect(fromCel(cel)).toEqual(one("order.channel", "eq", ['A "quoted" \\ token']));
  });
});

describe("a condition someone wrote by hand", () => {
  it.each([
    "order.total_amount > 100000",
    "order.guest_count >= 6",
    "order.line_count < 1 && order.line_count > 0",
    '(order.channel == "SALES_CHANNEL_DINE_IN") && (order.line_count < 1) || (order.line_count > 3)',
    "size(order.category_ids) > 1",
    "order.line_count<1",
    "",
  ])("reads %j as nothing the builder can edit, rather than half of it", (cel) => {
    expect(fromCel(cel)).toBeNull();
  });
});

describe("what the builder will not write", () => {
  it("refuses an empty condition, a missing value and a count that is not whole", () => {
    expect(problemWith({ join: "all", clauses: [] })).toBe("rule.problem.noClause");
    expect(problemWith(one("order.line_count", "lt", []))).toBe("rule.problem.noValue");
    expect(problemWith(one("order.line_count", "lt", [" "]))).toBe("rule.problem.noValue");
    expect(problemWith(one("order.line_count", "lt", ["1.5"]))).toBe("rule.problem.notWhole");
    expect(problemWith(one("order.channel", "lt", ["x"]))).toBe("rule.problem.unknownFact");
    expect(problemWith(one("order.line_count", "set"))).toBe("rule.problem.unknownFact");
    expect(problemWith(one("order.nothing", "eq", ["1"]))).toBe("rule.problem.unknownFact");
  });

  it("refuses a condition longer than the dialect allows", () => {
    const long = one("order.channel", "in", Array.from({ length: 60 }, (_, index) => `SALES_CHANNEL_${index}_PADDED`));
    expect(toCel(long).length).toBeGreaterThan(CONDITION_CHARS_MAX);
    expect(problemWith(long)).toBe("rule.problem.tooLong");
  });

  it("offers presence only for a fact an order can lack, and ordering only for a number", () => {
    const at = (path: string) => RULE_FACTS.find((fact) => fact.path === path)!;
    expect(opsFor(at("order.guest_count"))).toEqual(["set", "unset", "eq", "ne", "lt", "le", "gt", "ge"]);
    expect(opsFor(at("order.line_count"))).toEqual(["eq", "ne", "lt", "le", "gt", "ge"]);
    expect(opsFor(at("order.channel"))).toEqual(["eq", "ne", "in"]);
  });

  it("knows every fact it offers is one the edge holds, so any of them may guard on the edge", () => {
    for (const [condition] of CASES) {
      expect(edgeReadable(condition)).toBe(true);
    }
  });
});
