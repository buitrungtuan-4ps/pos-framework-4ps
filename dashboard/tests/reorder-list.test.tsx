import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ReorderList } from "../src/components/kit";

afterEach(cleanup);

describe("the ReorderList component", () => {
  const items = [
    { id: "1", name: "Item One" },
    { id: "2", name: "Item Two" },
    { id: "3", name: "Item Three" },
  ];

  it("renders item list with accessible up/down buttons and correct disabled states", () => {
    const onReorder = vi.fn();
    render(() => (
      <ReorderList
        items={items}
        itemKey={(item) => item.id}
        renderItem={(item) => <span>{item.name}</span>}
        onReorder={onReorder}
        upLabel="Move item up"
        downLabel="Move item down"
      />
    ));

    const upButtons = screen.getAllByRole("button", { name: "Move item up" });
    const downButtons = screen.getAllByRole("button", { name: "Move item down" });

    expect(upButtons.length).toBe(3);
    expect(downButtons.length).toBe(3);

    // The first item cannot move up
    expect((upButtons[0] as HTMLButtonElement).disabled).toBe(true);
    expect((downButtons[0] as HTMLButtonElement).disabled).toBe(false);

    // The middle item can move either way
    expect((upButtons[1] as HTMLButtonElement).disabled).toBe(false);
    expect((downButtons[1] as HTMLButtonElement).disabled).toBe(false);

    // The last item cannot move down
    expect((upButtons[2] as HTMLButtonElement).disabled).toBe(false);
    expect((downButtons[2] as HTMLButtonElement).disabled).toBe(true);
  });

  it("calls onReorder with correct indices when buttons are clicked", () => {
    const onReorder = vi.fn();
    render(() => (
      <ReorderList
        items={items}
        itemKey={(item) => item.id}
        renderItem={(item) => <span>{item.name}</span>}
        onReorder={onReorder}
        upLabel="Move item up"
        downLabel="Move item down"
      />
    ));

    const upButtons = screen.getAllByRole("button", { name: "Move item up" });
    const downButtons = screen.getAllByRole("button", { name: "Move item down" });

    // Click move down on first item
    fireEvent.click(downButtons[0]!);
    expect(onReorder).toHaveBeenCalledWith(0, 1);

    // Click move up on second item
    fireEvent.click(upButtons[1]!);
    expect(onReorder).toHaveBeenCalledWith(1, 0);
  });
});
