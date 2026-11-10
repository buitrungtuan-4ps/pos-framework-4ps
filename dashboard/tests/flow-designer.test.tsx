// The flow designer and the rule builder (ADR-0192, ADR-0193): HQ builds a flow's steps, their
// prerequisites and their conditions, and a reaction's when, if and then, by choosing, and the draft
// holds the flows, reactions and CEL the cloud checks. A refusal the cloud locates shows beside the
// flow or the reaction it names.

import { cleanup, fireEvent, render, screen, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import type { PackageRefusal } from "../src/api/schemas";
import en from "../src/i18n/en.json";
import { blankDraft, type PackageDraft } from "../src/lib/package-draft";
import type { DraftChange } from "../src/screens/packages/designer/use-draft";
import { FlowsPanel } from "../src/screens/packages/flows/FlowsPanel";
import { ReactionsPanel } from "../src/screens/packages/reactions/ReactionsPanel";

const messages = en as Record<string, string>;
const m = (key: string): string => messages[key] ?? key;

afterEach(cleanup);

function mount(Panel: typeof FlowsPanel, start: PackageDraft, refusals: PackageRefusal[] = []) {
  const [draft, setDraft] = createSignal(start);
  render(() => <Panel draft={draft} change={(next: DraftChange) => setDraft(next(draft()))} refusals={refusals} />);
  return draft;
}

const type = (label: string | RegExp, value: string, scope: HTMLElement = document.body) =>
  fireEvent.input(within(scope).getByLabelText(label), { target: { value } });
const pick = (label: string, value: string, scope: HTMLElement = document.body) =>
  fireEvent.change(within(scope).getByLabelText(label), { target: { value } });
const click = (name: string, scope: HTMLElement = document.body) =>
  fireEvent.click(within(scope).getByRole("button", { name }));

describe("the flow designer", () => {
  it("builds a flow of steps that wait on each other, with a condition written as CEL", () => {
    const draft = mount(FlowsPanel, blankDraft("March"));
    type(m("flow.newName"), "Seat and order");
    click(m("flow.add"));
    pick(m("flow.startsWith"), "sales.order.open");
    type(m("flow.step.newName"), "Guests");
    click(m("flow.step.add"));
    pick(m("flow.step.does"), "screen");
    // A field with a hint reads its hint as part of its label.
    type(new RegExp(`^${m("flow.step.screen")}`), "guest_count");
    type(m("flow.step.newName"), "Order");
    click(m("flow.step.add"));
    // The new step is the one being edited, and it may wait on the first.
    fireEvent.click(screen.getByRole("checkbox", { name: "guests" }));
    const applies = screen.getAllByRole("group", { name: m("flow.step.appliesWhen") })[0]!;
    click(m("rule.addClause"), applies);
    pick(m("rule.fact"), "order.channel", applies);
    pick(m("rule.compare"), "eq", applies);
    pick(m("rule.value"), "SALES_CHANNEL_DINE_IN", applies);

    const flow = draft().flows?.[0]?.body;
    expect(flow?.flow_id).toBe("seat_and_order");
    expect(flow?.start_action_id).toBe("sales.order.open");
    expect(flow?.steps.map((step) => step.step_id)).toEqual(["guests", "order"]);
    expect(flow?.steps[0]?.screen_id).toBe("guest_count");
    expect(flow?.steps[1]?.prerequisites).toEqual(["guests"]);
    expect(flow?.steps[1]?.applies_when).toBe('order.channel == "SALES_CHANNEL_DINE_IN"');
    // The graph puts the waiting step in the next column.
    expect(screen.getByText(m("flow.column").replace("{n}", "2"))).toBeTruthy();
  });

  it("shows a refusal beside the flow it names", () => {
    const start: PackageDraft = {
      ...blankDraft("March"),
      flows: [
        {
          body: {
            flow_id: "seat",
            start_action_id: "sales.order.open",
            steps: [{ step_id: "guests", screen_id: "guest_count", kind: "STEP_KIND_REQUIRED", prerequisites: [] }],
          },
        },
      ],
    };
    mount(FlowsPanel, start, [
      { field: "flows", reason: "CONDITION", message: "x", flow_index: 0, step_id: "guests" },
    ]);
    const shown = within(screen.getByRole("alert")).getByText((text) => text.includes("guests"));
    expect(shown.textContent).toContain(m("flow.refusal.condition"));
  });
});

describe("the rule builder's reactions", () => {
  it("builds when, if and then, offering only acts that move no money", () => {
    const draft = mount(ReactionsPanel, blankDraft("March"));
    type(m("reaction.newName"), "Open the menu");
    click(m("reaction.add"));
    pick(m("reaction.when"), "REACTION_TRIGGER_ORDER_OPENED");
    const acts = [...(screen.getByLabelText(m("reaction.then")) as HTMLSelectElement).options].map((option) => option.value);
    expect(acts).toContain("ui.screen.open");
    expect(acts).not.toContain("billing.bill.void");
    pick(m("reaction.then"), "ui.screen.open");
    type("screen_id", "menu");
    const condition = screen.getByRole("group", { name: m("reaction.if") });
    click(m("rule.addClause"), condition);
    pick(m("rule.fact"), "order.guest_count", condition);
    pick(m("rule.compare"), "ge", condition);
    type(m("rule.value"), "6", condition);

    const reaction = draft().reactions?.[0]?.body;
    expect(reaction?.reaction_id).toBe("open_the_menu");
    expect(reaction?.then).toEqual({ action_id: "ui.screen.open", params: { screen_id: "menu" } });
    expect(reaction?.if).toBe("has(order.guest_count) && order.guest_count >= 6");
  });

  it("keeps a hand-written condition as it is until it is replaced", () => {
    const draft = mount(ReactionsPanel, {
      ...blankDraft("March"),
      reactions: [
        {
          body: {
            reaction_id: "big",
            when: "REACTION_TRIGGER_ORDER_OPENED",
            if: "order.guest_count > 5",
            then: { action_id: "ui.screen.open", params: { screen_id: "menu" } },
          },
        },
      ],
    });
    expect(screen.getByText("order.guest_count > 5")).toBeTruthy();
    click(m("rule.replace"));
    expect(draft().reactions?.[0]?.body.if).toBeUndefined();
  });
});
