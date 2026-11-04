// `state/print-writes.ts` (ADR-0196 decision 1, ADR-0204 decision 5): a print says it is on its way
// at once and takes no second tap; a refusal says its own sentence; no answer is said unconfirmed,
// never rolled back to a re-read and never sent again.

import { describe, expect, it } from "vitest";

import { ApiError } from "../src/api/client";
import { t } from "../src/i18n";
import { errorMessage } from "../src/lib/errors";
import { writesPending } from "../src/state/edge-write";
import { createPrint } from "../src/state/print-writes";

function screen() {
  const reported: (string | null)[] = [];
  return { reported, print: createPrint((message) => reported.push(message)) };
}

describe("a print", () => {
  it("is printing from the tap, takes no second tap, and hands over the edge's answer", async () => {
    const { reported, print } = screen();
    let sent = 0;
    let answer = (_: string) => {};
    const send = () => {
      sent += 1;
      return new Promise<string>((resolve) => {
        answer = resolve;
      });
    };
    const answers: string[] = [];
    const first = print.run("billing.check.print", send, (printed) => answers.push(printed));
    expect(print.printing()).toBe(true);
    // A print changes no money: payment does not wait for it.
    expect(writesPending()).toBe(false);
    await print.run("billing.check.print", send, (printed) => answers.push(printed));
    expect(sent).toBe(1);
    answer("PRINTED");
    await first;
    expect(answers).toEqual(["PRINTED"]);
    expect(print.printing()).toBe(false);
    expect(reported).toEqual([null]);
  });

  it("says a refusal's own sentence", async () => {
    const { reported, print } = screen();
    const refusal = new ApiError(409, "not settled", "NOT_SETTLED");
    await print.run("billing.receipt.reprint", () => Promise.reject(refusal), () => undefined);
    expect(reported).toEqual([null, errorMessage(refusal)]);
  });

  it("says a print with no answer is not confirmed, and sends it once", async () => {
    const { reported, print } = screen();
    let sent = 0;
    const send = () => {
      sent += 1;
      return Promise.reject(new TypeError("network"));
    };
    await print.run("reports.shift.print_x", send, () => undefined);
    expect(sent).toBe(1);
    expect(reported).toEqual([null, t("print.unconfirmed")]);
  });
});
