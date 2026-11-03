import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError, api } from "../api/client";
import { ApproverFields, createApprover } from "../components/ApproverFields";
import { DISCOUNT, DiscountPanel, useDiscount } from "../components/DiscountPanel";
import { VOID_BILL, VoidBillPanel, useVoidBill } from "../components/VoidBillPanel";
import { BillEnded } from "../components/BillEnded";
import { DrawerOwner } from "../components/DrawerOwner";
import { CheckRow } from "../components/ui";
import { Keypad } from "../components/Keypad";
import type {
  BillResponse,
  BuyerRequest,
  CheckResponse,
  FeeLine,
  PaymentRequest,
  ReprintResponse,
} from "../api/schemas";
import { t, type MessageKey } from "../i18n";
import { WhyUnavailable } from "../components/StoreData";
import {
  formatQuantity,
  money,
  parseWhole,
  percentOf,
  quickCashFor,
  roundToIncrement,
} from "../lib/money";
import {
  cashDenominations,
  cashRoundingIncrement,
  currencyExponent,
  linesForTable,
  loadBillCheck,
  loadCheck,
  mergeBills,
  openBillFor,
  openBillsFor,
  reasonsFor,
  seatsEnabled,
  settle,
  splitBill,
  splitWays,
  tenderAccepted,
  tipPercents,
  tipsEnabled,
  waiveFee,
  formatAmount,
} from "../state/store";
import { errorMessage } from "../lib/errors";
import { useInFlight } from "../lib/in-flight";
import { endedElsewhere } from "../state/closing";
import { openForPay } from "../state/open-for-pay";
import { useSplitPlan } from "../state/split-plan";
import { printOutcomeKey } from "../lib/print";
import { asksApprover, can } from "../state/permissions";

// The action waiving a fee cites (ADR-0159 decision 5), its own again: a reason for forgiving a
// service charge is not necessarily one for taking money off the food.
const WAIVE_FEE = "REASON_ACTION_WAIVE_FEE";

// How many guest counts fit the even split's row as five equal columns: two to six, the row every
// store had before it could set its own most (ADR-0160 decision 2). More than that and the row
// scrolls sideways instead, like the kitchen board's station tabs. A second row for the rarer larger
// party would push the tenders below the fold on a phone for every bill, split or not.
const SPLIT_COLUMNS = 5;

// Whether the bill was reduced — a discount or a comp — when the screen spells out its arithmetic.
function reduced(totals: CheckResponse): boolean {
  return totals.discount_total.amount_minor > 0 || totals.comp_total.amount_minor > 0;
}

// The label for a payment method in a split's list of shares.
function methodKey(method: string): MessageKey {
  switch (method) {
    case "PAYMENT_METHOD_CARD":
      return "pay.method_card";
    case "PAYMENT_METHOD_QR":
      return "pay.method_qr";
    default:
      return "pay.method_cash";
  }
}

// Whether a set of tip keys is still worth offering: every key positive, and no two the same.
//
// The guard on the cash snap below. On a bill of 8,000₫ a 1,000₫ increment turns 5/10/15% into 0,
// 1,000 and 1,000 — a dead button and a duplicate, on a row whose buttons carry an amount and
// nothing else, so a cashier cannot tell which is which or why one of them does nothing. The exact
// figures are less tidy and strictly more useful, so the snap stands down rather than degrading the
// row it was meant to improve. Asked of the keys in the store's order, which need not be ascending
// (ADR-0160), so it compares each key with every other rather than with the one before it.
//
// The counter's pay pad (`Takeaway.tsx`) keeps a copy of this guard and of `tipKeys` below, in step
// by hand until a third screen needs them (`docs/design-principles.md`, rule of three).
function distinctAndSpendable(keys: readonly number[]): boolean {
  return keys.every((amount, index) => amount > 0 && keys.indexOf(amount) === index);
}

// The pay screen: the amount owed large, a cash pad with this currency's quick-cash denominations
// and its change, an optional tip, or card for the exact amount. On settlement it shows the gapless
// receipt number and the change to hand back; the table is now the floor's to clean.
//
// # The tip is optional, and that is a budget decision as much as a design one
//
// `docs/ui-ux.md` §6 caps a cash settle at three taps and this flow already spends all three
// (pay -> note -> take). A tip pad behind a button would be a fourth *required* tap and the step
// budget would fail the build, correctly. So the tip row sits in the flow already visible: a
// cashier who takes no tip taps nothing extra, and the tip's own taps are declared as their own
// task in `ui/scripts/step-tasks.mjs`.
/**
 * What the till says about the receipt (ADR-0100). An edge built before C2 sends no
 * `receipt_print`, so the old "Printing receipt…" wording is the fallback — the one case where the
 * till genuinely does not know whether paper came out.
 */
function receiptPrintKey(outcome: string | undefined): MessageKey {
  return outcome === undefined
    ? "pay.printing"
    : printOutcomeKey(outcome, "pay.printed", "pay.print_unavailable");
}

export function Pay() {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [billId, setBillId] = createSignal<string | null>(openBillFor(params.id) ?? null);
  const [error, setError] = createSignal<string | null>(null);
  const [tender, setTender] = createSignal<number | null>(null);
  // "Other amount": the cashier types what the guest handed over, for the pile no quick key names.
  // A string for the reason the discount's is one — a half-typed figure must stay as typed — and
  // only read while the pad is open, so tapping a quick key again puts that key back in charge.
  const [typing, setTyping] = createSignal(false);
  const [typedText, setTypedText] = createSignal("");
  // Splitting the bill evenly between guests who each pay their own share. `ways` is how many shares
  // and `taken` the shares already paid. They are held here until the last share lands and then
  // sent in one settle, because the edge settles a bill in one step: its payments must add up to the
  // total exactly (`pos_core::billing::settle`), and a half-paid bill is not a state it has. The
  // plan is kept per bill for the tab (`../state/split-plan`), so leaving or reloading keeps it.
  const { ways, setWays, taken, setTaken } = useSplitPlan(billId, () => done() !== null || voided());
  // The change handed back on the settle that closed the bill, summed over its payments, for the
  // settled panel: one payment's change, or what a split's cash shares were owed between them.
  const [givenChange, setGivenChange] = createSignal(0);
  // The tip, in minor units. Zero rather than null: there is no difference between "no tip" and "a
  // tip of nothing", and `Payment.tip` is optional on the wire so zero is what the edge would have
  // defaulted to anyway.
  const [tip, setTip] = createSignal(0);
  const [done, setDone] = createSignal<BillResponse | null>(null);
  // The corporate buyer, when a company asks for a tax invoice (ADR-0107). Behind a disclosure the
  // cashier only opens on request, so an ordinary sale costs no extra tap and the step budget for
  // the cash flow is untouched. Held here for the length of one settle and never stored: the till
  // keeps no customer list, which is the point — a cross-bill index of buyers is a profiling
  // feature with its own consent posture, not a side effect of issuing invoices.
  const [buyerName, setBuyerName] = createSignal("");
  const [buyerTaxCode, setBuyerTaxCode] = createSignal("");
  const [buyerAddress, setBuyerAddress] = createSignal("");
  // What the guest owes, as the edge assembled it (E5). Null until it arrives; the screen shows no
  // amount rather than a guess, because the till no longer has the means to guess one.
  const [check, setCheck] = createSignal<CheckResponse | null>(null);
  // The order lines the bill on screen covers, from the edge's read of that bill (ADR-0128). Null
  // until it lands, and on an edge too old to answer it: splitting by item partitions exactly these,
  // so the choice is only offered where they are known.
  const [covered, setCovered] = createSignal<readonly string[] | null>(null);
  // Splitting by item: the list of the bill's lines is open, and these are the ones picked for the
  // guest paying now. Nothing reaches the edge until the split is confirmed.
  const [picking, setPicking] = createSignal(false);
  const [picked, setPicked] = createSignal<ReadonlySet<string>>(new Set<string>());
  // Whether this bill was voided here (`../components/VoidBillPanel`). The manager's badge and PIN
  // are one pair for the void, the discount and the waive: only one of their panels is ever open,
  // and they live for the length of one refusal and are cleared with the panel.
  const [voided, setVoided] = createSignal(false);
  const approver = createApprover();
  // The fee whose waive is being reasoned about (ADR-0159 decision 5), and the name of the last one
  // waived, which the screen says back. The manager fields are the void's and the discount's: only
  // one panel is ever open.
  const [waiving, setWaiving] = createSignal<FeeLine | null>(null);
  const [waivedFee, setWaivedFee] = createSignal<string | null>(null);

  // This bill's pre-bill, for the guest who wants to check it on paper before paying — on a split
  // table, their own part (roadmap-v3 B2.1). What came of it, or null before the first press.
  const [preBill, setPreBill] = createSignal<string | null>(null);
  // A copy of the receipt just settled, for the guest who asks for one before they leave
  // (ADR-0164): what came of the last press, or null before the first. The same act as the Today
  // screen's reprint, marked COPY and counted the same way.
  const [copy, setCopy] = createSignal<ReprintResponse | null>(null);
  const printAgain = async (id: string) => {
    setError(null);
    try {
      setCopy(await api.reprintReceipt(id));
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };
  const printPreBill = async () => {
    const id = billId();
    if (id === null) {
      return;
    }
    setError(null);
    setPreBill(null);
    try {
      const printed = await api.printBillCheck(id);
      setPreBill(printed.prints[0] ?? null);
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  // What the bill on screen owes, read by its own id. Once a table is split, the table's check is
  // every open part together, and the guest at the till is asking about theirs (ADR-0128).
  //
  // An edge older than that read answers its address with the app's own page rather than a figure
  // (ADR-0111), and its table's check is the same figure for the one bill such an edge lets a till
  // show. Any other failure is shown rather than covered with the table's figure, which on a split
  // table is every part at once — a figure that would settle nobody's bill.
  const showBill = async (id: string) => {
    try {
      const bill = await loadBillCheck(id);
      if (billId() === id) {
        setCheck(bill);
        setCovered(bill.order_line_ids);
      }
    } catch (caught) {
      const olderEdge =
        caught instanceof SyntaxError || (caught instanceof ApiError && caught.status === 404);
      if (!olderEdge) {
        setError(errorMessage(caught));
        return;
      }
      try {
        const totals = await loadCheck(params.id);
        if (billId() === id) {
          setCheck(totals);
        }
      } catch (caught) {
        setError(errorMessage(caught));
      }
    }
  };

  onMount(() => {
    const existing = billId();
    if (existing !== null) {
      void showBill(existing);
      return;
    }
    // No bill open for this table yet: Take payment moves here at once and the bill is opened here,
    // beside a read of the table's figure (`../state/open-for-pay`). The same path serves a screen
    // reloaded, or reached by its address, before this device read what is open.
    openForPay(params.id)
      .then((opened) => {
        setBillId(opened.billId);
        setCheck(opened.check);
        return showBill(opened.billId);
      })
      .catch((caught: unknown) => setError(errorMessage(caught)));
  });

  // The edge's figure, in minor units, for the tender pad's arithmetic. Zero until the check lands,
  // which disables nothing the operator can get wrong: paying is refused without an amount owed.
  const total = () => check()?.total_due.amount_minor ?? 0;
  // The store's currency, taken from the edge's own figure rather than assumed — a store on any other
  // currency then renders and tenders correctly with no change here.
  const currency = () => check()?.total_due.currency_code ?? "";
  // What a split has put against the bill so far, and what is left of it.
  const takenApplied = () =>
    taken().reduce((sum, payment) => sum + payment.applied_to_bill.amount_minor, 0);
  const remaining = () => total() - takenApplied();
  // What this payment is for: the whole bill, or in a split the next guest's share. A share is what
  // is left divided by the shares still to pay, rounded up, and the last share is whatever remains,
  // so the shares add up to the total exactly (the edge refuses anything else) and none is more than
  // one minor unit off another. Counted with a remainder, not a division rounded up (ADR-0028).
  const due = () => {
    const n = ways();
    if (n === null) {
      return total();
    }
    const left = n - taken().length;
    const rest = remaining();
    if (left <= 1) {
      return rest;
    }
    const whole = (rest - (rest % left)) / left;
    return rest % left === 0 ? whole : whole + 1;
  };
  // What the guest handed over: the typed figure while "other amount" is open, otherwise the key the
  // cashier tapped. Null is "nothing chosen", which the settle reads as the exact amount; a typed
  // figure the till cannot read is null too, and the take-cash button refuses it below.
  const typedTender = () => parseWhole(typedText(), currencyExponent());
  const offered = () => (typing() ? typedTender() : tender());
  // Change is a subtraction of two amounts the operator can see — the edge's total and the note they
  // chose — shown as they tap. The authoritative figure is the one the settle records per payment
  // (`change_given`), which the receipt carries.
  // The tip comes out of the change, not out of the bill: the guest hands over one amount, the sale
  // takes its total and the tip is what is left behind on purpose. Subtracting it here is what makes
  // the figure on screen the same one the edge records — B1.3's second defect was exactly this
  // subtraction missing on the edge, so a till told a cashier to hand back money the guest had left.
  const change = () => {
    const chosen = offered();
    const owed = due() + tip();
    return chosen !== null && chosen >= owed ? chosen - owed : 0;
  };
  // How much a typed figure falls short of the sale plus the tip, or null when it covers them (or
  // nothing is typed yet). Shown, not only refused: "not enough" without the gap sends a cashier
  // back to the guest to do the subtraction the till could have done.
  const shortBy = () => {
    const typed = typing() ? typedTender() : null;
    const owed = due() + tip();
    return typed !== null && typed < owed ? owed - typed : null;
  };
  // A typed tender the edge would refuse: unreadable, or less than is owed. The button stays
  // disabled rather than answering with a refusal after the guest's money is on the counter.
  const typedTenderInvalid = () => typing() && (typedTender() === null || shortBy() !== null);
  // Opens the pad. A figure already typed survives a quick key closing the pad and this opening it
  // again, so a cashier who changes their mind twice does not type it twice.
  const typeTender = () => setTyping(true);

  // Tip keys as a share of the bill, plus a clear. Percentages rather than fixed amounts so they
  // scale with the check. The store's own keys, in its order: five, ten and fifteen percent until it
  // sets its own (ADR-0160 decision 2). At most three, because the row has four columns and one of
  // them is "none" — a fourth percentage would cost a line break on a phone for a choice a cashier
  // makes by tapping the nearest of three.
  //
  // `(total * percent) / 100` is what this line used to say, and it is a float division wearing an
  // integer's clothes. It was invisible for as long as every figure on the check was a round
  // thousand: a bill of 304,733₫ makes the 5% key 15,236.65, `Money.amount_minor` is an `i64`, and
  // the edge's deserializer refuses it — *"invalid type: floating point"*. What the cashier saw was
  // the generic store error, on the settle, with the guest's money already on the counter. Nothing
  // about the message said which of the four buttons they had pressed was the problem, and tapping
  // "no tip" made it go away, which is the kind of fix that becomes folklore.
  //
  // `percentOf` is the whole-minor-unit answer, rounded the way the edge rounds so the two agree.
  //
  // Whole is not yet spendable, which is the second half. 15,237₫ is an integer and still not an
  // amount a guest can leave: Vietnam's smallest note is 1,000 đồng, India's smallest coin is the
  // rupee. Where the store's country rounds its cash the keys round with it, so the button reads
  // 15,000₫ — what somebody would actually put on the table — and where it does not, Japan and the
  // United States among them, nothing is snapped and the exact share stands.
  //
  // The snap stands down on a small bill. Under about 20,000₫ a 1,000₫ increment swallows the gap
  // between 5% and 15% and the row collapses onto one amount; `distinctAndSpendable` catches that
  // and the exact figures are used instead. Better a key of 400₫ a cashier rounds in their head
  // than three identical buttons they cannot choose between.
  const tipKeys = () => {
    const exact = tipPercents().map((percent) => percentOf(total(), percent));
    const increment = cashRoundingIncrement();
    if (increment === null) {
      return exact;
    }
    const snapped = exact.map((amount) => roundToIncrement(amount, increment));
    return distinctAndSpendable(snapped) ? snapped : exact;
  };

  // The exact amount, plus this bill's own currency's banknotes that would cover it (roadmap E5).
  // These were VND's three notes regardless of where the store was, so a store on any other
  // currency was offered keys for amounts its guests cannot hand over. Keyed on `currency()` — the
  // bill's, not the store's — for the same reason every other figure here is: it is the edge's
  // answer for *this* bill. Before the check loads that is `""`, which has no note table and so
  // offers the exact amount alone; the total is zero then anyway.
  // Keyed on the sale *plus* the tip: with a tip added, a note that only covers the sale is not
  // enough money, and offering it would hand the cashier a key that cannot settle.
  const quickCash = () => {
    const owed = due() + tip();
    return [owed, ...quickCashFor(cashDenominations(), owed)];
  };

  // What the cashier typed, or nothing. A blank name means no buyer: the one field both Japan's
  // qualified invoice and India's Rule 46 require is the name, so a tax code with no name beside it
  // is an incomplete document rather than a partial one.
  const buyer = (): BuyerRequest | undefined => {
    const name = buyerName().trim();
    if (name === "") {
      return undefined;
    }
    const optional = (value: string) => (value.trim() === "" ? undefined : value.trim());
    return {
      name,
      tax_code: optional(buyerTaxCode()),
      address: optional(buyerAddress()),
    };
  };

  // The settle in flight, keyed by the tender tapped, which says so until the edge answers.
  const paying = useInFlight<string>(setError);
  // The bill on screen settled or voided on another till; not asked while this till's own act is out.
  const ended = () => (done() === null && !voided() && paying.busy() === null ? endedElsewhere(params.id, billId()) : null);
  const pay = (payments: PaymentRequest[]) =>
    paying.run(payments[payments.length - 1]?.method ?? "", async () => {
      const id = billId();
      if (id === null) {
        return;
      }
      const settled = await settle(id, payments, buyer());
      setGivenChange(
        payments.reduce(
          (sum, payment) =>
            sum +
            payment.tendered.amount_minor -
            payment.applied_to_bill.amount_minor -
            (payment.tip?.amount_minor ?? 0),
          0,
        ),
      );
      setDone(settled);
    });

  // Records one payment. Outside a split, or on a split's last share, that settles the bill with
  // every payment taken; before the last share it keeps this one and clears the pad for the next
  // guest.
  const record = (payment: PaymentRequest) => {
    const n = ways();
    if (n !== null && taken().length + 1 < n) {
      setTaken([...taken(), payment]);
      setTender(null);
      setTyping(false);
      setTypedText("");
      return;
    }
    void pay([...taken(), payment]);
  };

  // Starts an even split. The tip row goes with it and the tip is cleared: a tip is taken on a
  // tender, and which guest's tender carries it is a question this screen does not ask yet.
  const splitEvenly = (n: number) => {
    setWays(n);
    setTip(0);
    setTender(null);
    setTyping(false);
    setTypedText("");
  };

  const cancelSplit = () => {
    setWays(null);
    setTaken([]);
  };

  // Gives a taken share back: that guest has not paid it after all. The shares still to pay are
  // worked out again from what is left.
  const untake = (index: number) =>
    setTaken(taken().filter((_, position) => position !== index));

  const payCash = () => {
    // No note chosen means "exact", and exact now means the sale plus the tip — otherwise adding a
    // tip and tapping straight through would tender less than the guest owes.
    const chosen = offered() ?? due() + tip();
    record({
      method: "PAYMENT_METHOD_CASH",
      tendered: money(currency(), chosen),
      applied_to_bill: money(currency(), due()),
      tip: money(currency(), tip()),
    });
  };

  const payCard = () =>
    record({
      method: "PAYMENT_METHOD_CARD",
      // A card takes the sale plus whatever tip was added — there is no note to choose and no
      // change to give, so the tendered amount is the whole of what the terminal will capture.
      tendered: money(currency(), due() + tip()),
      applied_to_bill: money(currency(), due()),
      tip: money(currency(), tip()),
    });

  // A bank transfer to the store's QR, which the cashier records once they have seen it arrive. The
  // exact amount, like a card: the guest scans for what the bill says and there is no change to give.
  // Nothing here reaches a bank, which is why the button says the money *arrived* rather than asking
  // for it: no integration yet has a bank or gateway tell the edge a transfer landed (roadmap B10.2
  // is India's UPI equivalent).
  const payQr = () =>
    record({
      method: "PAYMENT_METHOD_QR",
      tendered: money(currency(), due() + tip()),
      applied_to_bill: money(currency(), due()),
      tip: money(currency(), tip()),
    });

  // Puts another of the table's bills on screen — the part just split off, or the next one to pay —
  // carrying nothing over from the last: its own amount, and no tender, tip, share, buyer, open
  // panel or manager's PIN from the guest before.
  const switchTo = (id: string) => {
    setCheck(null);
    setCovered(null);
    setDone(null);
    setError(null);
    setCopy(null);
    setTender(null);
    setTyping(false);
    setTypedText("");
    setTip(0);
    setWays(null);
    setTaken([]);
    setGivenChange(0);
    setBuyerName("");
    setBuyerTaxCode("");
    setBuyerAddress("");
    discount.closeDiscount();
    voiding.closeVoid();
    setBillId(id);
    void showBill(id);
  };

  // Optimization: Memoize coveredLines and isPart using createMemo to avoid redundant O(N log N) re-filtering
  // and sorting on every access/re-render. Placed below `covered` signal to avoid TDZ errors.
  const coveredLines = createMemo(() => {
    const ids = covered();
    if (ids === null) {
      return [];
    }
    return linesForTable(params.id)
      .filter((line) => ids.includes(line.orderLineId))
      .sort((a, b) => a.orderLineId.localeCompare(b.orderLineId));
  });

  // Whether the bill on screen is a part of its table rather than all of it: it covers fewer lines
  // than the table has live ones. Then the screen says what this guest is paying for.
  const isPart = createMemo(() => {
    const ids = covered();
    const live = linesForTable(params.id).filter((line) => line.state !== "ORDER_LINE_STATE_VOIDED");
    return ids !== null && ids.length < live.length;
  });
  const partItems = () =>
    coveredLines()
      .map((line) => `${formatQuantity(line.quantityMilli)} × ${line.name}`)
      .join(", ");
  // The seat every line of the part was ordered for, when they share one: a part split off by seat
  // says whose it is.
  const partSeat = () => {
    const seats = new Set(coveredLines().map((line) => line.seat));
    const [only] = [...seats];
    return seats.size === 1 ? only : undefined;
  };

  // The bill's lines by the seat each was ordered for, seats in order and the table's own lines last:
  // the partition a split by seat proposes (ADR-0128). Null when it would not split anything — seats
  // off, or everything on one seat. A line this device does not know goes with the table's, so the
  // parts still cover exactly what the bill does, which is the one shape the edge accepts.
  const seatParts = (): string[][] | null => {
    const ids = covered();
    if (ids === null || !seatsEnabled()) {
      return null;
    }
    const seatOf = new Map(linesForTable(params.id).map((line) => [line.orderLineId, line.seat]));
    const bySeat = new Map<number, string[]>();
    const tables: string[] = [];
    for (const lineId of ids) {
      const seat = seatOf.get(lineId);
      if (seat === undefined) {
        tables.push(lineId);
      } else {
        bySeat.set(seat, [...(bySeat.get(seat) ?? []), lineId]);
      }
    }
    const parts = [...bySeat.entries()].sort(([a], [b]) => a - b).map(([, lines]) => lines);
    if (tables.length > 0) {
      parts.push(tables);
    }
    return parts.length > 1 ? parts : null;
  };
  // One tap: a bill per seat, from the seat each line already carries, and the first on screen.
  const splitBySeat = async () => {
    const id = billId();
    const parts = seatParts();
    if (id === null || parts === null) {
      return;
    }
    setError(null);
    try {
      const [first] = await splitBill(id, parts);
      if (first !== undefined) {
        switchTo(first);
      }
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  // Opens the bill's lines for the cashier to pick what one guest is paying for (ADR-0128).
  const splitByItem = () => {
    setError(null);
    setPicked(new Set<string>());
    setPicking(true);
  };
  const pickLine = (lineId: string) => {
    const next = new Set(picked());
    if (next.has(lineId)) {
      next.delete(lineId);
    } else {
      next.add(lineId);
    }
    setPicked(next);
  };
  // Something picked and something left. A part with nothing in it is refused by the edge, and
  // picking every line is the bill as it already is.
  const canSplitOff = () => picked().size > 0 && picked().size < (covered()?.length ?? 0);
  // Splits the picked lines off into a bill of their own and puts it on screen; the rest stay open
  // on the table as the next bill. A partition of exactly what the bill covers, which is the one
  // shape the edge accepts.
  const splitOff = async () => {
    const id = billId();
    const ids = covered();
    if (id === null || ids === null || !canSplitOff()) {
      return;
    }
    const chosen = ids.filter((lineId) => picked().has(lineId));
    const rest = ids.filter((lineId) => !picked().has(lineId));
    setError(null);
    try {
      const [first] = await splitBill(id, [chosen, rest]);
      setPicking(false);
      if (first !== undefined) {
        switchTo(first);
      }
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  // The table's other open bills, which putting the bill back together folds into this one.
  const otherParts = () => openBillsFor(params.id).filter((id) => id !== billId());
  // Undoing a split is a merge into the bill on screen, never an edit (ADR-0128 decision 10).
  const putBack = async () => {
    const id = billId();
    const others = otherParts();
    if (id === null || others.length === 0) {
      return;
    }
    setError(null);
    try {
      await mergeBills(id, others);
      setTender(null);
      setTyping(false);
      setTypedText("");
      await showBill(id);
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  // After one guest's part is paid: the next bill still open on the table.
  const nextBill = (id: string) => switchTo(id);

  // Voiding the bill and taking money off (`../components/VoidBillPanel`, `../components/DiscountPanel`).
  const voiding = useVoidBill({
    billId,
    approver,
    report: setError,
    run: (act) => paying.run("void", act),
    onVoided: () => setVoided(true),
  });
  const discount = useDiscount({ billId, currency, approver, report: setError, onApplied: setCheck });

  // Waiving a fee (ADR-0159 decision 5): offered on a fee whose rule allows it, on a bill nothing
  // has been taken against yet, to a person who may waive and where the store publishes a reason to
  // cite. `billing.fee.waive` asks for an approver where the person holds it with approval, and
  // where the store does not enforce each person's own set, always.
  const waiveAsks = () => asksApprover("billing.fee.waive");
  const readyToWaive = () => !waiveAsks() || approver.typed();
  const canWaive = (fee: FeeLine) =>
    fee.waivable === true &&
    billId() !== null &&
    taken().length === 0 &&
    can("billing.fee.waive") &&
    reasonsFor(WAIVE_FEE).length > 0;

  const closeWaive = () => {
    setWaiving(null);
    approver.clear();
  };

  // The first tap opens the reasons for this fee; nothing is written until one is chosen. A
  // discount or a void half begun is put away, as the three share the manager's fields.
  const askWaiveFee = (fee: FeeLine) => {
    setError(null);
    setWaivedFee(null);
    discount.closeDiscount();
    voiding.closeVoid();
    setWaiving(fee);
  };

  // The second tap: waive it, citing this reason. The edge answers with the whole bill as it now
  // stands and the screen takes that figure, because the fee's tax leaves with it.
  const waiveFeeReason = (reasonCodeId: string) => {
    const id = billId();
    const fee = waiving();
    if (id === null || fee === null) {
      return;
    }
    setError(null);
    void waiveFee(id, fee.fee_id, reasonCodeId, waiveAsks() ? approver.approval() : undefined)
      .then((totals) => {
        setCheck(totals);
        setWaivedFee(fee.display_name);
        closeWaive();
      })
      .catch((caught: unknown) => setError(errorMessage(caught)));
  };

  // What the screen becomes once the bill is voided: no money was taken, and the order is still
  // sitting on the table to be charged again. The way back is the order, not the floor — a voided
  // bill usually means the cashier is about to open the right one.
  const voidedPanel = () => (
    <div class="mt-6 rounded-token border border-line bg-surface p-4" data-outcome="bill-voided">
      <p class="text-lg font-semibold">{t("pay.void_done")}</p>
      <p class="mt-1 text-sm text-ink-muted">{t("pay.void_done_hint")}</p>
      <button
        type="button"
        class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
        onClick={() => navigate(`/table/${params.id}`)}
      >
        {t("common.back_order")}
      </button>
    </div>
  );

  return (
    <section class="mx-auto max-w-xl p-4">
      <a
        href={`/table/${params.id}`}
        class="inline-flex min-h-touch items-center text-sm text-ink-muted no-underline"
      >
        {t("common.back_order")}
      </a>

      <Show
        when={done()}
        fallback={
          <Show when={!voided() && ended() === null} fallback={voided() ? voidedPanel() : <BillEnded how={ended() ?? "settled"} tableId={params.id} />}>
            <p class="mt-4 text-sm text-ink-muted">{t("pay.amount_due")}</p>
            <Show when={check()} fallback={<p class="h-8 w-40 animate-pulse rounded-token bg-surface-raised" role="status" aria-label={t("pay.amount_loading")} data-outcome="amount-loading" />}>
              {(totals) => (
                <>
                  {/* A reduced bill says so and by how much. A total that simply came out smaller
                      than the food on the table is the kind of figure a guest queries and nobody
                      at the till can explain. Shown only when there is one, because a row of
                      zeroes on every bill is noise the eye learns to skip. */}
                  {/* And once it is reduced, the bill's own arithmetic around it: the lines before tax,
                      what came off, each fee, and the tax the edge worked out again on what is left —
                      so the figures add up to the total rather than leaving a gap nobody can explain
                      (UX-11). The tax line is the edge's `tax_total`, never the till's. */}
                  <Show when={reduced(totals())}>
                    <CheckRow label={t("order.subtotal")} amount={formatAmount(totals().subtotal)} outcome="bill-subtotal" />
                  </Show>
                  <Show when={totals().discount_total.amount_minor > 0}>
                    <div data-outcome="bill-discounted"><CheckRow label={t("pay.discount_applied")} amount={formatAmount(totals().discount_total)} minus /></div>
                  </Show>
                  <Show when={totals().comp_total.amount_minor > 0}>
                    <CheckRow label={t("pay.comp_applied")} amount={formatAmount(totals().comp_total)} minus />
                  </Show>
                  {/* Each fee the bill carries, under its own name (ADR-0159). The figure below
                      already includes them: the till shows the edge's arithmetic, never its own.
                      A fee its rule lets staff waive carries **Waive** (decision 5). */}
                  <For
                    each={(totals().fee_lines ?? []).filter(
                      (fee) => fee.amount.amount_minor !== 0,
                    )}
                  >
                    {(fee) => (
                      <p
                        class="flex items-center justify-between gap-2 text-sm text-ink-muted"
                        data-outcome="bill-fee"
                      >
                        <span>{fee.display_name}</span>
                        <span class="flex items-center gap-2">
                          <span class="tabular-nums">{formatAmount(fee.amount)}</span>
                          <Show when={canWaive(fee) && waiving() === null}>
                            <button
                              type="button"
                              class="min-h-touch rounded-token border border-line px-3 text-sm"
                              data-step="askWaiveFee"
                              onClick={() => askWaiveFee(fee)}
                            >
                              {t("pay.waive_fee")}
                            </button>
                          </Show>
                        </span>
                      </p>
                    )}
                  </For>
                  <Show when={reduced(totals())}>
                    <CheckRow label={t("order.tax")} amount={formatAmount(totals().tax_total)} outcome="bill-tax" />
                  </Show>
                  <p class="text-2xl font-semibold tabular-nums">
                    {formatAmount(totals().total_due)}
                  </p>
                </>
              )}
            </Show>
            {/*
              Waiving a fee (ADR-0159 decision 5): the reasons the store publishes for a waive, and
              the manager's badge and PIN where the person needs an approver, typed rather than
              tapped, so the act is three taps from the order screen — pay, waive, reason — as the
              discount is. The reasons stay disabled until the approver is typed.
            */}
            <Show when={waiving()}>
              {(fee) => (
                <div class="mt-3 rounded-token border border-line bg-surface p-3">
                  <h2 class="font-semibold">{t("pay.waive_title", { fee: fee().display_name })}</h2>
                  <Show when={waiveAsks()}>
                    <p class="mt-1 text-sm text-ink-muted">{t("pay.waive_manager")}</p>
                    <ApproverFields
                      id="waive-fee-approver"
                      code={approver.code()}
                      pin={approver.pin()}
                      onCode={approver.setCode}
                      onPin={approver.setPin}
                      codeLabel={t("pay.approver_code")}
                      pinLabel={t("pay.approver_pin")}
                    />
                  </Show>
                  <p class="mt-3 text-sm text-ink-muted">{t("pay.waive_reason")}</p>
                  <div class="mt-2 grid grid-cols-2 gap-2">
                    <For each={reasonsFor(WAIVE_FEE)}>
                      {(reason) => (
                        <button
                          type="button"
                          class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                          disabled={!readyToWaive()}
                          data-step="waiveFeeReason"
                          onClick={() => waiveFeeReason(reason.reason_code_id)}
                        >
                          {reason.display_name}
                        </button>
                      )}
                    </For>
                  </div>
                  <button
                    type="button"
                    class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
                    onClick={() => closeWaive()}
                  >
                    {t("common.cancel")}
                  </button>
                </div>
              )}
            </Show>
            <Show when={waivedFee()}>
              {(name) => (
                <p class="mt-1 text-sm text-ink-muted" role="status" data-outcome="fee-waived">
                  {t("pay.fee_waived", { fee: name() })}
                </p>
              )}
            </Show>
            {/*
              A part of a split table says what it is for (ADR-0128), so the guest can check it
              before paying, and the cashier is not reading out a total that matches nobody's order.
              Putting the bill back together is the way out of a split made by mistake: a merge into
              the bill on screen, which the edge records rather than edits.
            */}
            <Show when={isPart()}>
              <p class="mt-1 text-sm text-ink-muted" data-outcome="bill-part">
                {partSeat() === undefined
                  ? t("pay.part_for", { items: partItems() })
                  : t("pay.part_for_seat", { seat: partSeat() ?? 0, items: partItems() })}
              </p>
            </Show>
            <Show
              when={otherParts().length > 0 && taken().length === 0 && can("billing.bill.split")}
            >
              <button
                type="button"
                class="mt-2 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted"
                onClick={() => void putBack()}
              >
                {t("pay.put_back", { count: otherParts().length + 1 })}
              </button>
            </Show>

            {/* Before any money is taken: once a payment is against the bill, the guest has agreed
                the figure and the receipt is the paper that follows. */}
            <Show when={taken().length === 0 && can("billing.bill.open")}>
              <button
                type="button"
                class="mt-2 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted disabled:opacity-50"
                disabled={billId() === null || check() === null}
                onClick={() => void printPreBill()}
              >
                {t("order.pre_bill")}
              </button>
            </Show>
            <Show when={preBill()}>
              {(outcome) => (
                <p class="mt-1 text-sm text-ink-muted" role="status" data-outcome="bill-pre-bill-print">
                  {t(printOutcomeKey(outcome(), "order.pre_bill_printed"))}
                </p>
              )}
            </Show>

            <Show when={error()}>
              {(message) => (
                <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                  {message()}
                </p>
              )}
            </Show>

            {/*
              Splitting the bill evenly (the till review's first gap). A row of guest counts rather
              than a button that opens one, so a split costs one tap before the guests' own: a
              screen that asked "split?" first would make every split a tap longer and every other
              settle no shorter.
            */}
            <Show when={can("billing.payment.take")}>
            <h2 class="mt-6 mb-2 text-sm font-semibold text-ink-muted">{t("pay.split")}</h2>
            <div
              class={
                splitWays().length > SPLIT_COLUMNS
                  ? "-mx-1 flex gap-2 overflow-x-auto px-1 pb-1"
                  : "grid grid-cols-5 gap-2"
              }
            >
              <For each={splitWays()}>
                {(n) => (
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line bg-surface tabular-nums disabled:opacity-50"
                    classList={{
                      "border-accent": ways() === n,
                      "min-w-touch shrink-0": splitWays().length > SPLIT_COLUMNS,
                    }}
                    aria-pressed={ways() === n}
                    aria-label={t("pay.split_ways", { count: n })}
                    disabled={taken().length > 0 || picking()}
                    data-step="splitEvenly"
                    onClick={() => splitEvenly(n)}
                  >
                    {n}
                  </button>
                )}
              </For>
            </div>
            </Show>
            <Show when={ways()}>
              {(n) => (
                <div class="mt-3 rounded-token border border-line bg-surface p-3">
                  <p class="text-sm text-ink-muted">
                    {t("pay.share_of", { part: taken().length + 1, count: n() })}
                  </p>
                  <p class="text-2xl font-semibold tabular-nums" data-outcome="share-due">
                    {formatAmount(money(currency(), due()))}
                  </p>
                  <Show when={taken().length > 0}>
                    <ul class="mt-2 flex list-none flex-col gap-1 p-0">
                      <For each={taken()}>
                        {(payment, index) => (
                          <li
                            class="flex items-center justify-between gap-2 text-sm"
                            data-outcome="share-taken"
                          >
                            <span>
                              {t("pay.share_taken", {
                                part: index() + 1,
                                method: t(methodKey(payment.method)),
                              })}
                            </span>
                            <span class="flex items-center gap-2">
                              <span class="tabular-nums">{formatAmount(payment.applied_to_bill)}</span>
                              <button
                                type="button"
                                class="min-h-touch rounded-token border border-line px-3"
                                onClick={() => untake(index())}
                              >
                                {t("pay.share_untake")}
                              </button>
                            </span>
                          </li>
                        )}
                      </For>
                    </ul>
                    <p class="mt-2 text-sm text-ink-muted">
                      {t("pay.remaining")}:{" "}
                      <span class="tabular-nums">{formatAmount(money(currency(), remaining()))}</span>
                    </p>
                  </Show>
                  <button
                    type="button"
                    class="mt-2 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted"
                    onClick={() => cancelSplit()}
                  >
                    {t("pay.split_cancel")}
                  </button>
                </div>
              )}
            </Show>

            {/*
              Splitting by item (ADR-0128): pick the lines one guest is paying for and split them off
              into a bill of their own, and what is left stays open as the next bill. Behind a button
              rather than a list on every bill, because the list would push the tenders below the fold
              on a phone for every settle that never splits.
            */}
            <Show when={(covered()?.length ?? 0) > 1 && !picking() && can("billing.bill.split")}>
              <div class="mt-2 flex gap-2">
                <button
                  type="button"
                  class="min-h-touch flex-1 rounded-token border border-line bg-surface disabled:opacity-50"
                  disabled={ways() !== null || taken().length > 0}
                  data-step="splitByItem"
                  onClick={() => splitByItem()}
                >
                  {t("pay.split_by_item")}
                </button>
                {/* By seat, where the store assigns them: the partition is already written on the
                    lines, so it costs one tap and no picking. */}
                <Show when={seatParts()}>
                  {(parts) => (
                    <button
                      type="button"
                      class="min-h-touch flex-1 rounded-token border border-line bg-surface disabled:opacity-50"
                      disabled={ways() !== null || taken().length > 0}
                      data-step="splitBySeat"
                      onClick={() => void splitBySeat()}
                    >
                      {t("pay.split_by_seat", { count: parts().length })}
                    </button>
                  )}
                </Show>
              </div>
            </Show>
            <Show when={picking()}>
              <div class="mt-3 rounded-token border border-line bg-surface p-3">
                <p class="text-sm text-ink-muted">{t("pay.split_pick")}</p>
                <ul class="mt-2 flex list-none flex-col gap-2 p-0">
                  <For each={coveredLines()}>
                    {(line) => (
                      <li>
                        <button
                          type="button"
                          class="flex min-h-touch w-full items-center justify-between gap-2 rounded-token border border-line px-3 text-left"
                          classList={{ "border-2 border-accent font-semibold": picked().has(line.orderLineId) }}
                          aria-pressed={picked().has(line.orderLineId)}
                          data-step="pickLine"
                          onClick={() => pickLine(line.orderLineId)}
                        >
                          <span>{`${formatQuantity(line.quantityMilli)} × ${line.name}`}</span>
                          <span class="tabular-nums">{formatAmount(line.lineTotal)}</span>
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
                <button
                  type="button"
                  class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
                  disabled={!canSplitOff()}
                  data-step="splitOff"
                  onClick={() => void splitOff()}
                >
                  {t("pay.split_off", { count: picked().size })}
                </button>
                <button
                  type="button"
                  class="mt-2 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted"
                  onClick={() => setPicking(false)}
                >
                  {t("common.cancel")}
                </button>
              </div>
            </Show>

            {/* The tip, the buyer and the tenders are taking payment (ADR-0158). */}
            <Show when={can("billing.payment.take")}>
            {/*
              No tip key left to offer, and the row is not drawn: with no amount to type, a lone
              "No tip" would offer nothing (ADR-0160 decision 2).
            */}
            <Show when={tipsEnabled() && ways() === null && tipPercents().length > 0}>
              <h2 class="mt-6 mb-2 text-sm font-semibold text-ink-muted">{t("pay.tip")}</h2>
              <div class="grid grid-cols-4 gap-2">
                <button
                  type="button"
                  class="min-h-touch rounded-token border border-line bg-surface"
                  classList={{ "border-accent": tip() === 0 }}
                  onClick={() => setTip(0)}
                >
                  {t("pay.tip_none")}
                </button>
                <For each={tipKeys()}>
                  {(amount) => (
                    <button
                      type="button"
                      class="min-h-touch rounded-token border border-line bg-surface tabular-nums"
                      classList={{ "border-accent": tip() === amount && amount > 0 }}
                      data-step="setTip"
                      onClick={() => setTip(amount)}
                    >
                      {formatAmount(money(currency(), amount))}
                    </button>
                  )}
                </For>
              </div>
            </Show>

            <details class="mt-6 rounded-token border border-line bg-surface px-3 py-2">
              <summary class="min-h-touch cursor-pointer text-sm font-semibold text-ink-muted">
                {t("pay.buyer")}
              </summary>
              <p class="mt-1 text-sm text-ink-muted">{t("pay.buyer_hint")}</p>
              <label class="mt-3 block text-sm">
                {t("pay.buyer_name")}
                <input
                  type="text"
                  class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                  value={buyerName()}
                  onInput={(event) => setBuyerName(event.currentTarget.value)}
                />
              </label>
              <label class="mt-2 block text-sm">
                {t("pay.buyer_tax_code")}
                <input
                  type="text"
                  class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                  value={buyerTaxCode()}
                  onInput={(event) => setBuyerTaxCode(event.currentTarget.value)}
                />
              </label>
              <label class="mt-2 block text-sm">
                {t("pay.buyer_address")}
                <input
                  type="text"
                  class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                  value={buyerAddress()}
                  onInput={(event) => setBuyerAddress(event.currentTarget.value)}
                />
              </label>
            </details>

            <Show when={tenderAccepted("PAYMENT_METHOD_CASH")}>
            <h2 class="mt-6 mb-2 text-sm font-semibold text-ink-muted">{t("pay.cash")}</h2>
            <fieldset class="grid min-w-0 grid-cols-2 gap-2" disabled={check() === null} data-outcome="quick-cash">
              <For each={quickCash()}>
                {(amount) => (
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line bg-surface tabular-nums disabled:opacity-50"
                    classList={{ "border-accent": !typing() && tender() === amount }}
                    data-step="setTender"
                    onClick={() => {
                      setTyping(false);
                      setTender(amount);
                    }}
                  >
                    {amount === due() ? t("pay.exact") : formatAmount(money(currency(), amount))}
                  </button>
                )}
              </For>
              <button
                type="button"
                class="min-h-touch rounded-token border border-line bg-surface disabled:opacity-50"
                classList={{ "border-accent": typing() }}
                aria-expanded={typing()}
                data-step="typeTender"
                onClick={() => typeTender()}
              >
                {t("pay.other_amount")}
              </button>
            </fieldset>
            <Show when={typing()}>
              <p class="mt-3 text-sm text-ink-muted">{t("pay.tendered")}</p>
              <p class="text-2xl font-semibold tabular-nums" data-outcome="typed-tender">
                {typedTender() === null ? "—" : formatAmount(money(currency(), typedTender() ?? 0))}
              </p>
              <Keypad value={typedText()} onChange={setTypedText} data-step="tenderKeypad" />
              <Show when={shortBy()}>
                {(gap) => (
                  <p class="mt-2 text-sm text-danger" role="status">
                    {t("pay.short_by", { amount: formatAmount(money(currency(), gap())) })}
                  </p>
                )}
              </Show>
            </Show>
            <p class="mt-2 text-sm text-ink-muted">
              {t("pay.change")}: <span class="tabular-nums">{formatAmount(money(currency(), change()))}</span>
            </p>
            </Show>

            {/*
              Every tender waits for the edge's figure, and so do the cash keys above it, whose "Exact"
              is nothing until the figure lands. Before it lands the amount owed reads as
              nothing, and a tap then would ask the edge to settle for nothing — which it refuses, with
              the guest's money already on the counter. A bill put on screen mid-flow (the next part of
              a split) starts in exactly that state, so the window is not only the first load's.
            */}
            <DrawerOwner own approver />
            <div class="mt-4 flex flex-col gap-2">
              <Show when={tenderAccepted("PAYMENT_METHOD_CASH")}>
              <button
                type="button"
                class="min-h-money rounded-token bg-primary text-lg font-semibold text-primary-ink disabled:opacity-50"
                disabled={check() === null || typedTenderInvalid() || paying.busy() !== null}
                data-step="payCash"
                onClick={() => payCash()}
              >
                {t(paying.busy() === "PAYMENT_METHOD_CASH" ? "pay.paying" : "pay.take_cash")}
              </button>
              </Show>
              <Show when={tenderAccepted("PAYMENT_METHOD_CARD")}>
                <button
                  type="button"
                  class="min-h-touch rounded-token border border-line bg-surface disabled:opacity-50"
                  disabled={check() === null || paying.busy() !== null}
                  data-step="payCard"
                  onClick={() => payCard()}
                >
                  {t(paying.busy() === "PAYMENT_METHOD_CARD" ? "pay.paying" : "pay.card")}
                </button>
              </Show>
              <Show when={tenderAccepted("PAYMENT_METHOD_QR")}>
                <button
                  type="button"
                  class="min-h-touch rounded-token border border-line bg-surface disabled:opacity-50"
                  disabled={check() === null || paying.busy() !== null}
                  data-step="payQr"
                  onClick={() => payQr()}
                >
                  {t(paying.busy() === "PAYMENT_METHOD_QR" ? "pay.paying" : "pay.qr")}
                </button>
                <p class="text-sm text-ink-muted">{t("pay.qr_hint")}</p>
              </Show>
            </div>
            </Show>

            {/* Taking money off and voiding the bill: each its own panel, shared with the counter. */}
            <Show
              when={discount.open()}
              fallback={
                <Show when={can("billing.discount.apply")}>
                <button
                  type="button"
                  class="mt-6 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted disabled:opacity-50"
                  disabled={billId() === null || reasonsFor(DISCOUNT).length === 0 || taken().length > 0}
                  aria-describedby="why-reasons"
                  data-step="askDiscount"
                  onClick={() => discount.askDiscount()}
                >
                  {t("pay.discount")}
                </button>
                </Show>
              }
            >
              <DiscountPanel state={discount} approver={approver} data-step="discountReason" onSubmit={(id) => discount.discountReason(id)} />
            </Show>

            <Show
              when={voiding.open()}
              fallback={
                <Show when={can("billing.bill.void")}>
                <button
                  type="button"
                  class="mt-6 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted disabled:opacity-50"
                  disabled={billId() === null || reasonsFor(VOID_BILL).length === 0 || taken().length > 0}
                  aria-describedby="why-reasons"
                  data-step="askVoidBill"
                  onClick={() => voiding.askVoidBill()}
                >
                  {t("pay.void")}
                </button>
                <WhyUnavailable entry="reason_codes" id="why-reasons" />
                </Show>
              }
            >
              <VoidBillPanel state={voiding} approver={approver} data-step="voidBillReason" onSubmit={(id) => voiding.voidBillReason(id)} />
            </Show>
          </Show>
        }
      >
        {(bill) => (
          // The settled block: what a pay flow has to reach, and what an undeclared extra tap would
          // stop it reaching (ADR-0109).
          <div class="mt-6 rounded-token border border-line bg-surface p-4" data-outcome="settled">
            <p class="text-lg font-semibold text-ok">{t("pay.settled")}</p>
            <p class="mt-2 tabular-nums">
              {t("pay.receipt", { number: bill().receipt_number ?? 0 })}
            </p>
            {/* The change is what the cashier acts on next, so it is the largest figure here (UX-15). */}
            <Show when={givenChange() > 0}>
              <p class="mt-3 text-sm text-ink-muted">{t("pay.change")}<span class="block text-4xl font-semibold text-ink tabular-nums" data-outcome="change-due">{formatAmount(money(currency(), givenChange()))}</span></p>
            </Show>
            <Show when={bill().print_receipt}>
              <p
                class="mt-1 text-sm"
                classList={{
                  "text-ink-muted":
                    bill().receipt_print === undefined || bill().receipt_print === "PRINTED",
                  "text-danger":
                    bill().receipt_print !== undefined && bill().receipt_print !== "PRINTED",
                }}
              >
                {t(receiptPrintKey(bill().receipt_print))}
              </p>
            </Show>
            {/*
              A drawer that should have sprung and did not (ADR-0165). A store with no drawer marked
              says nothing here: `NO_DRAWER` is its ordinary state, not news for the cashier.
            */}
            <Show when={bill().drawer_open === "DRAWER_UNAVAILABLE"}>
              <p class="mt-1 text-sm text-danger" role="status" data-outcome="drawer-unavailable">
                {t("pay.drawer_unavailable")}
              </p>
            </Show>
            <Show when={can("billing.receipt.reprint")}>
            <button
              type="button"
              class="mt-3 min-h-touch w-full rounded-token border border-line text-sm"
              data-step="printAgain"
              onClick={() => void printAgain(bill().bill_id)}
            >
              {t("pay.print_again")}
            </button>
            </Show>
            <Show when={copy()}>
              {(printed) => (
                <p class="mt-1 text-sm text-ink-muted" role="status" data-outcome="receipt-reprinted">
                  {t(printOutcomeKey(printed().receipt_print, "pay.copy_printed"), {
                    number: printed().copy_number,
                  })}
                </p>
              )}
            </Show>
            <Show when={error()}>
              {(message) => (
                <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                  {message()}
                </p>
              )}
            </Show>
            {/*
              A split table's next guest (ADR-0128). The table waits for the last part, so the pay
              screen offers it straight away rather than sending the cashier back through the order.
            */}
            <Show when={openBillFor(params.id)}>
              {(next) => (
                <button
                  type="button"
                  class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
                  data-step="nextBill"
                  onClick={() => nextBill(next())}
                >
                  {t("pay.next_bill", { count: openBillsFor(params.id).length })}
                </button>
              )}
            </Show>
            <button
              type="button"
              class="mt-4 min-h-touch w-full rounded-token font-semibold"
              classList={{
                "bg-primary text-primary-ink": openBillFor(params.id) === undefined,
                "border border-line": openBillFor(params.id) !== undefined,
              }}
              onClick={() => navigate("/")}
            >
              {t("pay.back_floor")}
            </button>
          </div>
        )}
      </Show>
    </section>
  );
}
