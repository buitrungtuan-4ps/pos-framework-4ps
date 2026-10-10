// Fees ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md)).
//
// A fee — a service charge, a packaging fee, a delivery charge — is a rule every bill applies: a
// percentage of the lines it counts, or an amount per bill or per unit, on the channels and items it
// names, taxed the way its lines are, not at all, or at one tax class. This screen writes those
// rules and does no arithmetic of its own: the sums are `pos_core::billing`'s, in the cloud's
// sample bill and on the till alike.
//
// # One rule, many stores
//
// A rule is written for every store, one brand or one store, and for each fee a store runs the most
// specific rule there is: its own, else its brand's, else every store's. The scope picker says where
// the operator is writing. The list shows the rules written there and, at a narrower scope, the ones
// that reach it from a wider one, each of which can be overridden here. For one store the screen
// also shows what the store runs and anything that stops it applying a rule.
//
// # A save publishes
//
// A save or a delete republishes every store it reaches in the same request and answers per store:
// published, unchanged, refused (the store keeps the fees it had, and the screen names the rule it
// cannot apply and why) or failed. The screen lists each store's answer and offers to publish again
// to the ones that did not take it. "Saved" alone would be the lie ADR-0122's batch report was
// written against.
//
// # Try it first
//
// A fee is part of a price, so a mistake in one is a pricing mistake (ADR-0159, consequences). The
// editor assembles a sample bill at one of the stores the rule reaches — its own menu, tax table and
// rounding, with the rule as typed in place of what is written — before anything is saved
// (`POST /admin/fees/preview`, which writes nothing). New rules start from the owner's defaults:
// taxed the way their lines are, after discounts, net of tax, not waivable.
//
// Nothing on this screen is personal data: fee codes, names and figures, and the names of stores,
// brands, items, categories and tax classes.

import { createEffect, createMemo, createSignal, For, Index, on, Show } from "solid-js";

import { ApiError, api } from "../api/client";
import {
  SALES_CHANNELS,
  type EffectiveFee,
  type EffectiveFees,
  type FeeItems,
  type FeeKind,
  type FeePublishResult,
  type FeeRule,
  type FeeRuleFields,
  type FeeScope,
  type FeeTax,
  type Json,
  type SalesChannel,
  type SampleBill,
} from "../api/types";
import { LOCALES, type Locale, localeName, type MessageKey, t } from "../i18n";
import { useEntityCrud } from "../lib/entity-crud";
import {
  draftFromRule,
  draftGap,
  type DraftGap,
  FEE_ITEM_SCOPES,
  FEE_KINDS,
  FEE_TAXES,
  type FeeDraft,
  menuOffers,
  newDraft,
  percentText,
  ratioFromPercent,
  ruleFromDraft,
  storeCurrency,
} from "../lib/fees";
import { LOADING, type Panel, panelOf } from "../lib/panel";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { formatAmount } from "../state/money";
import { storeId, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  ComboboxField,
  MoneyField,
  MultiComboboxField,
  NumberField,
  PageHeader,
  SelectField,
  Skeleton,
  StatusBadge,
  SwitchField,
  TextField,
} from "../components/ui";
import {
  CLIENT_PAGE_SIZE,
  type Column,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  FormSection,
  RowActions,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";
import { CHANNEL_LABEL } from "./catalog/shared";

const TENANT: FeeScope = "FEE_SCOPE_TENANT";
const BRAND: FeeScope = "FEE_SCOPE_BRAND";
const STORE: FeeScope = "FEE_SCOPE_STORE";

/** The three scopes, widest first, as the picker offers them. */
const SCOPES: readonly { readonly scope: FeeScope; readonly label: MessageKey }[] = [
  { scope: TENANT, label: "fees.scope.tenant" },
  { scope: BRAND, label: "fees.scope.brand" },
  { scope: STORE, label: "fees.scope.store" },
];

/** How specific a scope is: a store's own rule beats its brand's, which beats every store's. */
function rank(scope: string): number {
  switch (scope) {
    case STORE:
      return 2;
    case BRAND:
      return 1;
    default:
      return 0;
  }
}

const KIND_LABEL: Record<FeeKind, MessageKey> = {
  FEE_KIND_PERCENT: "fees.kind.percent",
  FEE_KIND_AMOUNT_PER_BILL: "fees.kind.perBill",
  FEE_KIND_AMOUNT_PER_UNIT: "fees.kind.perUnit",
};

const ITEMS_LABEL: Record<FeeItems, MessageKey> = {
  FEE_ITEMS_ALL: "fees.items.all",
  FEE_ITEMS_INCLUDE: "fees.items.include",
  FEE_ITEMS_EXCLUDE: "fees.items.exclude",
};

const TAX_LABEL: Record<FeeTax, MessageKey> = {
  FEE_TAX_FOLLOW_LINES: "fees.tax.followLines",
  FEE_TAX_NOT_TAXABLE: "fees.tax.notTaxable",
  FEE_TAX_TAX_CLASS: "fees.tax.taxClass",
};

/** Why Save is not offered yet, in the operator's words. */
const GAP_LABEL: Record<DraftGap, MessageKey> = {
  code: "fees.gap.code",
  displayName: "fees.gap.displayName",
  rate: "fees.gap.rate",
  currency: "fees.gap.currency",
  amount: "fees.gap.amount",
  items: "fees.gap.items",
  taxClass: "fees.gap.taxClass",
};

const APPLIED = "FEE_PUBLISH_APPLIED";
const UNCHANGED = "FEE_PUBLISH_UNCHANGED";
const REFUSED = "FEE_PUBLISH_REFUSED";
const FAILED = "FEE_PUBLISH_FAILED";

/** How each outcome is drawn. `UNCHANGED` is neutral: the store already ran these fees. */
const OUTCOME: Readonly<
  Record<string, { readonly label: MessageKey; readonly tone: "active" | "neutral" | "danger" }>
> = {
  [APPLIED]: { label: "fees.outcome.applied", tone: "active" },
  [UNCHANGED]: { label: "fees.outcome.unchanged", tone: "neutral" },
  [REFUSED]: { label: "fees.outcome.refused", tone: "danger" },
  [FAILED]: { label: "fees.outcome.failed", tone: "danger" },
};

/** The stores that need something done first, so they are read first; an unknown outcome last. */
const OUTCOME_ORDER: readonly string[] = [REFUSED, FAILED, APPLIED, UNCHANGED];

function outcomeRank(outcome: string): number {
  const position = OUTCOME_ORDER.indexOf(outcome);
  return position === -1 ? OUTCOME_ORDER.length : position;
}

/** Why a store cannot apply a rule, in the operator's words; a reason from a newer cloud is shown as sent. */
const FAULT_LABEL: Readonly<Record<string, MessageKey>> = {
  CURRENCY_MISMATCH: "fees.fault.currency",
  TAX_RATE_NOT_CONFIGURED: "fees.fault.taxRate",
};

/** The most lines and units a sample bill takes — the cloud's own limits, checked before it does. */
const MOST_SAMPLE_LINES = 50;
const MOST_UNITS = 999;

/** How many listed names a summary spells out before it counts the rest. */
const NAMES_SHOWN = 3;

/** The rule the editor opened on, and whether saving it writes over a wider one here. */
interface Editing {
  /** The rule as written: at the chosen scope for an edit, at a wider one for an override. */
  readonly rule: FeeRule;
  readonly overriding: boolean;
}

/** One line of the sample bill, as the operator builds it. */
interface SampleRow {
  readonly menuItemId: string;
  readonly quantity: number | null;
}

/** What one write did at every store it reached, and where it was made. */
interface Report {
  /** The scope and id it was made at, from `place`: it is shown only while the picker names them. */
  readonly key: string;
  readonly stores: readonly FeePublishResult[];
}

/** What a refusal was a refusal of, which decides the words it is explained in. */
type Refused = "write" | "delete" | "preview";

/** Two ULIDs naming the same thing. They are case-insensitive, and a URL may carry either case. */
function sameId(left: string, right: string): boolean {
  return left.toUpperCase() === right.toUpperCase();
}

function byCode(left: FeeRule, right: FeeRule): number {
  return left.rule.code.localeCompare(right.rule.code);
}

/** A panel's value once its read has landed, else `null`. */
function readyValue<T>(panel: Panel<T> | null): T | null {
  return panel?.state === "ready" ? panel.value : null;
}

/** Why a panel's read was refused, else `""` — the shape a `<Show>` around a `<Banner>` wants. */
function failedMessage<T>(panel: Panel<T> | null): string {
  return panel?.state === "failed" ? panel.message : "";
}

/** An outcome in the operator's words; one from a newer cloud is shown as sent. */
function outcomeLabel(outcome: string): string {
  const drawn = OUTCOME[outcome];
  return drawn === undefined ? outcome : t(drawn.label);
}

/** The key for a refusal the cloud named a reason for, or `null` to show its own message. */
function refusalKey(caught: ApiError, refused: Refused): MessageKey | null {
  const has = (field: string | null, reason: string) =>
    caught.details.some(
      (detail) => detail.reason === reason && (field === null || detail.field === field),
    );
  if (refused === "delete" && caught.status === 422) {
    return "fees.refused.delete";
  }
  if (has("store_id", "NOT_PUBLISHED")) {
    return "fees.refused.notPublished";
  }
  if (has("lines", "TAX_RATE_NOT_CONFIGURED")) {
    return "fees.refused.lineTaxRate";
  }
  if (has("lines", "NOT_ON_MENU")) {
    return "fees.refused.lineNotOnMenu";
  }
  if (has("lines", "NOT_AVAILABLE")) {
    return "fees.refused.lineNotAvailable";
  }
  if (has(null, "CURRENCY_MISMATCH")) {
    return "fees.refused.currency";
  }
  if (has(null, "TAX_RATE_NOT_CONFIGURED")) {
    return "fees.refused.taxRate";
  }
  if (has(null, "ALREADY_EXISTS")) {
    return "fees.refused.code";
  }
  if (has(null, "NOT_ON_MENU")) {
    return "fees.refused.notOnMenu";
  }
  if (caught.status === 404) {
    return "fees.refused.notFound";
  }
  return null;
}

/**
 * A refusal in the operator's words, where the cloud named what was wrong; any other refusal passes
 * through as the server said it. The cloud's own sentences name rules and stores by id, which is
 * right for a log and wrong for a person, so a named reason is always re-said here.
 */
function explained(caught: unknown, refused: Refused): unknown {
  if (!(caught instanceof ApiError)) {
    return caught;
  }
  const key = refusalKey(caught, refused);
  return key === null
    ? caught
    : new ApiError(caught.status, t(key), caught.canonical, caught.details);
}

export function Fees() {
  // What the screen is drawn from, read together: the stores and brands a rule can be written for,
  // and the catalog a rule names — items, categories and tax classes — with the currencies an
  // amount can be in.
  const layout = createAdminResource(
    async (tenant) => {
      const [stores, brands, items, categories, taxClasses, countries] = await Promise.all([
        api.listStores(tenant),
        api.listBrands(tenant),
        api.listItems(tenant),
        api.listItemCategories(tenant),
        api.listTaxClasses(tenant),
        api.listCountries(),
      ]);
      return { stores, brands, items, categories, taxClasses, countries };
    },
    { scope: "tenant" },
  );
  // What the tenant has written. Its own read, because it is the one a write changes.
  const written = createAdminResource((tenant) => api.listFees(tenant), { scope: "tenant" });

  const [scope, setScope] = createSignal<FeeScope>(TENANT);
  // The brand or store the rules are for; unused for every store, whose id is the tenant's.
  const [target, setTarget] = createSignal("");
  const [report, setReport] = createSignal<Report | null>(null);
  const [effective, setEffective] = createSignal<Panel<EffectiveFees> | null>(null);
  const [deleting, setDeleting] = createSignal<FeeRule | null>(null);

  const [draft, setDraft] = createSignal<FeeDraft>(newDraft(""));
  const patch = (fields: Partial<FeeDraft>) => setDraft((current) => ({ ...current, ...fields }));
  const [previewStore, setPreviewStore] = createSignal("");
  const [previewChannel, setPreviewChannel] = createSignal<SalesChannel>("SALES_CHANNEL_DINE_IN");
  const [sampleRows, setSampleRows] = createSignal<readonly SampleRow[]>([]);
  const [storeConfig, setStoreConfig] = createSignal<Panel<Json | null> | null>(null);
  const [bill, setBill] = createSignal<Panel<SampleBill> | null>(null);

  // Two lifecycles. The editor's keeps a refusal in the drawer, beside what was typed; every other
  // write — a delete, a publish again — shows its refusal on the page. Each write publishes to the
  // stores it reaches, so none may start while another is in flight.
  const editor = useEntityCrud<Editing>();
  const writing = useEntityCrud<never>();
  const busy = () => editor.saving() || writing.saving();

  const stores = () => layout.value()?.stores ?? [];
  const brands = () => layout.value()?.brands ?? [];
  const rules = () => written.value() ?? [];
  // Memoize active items, categories, and tax classes using createMemo to avoid redundant O(N) array filtering on input strokes / re-renders.
  const activeItems = createMemo(() =>
    (layout.value()?.items ?? []).filter((row) => row.status === "active"),
  );
  const activeCategories = createMemo(() =>
    (layout.value()?.categories ?? []).filter((row) => row.status === "active"),
  );
  const activeTaxClasses = createMemo(() =>
    (layout.value()?.taxClasses ?? []).filter((row) => row.status === "active"),
  );

  const storeName = (id: string) => stores().find((row) => sameId(row.store_id, id))?.name ?? id;
  const brandName = (id: string) => brands().find((row) => sameId(row.brand_id, id))?.name ?? id;
  const itemName = (id: string) =>
    (layout.value()?.items ?? []).find((row) => sameId(row.menu_item_id, id))?.name ?? id;
  const categoryName = (id: string) =>
    (layout.value()?.categories ?? []).find((row) => sameId(row.item_category_id, id))?.name ?? id;
  const taxClassName = (id: string) =>
    (layout.value()?.taxClasses ?? []).find((row) => sameId(row.tax_class_id, id))?.name ?? id;
  const codeOf = (feeId: string) =>
    rules().find((rule) => sameId(rule.rule.fee_id, feeId))?.rule.code ?? t("fees.unknownFee");

  /** The id the rules are written for: the tenant itself for every store. */
  const scopeId = () => (scope() === TENANT ? tenantId() : target());
  /** Whether the picker names a place: every store always does, a brand or a store once chosen. */
  const chosen = () => scope() === TENANT || target() !== "";
  /** The place the picker names, as a report is keyed. */
  const place = () => `${scope()}:${scopeId().toUpperCase()}`;

  // A store chosen in the top bar is the store this screen opens on, so an operator who came to see
  // one shop sees what it runs; without one, it opens on every store. The context changing — a
  // shared link, the top bar — starts over from it.
  createEffect(
    on([tenantId, storeId], ([, store]) => {
      setScope(store ? STORE : TENANT);
      setTarget(store);
      setReport(null);
      editor.close();
    }),
  );

  const chooseScope = (next: string) => {
    const picked = SCOPES.find((entry) => entry.scope === next)?.scope ?? TENANT;
    setScope(picked);
    setTarget(picked === STORE ? storeId() : "");
    setReport(null);
  };

  const chooseTarget = (next: string) => {
    setTarget(next);
    setReport(null);
  };

  const brandOf = (store: string) =>
    stores().find((row) => sameId(row.store_id, store))?.brand_id ?? null;

  /** The stores a rule written here reaches — the same rule the cloud applies to a write. */
  const reached = createMemo(() => {
    const id = target();
    switch (scope()) {
      case TENANT:
        return stores();
      case BRAND:
        return id ? stores().filter((row) => row.brand_id !== null && sameId(row.brand_id, id)) : [];
      default:
        return id ? stores().filter((row) => sameId(row.store_id, id)) : [];
    }
  });

  /** Whether `rule` reaches `store`: written for every store, for the store's brand, or for it. */
  const reachesStore = (rule: FeeRule, store: string) => {
    switch (rule.scope) {
      case TENANT:
        return true;
      case BRAND: {
        const brand = brandOf(store);
        return brand !== null && sameId(brand, rule.scope_id);
      }
      case STORE:
        return sameId(rule.scope_id, store);
      default:
        return false;
    }
  };

  /** The rules written at the chosen place, by code. */
  const rulesHere = createMemo(() =>
    chosen()
      ? rules()
          .filter((rule) => rule.scope === scope() && sameId(rule.scope_id, scopeId()))
          .sort(byCode)
      : [],
  );

  /** The rules written at a wider scope that reach the chosen place. */
  const widerRules = createMemo(() => {
    const here = scope();
    const id = target();
    if (here === TENANT || !id) {
      return [];
    }
    return rules().filter(
      (rule) =>
        rank(rule.scope) < rank(here) && (here === BRAND ? rule.scope === TENANT : reachesStore(rule, id)),
    );
  });

  /** The wider rule a fee would run here without a rule of its own: the most specific of them. */
  const widerFor = (feeId: string): FeeRule | undefined =>
    widerRules()
      .filter((rule) => sameId(rule.rule.fee_id, feeId))
      .sort((left, right) => rank(right.scope) - rank(left.scope))[0];

  /** The wider rules nothing here overrides, one per fee: what "Override here" is offered on. */
  const inherited = createMemo(() => {
    const here = new Set(rulesHere().map((rule) => rule.rule.fee_id.toUpperCase()));
    const best = new Map<string, FeeRule>();
    for (const rule of widerRules()) {
      const key = rule.rule.fee_id.toUpperCase();
      const held = best.get(key);
      if (!here.has(key) && (held === undefined || rank(rule.scope) > rank(held.scope))) {
        best.set(key, rule);
      }
    }
    return [...best.values()].sort(byCode);
  });

  /** Where a rule was written, as a list labels it. */
  const fromText = (ruleScope: string, id: string) => {
    switch (ruleScope) {
      case TENANT:
        return t("fees.from.tenant");
      case BRAND:
        return t("fees.from.brand", { name: brandName(id) });
      case STORE:
        return t("fees.from.store", { name: storeName(id) });
      default:
        return ruleScope;
    }
  };

  /** Where a rule was written, as a sentence says it. */
  const phraseOf = (ruleScope: string, id: string) => {
    switch (ruleScope) {
      case TENANT:
        return t("fees.phrase.tenant");
      case BRAND:
        return t("fees.phrase.brand", { name: brandName(id) });
      case STORE:
        return t("fees.phrase.store", { name: storeName(id) });
      default:
        return ruleScope;
    }
  };

  /** The place the picker names, in words: every store, or the brand or store by name. */
  const targetName = () => {
    switch (scope()) {
      case TENANT:
        return t("fees.target.tenant");
      case BRAND:
        return brandName(target());
      default:
        return storeName(target());
    }
  };

  // What one store runs. Read for the store the picker names, which need not be the one in the top
  // bar. Only the newest read may land: choosing two stores quickly must not leave the first one's
  // fees under the second one's name.
  let effectiveRead = 0;
  const loadEffective = () => {
    effectiveRead += 1;
    const mine = effectiveRead;
    const tenant = tenantId();
    const store = scope() === STORE ? target() : "";
    if (!tenant || !store) {
      setEffective(null);
      return;
    }
    setEffective(LOADING);
    void panelOf(api.effectiveFees(tenant, store), (panel) => {
      if (mine === effectiveRead) {
        setEffective(panel);
      }
    });
  };
  createEffect(on([tenantId, scope, target], () => loadEffective()));

  // --- the sample bill ----------------------------------------------------------------------------

  /** The store a sample bill is assembled at unless another is chosen: the one in context, if reached. */
  const defaultPreviewStore = () => {
    const candidates = reached();
    const preferred =
      candidates.find((row) => sameId(row.store_id, storeId())) ??
      candidates.find((row) => row.status === "active") ??
      candidates[0];
    return preferred?.store_id ?? "";
  };
  // Kept among the stores the rule reaches, and read before the editor opens, so the store's menu
  // and currency are usually there by the time the operator wants them.
  createEffect(
    on(reached, (candidates) => {
      if (!candidates.some((row) => sameId(row.store_id, previewStore()))) {
        setPreviewStore(defaultPreviewStore());
      }
    }),
  );

  // The sample bill's store, as it is published: the menu its lines come from and its currency.
  let configRead = 0;
  createEffect(
    on([tenantId, previewStore], ([tenant, store]) => {
      configRead += 1;
      const mine = configRead;
      if (!tenant || !store) {
        setStoreConfig(null);
        return;
      }
      setStoreConfig(LOADING);
      void panelOf(api.effectiveConfig(tenant, store), (panel) => {
        if (mine === configRead) {
          setStoreConfig(panel);
        }
      });
    }),
  );

  const previewConfig = () => readyValue(storeConfig());
  /** What the sample bill's store sells on its channel, which is all a sample line can be. */
  const offers = createMemo(() => menuOffers(previewConfig(), previewChannel()));
  const previewCurrency = () => storeCurrency(previewConfig());

  // A different store or channel is a different menu: the lines it still sells are kept, and a bill
  // left with none starts from the first thing on the menu, so there is always something to try.
  createEffect(
    on(offers, (list) => {
      setSampleRows((rows) => {
        const kept = rows.filter((row) =>
          list.some((offer) => sameId(offer.menu_item_id, row.menuItemId)),
        );
        if (kept.length > 0) {
          return kept;
        }
        const first = list[0];
        return first === undefined ? [] : [{ menuItemId: first.menu_item_id, quantity: 1 }];
      });
    }),
  );

  // An amount is in the store's currency unless the operator says otherwise; a draft opened before
  // the store's locale was read takes it as soon as it is.
  createEffect(() => {
    const currency = previewCurrency();
    if (currency !== null && editor.mode() !== "idle" && draft().currency === "") {
      patch({ currency });
    }
  });

  // A bill shown for a rule that has since changed would be a bill for a rule nobody is saving, so
  // any change takes it away, and an answer still in flight for the old one is dropped.
  let previewRead = 0;
  createEffect(
    on(
      [draft, previewStore, previewChannel, sampleRows],
      () => {
        previewRead += 1;
        setBill(null);
      },
      { defer: true },
    ),
  );

  /** The lines a preview sends: an item chosen and a whole number of units the cloud takes. */
  const previewLines = () =>
    sampleRows().flatMap((row) =>
      row.menuItemId !== "" &&
      row.quantity !== null &&
      Number.isInteger(row.quantity) &&
      row.quantity >= 1 &&
      row.quantity <= MOST_UNITS
        ? [{ menu_item_id: row.menuItemId, quantity: row.quantity }]
        : [],
    );

  /**
   * When the bill would show a rule other than this one: the store has its own rule for the fee
   * (or, for a rule written for every store, its brand has one), and the most specific rule wins.
   */
  const shadowedAtPreview = () => {
    const feeId = draft().feeId;
    const store = previewStore();
    if (feeId === null || !store) {
      return false;
    }
    return rules().some(
      (rule) =>
        sameId(rule.rule.fee_id, feeId) &&
        rank(rule.scope) > rank(scope()) &&
        reachesStore(rule, store),
    );
  };

  const previewBill = async () => {
    const current = draft();
    const store = previewStore();
    const lines = previewLines();
    if (draftGap(current) !== null || !store || lines.length === 0) {
      return;
    }
    previewRead += 1;
    const mine = previewRead;
    setBill(LOADING);
    const asked = api
      .previewFees(
        tenantId(),
        store,
        previewChannel(),
        [{ scope: scope(), scope_id: scopeId(), rule: ruleFromDraft(current) }],
        lines,
      )
      .catch((caught: unknown) => {
        throw explained(caught, "preview");
      });
    await panelOf(asked, (panel) => {
      if (mine === previewRead) {
        setBill(panel);
      }
    });
  };

  const setSampleItem = (index: number, menuItemId: string) =>
    setSampleRows((rows) =>
      rows.map((row, position) => (position === index ? { ...row, menuItemId } : row)),
    );
  const setSampleQuantity = (index: number, quantity: number | null) =>
    setSampleRows((rows) =>
      rows.map((row, position) => (position === index ? { ...row, quantity } : row)),
    );
  const removeSampleLine = (index: number) =>
    setSampleRows((rows) => rows.filter((_, position) => position !== index));
  /** Adds the first thing on the menu the bill does not have yet, or the first thing again. */
  const addSampleLine = () => {
    const list = offers();
    const unused =
      list.find((offer) => !sampleRows().some((row) => sameId(row.menuItemId, offer.menu_item_id))) ??
      list[0];
    if (unused !== undefined && sampleRows().length < MOST_SAMPLE_LINES) {
      setSampleRows((rows) => [...rows, { menuItemId: unused.menu_item_id, quantity: 1 }]);
    }
  };

  // --- the editor ---------------------------------------------------------------------------------

  const setCode = (code: string) => patch({ code });
  const setDisplayName = (displayName: string) => patch({ displayName });
  const setTranslation = (locale: string, name: string) =>
    patch({ translations: { ...draft().translations, [locale]: name } });
  const setKind = (kind: FeeKind) => patch({ kind });
  const setRate = (percent: string) => patch({ percent });
  const setAmount = (amountMinor: number | null) => patch({ amountMinor });
  const setCurrency = (currency: string) => patch({ currency });
  const toggleChannel = (channel: SalesChannel, ticked: boolean) => {
    const current = draft().channels;
    const known = SALES_CHANNELS.filter((each) =>
      each === channel ? ticked : current.includes(each),
    );
    // A channel from a newer cloud this console cannot show is kept as it was written.
    const unknown = current.filter((each) => !SALES_CHANNELS.includes(each));
    patch({ channels: [...known, ...unknown] });
  };

  /** The languages the name is offered in: the console's own, and any the rule already has. */
  const translationLocales = () => {
    const codes = new Set<string>(LOCALES);
    for (const code of Object.keys(draft().translations)) {
      codes.add(code);
    }
    return [...codes];
  };
  const languageOf = (code: string) =>
    LOCALES.some((known) => known === code) ? localeName(code as Locale) : code;

  /** The currencies an amount can be written in: the platform's, and the draft's own. */
  const currencyOptions = createMemo(() => {
    const codes = new Set((layout.value()?.countries ?? []).map((country) => country.currency_code));
    const own = draft().currency;
    if (own !== "") {
      codes.add(own);
    }
    return [...codes].sort((left, right) => left.localeCompare(right));
  });

  const openEditor = (next: FeeDraft) => {
    setDraft(next);
    setBill(null);
    if (!reached().some((row) => sameId(row.store_id, previewStore()))) {
      setPreviewStore(defaultPreviewStore());
    }
    // A rule for some channels only is tried on one of them; a bill on another would show no fee.
    const charged = SALES_CHANNELS.find((channel) => next.channels.includes(channel));
    if (charged !== undefined && !next.channels.includes(previewChannel())) {
      setPreviewChannel(charged);
    }
  };
  const openCreate = () => {
    editor.create();
    openEditor(newDraft(previewCurrency() ?? ""));
  };
  const openEdit = (rule: FeeRule) => {
    editor.edit({ rule, overriding: false });
    openEditor(draftFromRule(rule.rule, previewCurrency() ?? ""));
  };
  const openOverride = (rule: FeeRule) => {
    editor.edit({ rule, overriding: true });
    openEditor(draftFromRule(rule.rule, previewCurrency() ?? ""));
  };

  const editorTitle = () => {
    const subject = editor.subject();
    if (subject === null) {
      return t("fees.createTitle", { target: targetName() });
    }
    return subject.overriding
      ? t("fees.overrideTitle", { code: subject.rule.rule.code, target: targetName() })
      : t("fees.editTitle", { code: subject.rule.rule.code });
  };

  // --- writing ------------------------------------------------------------------------------------

  /**
   * Every write but the editor's: run `call`, show what it did at every store under the place it was
   * made, and re-read what is written. Re-read on a refusal too — a rule somebody else removed is a
   * `404` that has also changed what this screen should show.
   */
  const write = (
    call: () => Promise<{ readonly stores: readonly FeePublishResult[] }>,
    done: () => void,
    refused: Refused = "write",
  ) => {
    const key = place();
    void writing
      .run(async () => {
        try {
          const result = await call();
          setReport({ key, stores: result.stores });
          done();
        } catch (caught) {
          throw explained(caught, refused);
        }
      })
      .then((ok) => {
        if (!ok) {
          toast.error(writing.error());
        }
        void written.refetch();
        loadEffective();
      });
  };

  const saveFee = () => {
    const current = draft();
    if (draftGap(current) !== null || !chosen()) {
      return;
    }
    const at = scope();
    const id = scopeId();
    const key = place();
    const rule = ruleFromDraft(current);
    const creating = editor.mode() === "creating";
    void editor
      .run(async () => {
        try {
          const result = creating
            ? await api.createFee(tenantId(), at, id, rule)
            : await api.putFee(tenantId(), at, id, rule);
          setReport({ key, stores: result.stores });
          toast.ok(t("fees.saved", { code: rule.code }));
        } catch (caught) {
          throw explained(caught, "write");
        }
      })
      .then(() => {
        void written.refetch();
        loadEffective();
      });
  };

  const confirmDelete = () => {
    const rule = deleting();
    setDeleting(null);
    const at = SCOPES.find((entry) => entry.scope === rule?.scope)?.scope;
    if (rule === null || at === undefined) {
      return;
    }
    write(
      () => api.deleteFee(tenantId(), at, rule.scope_id, rule.rule.fee_id),
      () => toast.ok(t("fees.deleted", { code: rule.rule.code })),
      "delete",
    );
  };

  /** Publishes the fees of every store the chosen place reaches again, and reports each one. */
  const republish = () => {
    const at = scope();
    const id = target();
    const targets = reached().map((row) => row.store_id);
    write(
      async () => {
        if (at === TENANT) {
          return api.publishFees(tenantId());
        }
        const answers: FeePublishResult[] = [];
        for (const store of at === STORE ? [id] : targets) {
          answers.push(...(await api.publishFees(tenantId(), store)).stores);
        }
        return { stores: answers };
      },
      () => toast.ok(t("fees.republished")),
    );
  };

  /** Publishes again to each store the last write did not reach, one at a time, and folds the answers in. */
  const publishAgain = () => {
    const shown = report();
    if (shown === null) {
      return;
    }
    const again = shown.stores
      .filter((row) => row.outcome === REFUSED || row.outcome === FAILED)
      .map((row) => row.store_id);
    if (again.length === 0) {
      return;
    }
    write(
      async () => {
        const answers: FeePublishResult[] = [];
        for (const store of again) {
          answers.push(...(await api.publishFees(tenantId(), store)).stores);
        }
        return {
          stores: shown.stores.map(
            (row) => answers.find((answer) => sameId(answer.store_id, row.store_id)) ?? row,
          ),
        };
      },
      () => toast.ok(t("fees.republished")),
    );
  };

  // --- what a rule says, in words -----------------------------------------------------------------

  const chargeText = (rule: FeeRuleFields): string => {
    switch (rule.kind) {
      case "FEE_KIND_PERCENT":
        return rule.rate === undefined
          ? t("fees.charge.unknown")
          : t("fees.charge.percent", { rate: percentText(rule.rate) });
      case "FEE_KIND_AMOUNT_PER_BILL":
        return rule.amount === undefined
          ? t("fees.charge.unknown")
          : t("fees.charge.perBill", { amount: formatAmount(rule.amount) });
      case "FEE_KIND_AMOUNT_PER_UNIT":
        return rule.amount === undefined
          ? t("fees.charge.unknown")
          : t("fees.charge.perUnit", { amount: formatAmount(rule.amount) });
      default:
        return t("fees.charge.unknown");
    }
  };

  /** What a percentage is taken of; nothing for an amount, which has no base. */
  const baseText = (rule: FeeRuleFields): string =>
    rule.kind === "FEE_KIND_PERCENT"
      ? t("fees.baseSummary", {
          discounts: t(rule.base_discounted === false ? "fees.base.before" : "fees.base.after"),
          tax: t(rule.base_tax_inclusive === true ? "fees.base.inclusive" : "fees.base.net"),
        })
      : "";

  /** The first few names a list carries, and how many more. */
  const namesText = (names: readonly string[]): string => {
    const shown = names.slice(0, NAMES_SHOWN).join(", ");
    const more = names.length - NAMES_SHOWN;
    return more > 0 ? t("fees.andMore", { names: shown, count: more }) : shown;
  };

  const linesText = (rule: FeeRuleFields): string => {
    const names = [
      ...(rule.item_category_ids ?? []).map(categoryName),
      ...(rule.menu_item_ids ?? []).map(itemName),
    ];
    switch (rule.item_scope) {
      case "FEE_ITEMS_INCLUDE":
        return t("fees.linesSummary.include", { names: namesText(names) });
      case "FEE_ITEMS_EXCLUDE":
        return t("fees.linesSummary.exclude", { names: namesText(names) });
      default:
        return t("fees.linesSummary.all");
    }
  };

  const channelsText = (rule: FeeRuleFields): string => {
    const channels = rule.channels ?? [];
    if (channels.length === 0) {
      return t("fees.channels.every");
    }
    return channels
      .map((channel) => {
        const key = CHANNEL_LABEL[channel as SalesChannel] as MessageKey | undefined;
        return key === undefined ? channel : t(key);
      })
      .join(", ");
  };

  const taxText = (rule: FeeRuleFields): string => {
    switch (rule.tax) {
      case "FEE_TAX_NOT_TAXABLE":
        return t("fees.taxSummary.notTaxable");
      case "FEE_TAX_TAX_CLASS":
        return t("fees.taxSummary.taxClass", { taxClass: taxClassName(rule.tax_class_id ?? "") });
      default:
        return t("fees.taxSummary.followLines");
    }
  };

  const faultText = (reason: string) => {
    const key = FAULT_LABEL[reason];
    return key === undefined ? reason : t(key);
  };

  /** The columns every list of rules shares. */
  const ruleColumns = (extra: readonly Column<FeeRule>[] = []): Column<FeeRule>[] => [
    {
      key: "code",
      header: t("fees.column.code"),
      sortValue: (row) => row.rule.code,
      cell: (row) => <span class="font-mono text-sm">{row.rule.code}</span>,
    },
    {
      key: "name",
      header: t("fees.column.name"),
      sortValue: (row) => row.rule.display_name,
      cell: (row) => row.rule.display_name,
    },
    {
      key: "charge",
      header: t("fees.column.charge"),
      cell: (row) => (
        <div class="flex flex-col">
          <span>{chargeText(row.rule)}</span>
          <Show when={baseText(row.rule)}>
            {(base) => <span class="text-xs text-ink-muted">{base()}</span>}
          </Show>
        </div>
      ),
    },
    {
      key: "appliesTo",
      header: t("fees.column.appliesTo"),
      cell: (row) => (
        <div class="flex flex-col">
          <span>{channelsText(row.rule)}</span>
          <span class="text-xs text-ink-muted">{linesText(row.rule)}</span>
        </div>
      ),
    },
    {
      key: "tax",
      header: t("fees.column.tax"),
      cell: (row) => taxText(row.rule),
    },
    ...extra,
  ];

  /** A rule's standing: charged or paused, waivable or not, and what it overrides here. */
  const statusColumn = (): Column<FeeRule> => ({
    key: "status",
    header: t("fees.column.status"),
    cell: (row) => (
      <div class="flex flex-col items-start gap-1">
        <StatusBadge
          tone={row.rule.active === false ? "archived" : "active"}
          label={row.rule.active === false ? t("fees.status.paused") : t("fees.status.charged")}
        />
        <Show when={row.rule.waivable === true}>
          <span class="text-xs text-ink-muted">{t("fees.status.waivable")}</span>
        </Show>
        <Show when={widerFor(row.rule.fee_id)}>
          {(wider) => (
            <span class="text-xs text-ink-muted">
              {t("fees.status.overrides", { from: phraseOf(wider().scope, wider().scope_id) })}
            </span>
          )}
        </Show>
      </div>
    ),
  });

  const fromColumn = (): Column<FeeRule> => ({
    key: "from",
    header: t("fees.column.from"),
    cell: (row) => fromText(row.scope, row.scope_id),
  });

  // --- the views ----------------------------------------------------------------------------------

  /** What the last write did at every store it reached, while the picker names where it was made. */
  const ReportCard = () => (
    <Show when={report()?.key === place() ? report() : null}>
      {(shown) => {
        const count = (outcome: string) =>
          shown().stores.filter((row) => row.outcome === outcome).length;
        const ordered = () =>
          [...shown().stores].sort(
            (left, right) => outcomeRank(left.outcome) - outcomeRank(right.outcome),
          );
        const summary = () =>
          t("fees.reportSummary", {
            applied: count(APPLIED),
            unchanged: count(UNCHANGED),
            refused: count(REFUSED),
            failed: count(FAILED),
          });
        return (
          <Card title={t("fees.report")}>
            <div class="flex flex-col gap-3">
              <Show
                when={shown().stores.length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("fees.reportEmpty")}</p>}
              >
                {/* The outcome a replay waits for (`scripts/step-tasks.mjs`): present only once a
                    store has taken the change, so it is the publish landing and not the card. */}
                <Show
                  when={count(APPLIED) > 0}
                  fallback={<p class="text-sm text-ink">{summary()}</p>}
                >
                  <p data-outcome="fee-published" class="text-sm text-ink">
                    {summary()}
                  </p>
                </Show>
                <ul class="flex flex-col gap-1">
                  <For each={ordered()}>
                    {(row) => (
                      <li class="flex flex-wrap items-center gap-2 rounded-token border border-line bg-surface-raised px-3 py-2">
                        <StatusBadge
                          tone={OUTCOME[row.outcome]?.tone ?? "neutral"}
                          label={outcomeLabel(row.outcome)}
                        />
                        <span class="text-sm text-ink">{storeName(row.store_id)}</span>
                        <Show when={(row.faults ?? []).length > 0}>
                          <ul class="w-full text-sm text-danger">
                            <For each={row.faults ?? []}>
                              {(fault) => (
                                <li>
                                  {t("fees.faultLine", {
                                    code: codeOf(fault.fee_id),
                                    reason: faultText(fault.reason),
                                  })}
                                </li>
                              )}
                            </For>
                          </ul>
                        </Show>
                        <Show when={row.config_version_id}>
                          {(version) => (
                            <TechnicalDetails label={t("common.technicalDetails")}>
                              {version()}
                            </TechnicalDetails>
                          )}
                        </Show>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
              <Show when={count(REFUSED) + count(FAILED) > 0}>
                <div class="flex flex-col gap-1">
                  <div>
                    <Button variant="secondary" disabled={busy()} onClick={publishAgain}>
                      {t("fees.publishAgain")}
                    </Button>
                  </div>
                  <p class="text-sm text-ink-muted">{t("fees.publishAgainHint")}</p>
                </div>
              </Show>
            </div>
          </Card>
        );
      }}
    </Show>
  );

  /** One sample bill, as the store would assemble it. */
  const BillView = (props: { bill: SampleBill }) => {
    const row = (label: string, amount: string, strong = false) => (
      <div class={`flex justify-between gap-4 ${strong ? "font-semibold text-ink" : "text-ink"}`}>
        <span>{label}</span>
        <span class="tabular-nums">{amount}</span>
      </div>
    );
    // In a store whose prices include tax, the tax is part of what the guest already pays rather
    // than added to it, and the bill says which: the total is the lines and the fees, or those and
    // the tax as well.
    const taxIncluded = () => {
      const bill = props.bill;
      const added =
        bill.total_due.amount_minor -
        bill.subtotal.amount_minor -
        bill.service_charge.amount_minor -
        bill.rounding_adjustment.amount_minor;
      return added === 0 && bill.tax_total.amount_minor !== 0;
    };
    return (
      <div
        data-preview="sample-bill"
        class="flex flex-col gap-2 rounded-token border border-line bg-surface-raised p-3 text-sm"
      >
        <For each={props.bill.lines}>
          {(line) =>
            row(
              t("fees.bill.line", { name: line.display_name, quantity: line.quantity }),
              formatAmount(line.line_total),
            )
          }
        </For>
        <div class="border-t border-line pt-2">
          {row(t("fees.bill.subtotal"), formatAmount(props.bill.subtotal))}
        </div>
        <Show
          when={props.bill.fee_lines.length > 0}
          fallback={<p class="text-ink-muted">{t("fees.bill.noFee")}</p>}
        >
          <For each={props.bill.fee_lines}>
            {(fee) => (
              <div class="flex flex-col gap-0.5">
                {row(fee.display_name, formatAmount(fee.amount))}
                <span class="text-xs text-ink-muted">
                  {t("fees.bill.feeTax", { tax: formatAmount(fee.tax) })}
                </span>
                <Show when={fee.class_shares.length > 1}>
                  <For each={fee.class_shares}>
                    {(share) => (
                      <span class="text-xs text-ink-muted">
                        {t("fees.bill.share", {
                          taxClass: taxClassName(share.tax_class_id),
                          amount: formatAmount(share.amount),
                          tax: formatAmount(share.tax),
                        })}
                      </span>
                    )}
                  </For>
                </Show>
              </div>
            )}
          </For>
        </Show>
        <div class="flex flex-col gap-1 border-t border-line pt-2">
          <For each={props.bill.tax_lines}>
            {(line) => (
              <div class="flex flex-col gap-0.5">
                {row(
                  t("fees.bill.taxLine", {
                    taxClass: taxClassName(line.tax_class_id),
                    rate: percentText({ numerator: line.rate_basis_points, denominator: 10_000 }),
                  }),
                  formatAmount(line.tax),
                )}
                <span class="text-xs text-ink-muted">
                  {t("fees.bill.taxBase", { base: formatAmount(line.taxable_base) })}
                </span>
              </div>
            )}
          </For>
          {row(
            taxIncluded() ? t("fees.bill.taxIncluded") : t("fees.bill.taxTotal"),
            formatAmount(props.bill.tax_total),
          )}
          <Show when={props.bill.rounding_adjustment.amount_minor !== 0}>
            {row(t("fees.bill.rounding"), formatAmount(props.bill.rounding_adjustment))}
          </Show>
        </div>
        <div class="border-t border-line pt-2">
          {row(t("fees.bill.total"), formatAmount(props.bill.total_due), true)}
        </div>
      </div>
    );
  };

  /** The editor's sample bill: where, on which channel, with which lines — and the bill itself. */
  const PreviewPanel = () => (
    <FormSection title={t("fees.previewTitle")} hint={t("fees.previewHint")}>
      <Show
        when={reached().length > 0}
        fallback={<p class="text-sm text-ink-muted">{t("fees.previewNoStore")}</p>}
      >
        <Show
          when={reached().length > 1}
          fallback={
            <p class="text-sm text-ink">{t("fees.previewAt", { store: storeName(previewStore()) })}</p>
          }
        >
          <SelectField
            label={t("fees.previewStore")}
            value={previewStore()}
            options={reached().map((row) => ({ value: row.store_id, label: row.name }))}
            onChange={setPreviewStore}
          />
        </Show>
        <SelectField
          label={t("fees.previewChannel")}
          value={previewChannel()}
          options={SALES_CHANNELS.map((channel) => ({
            value: channel,
            label: t(CHANNEL_LABEL[channel]),
          }))}
          onChange={(value) =>
            setPreviewChannel(SALES_CHANNELS.find((channel) => channel === value) ?? "SALES_CHANNEL_DINE_IN")
          }
        />
        <Show when={shadowedAtPreview()}>
          <p class="text-sm text-ink-muted">
            {t("fees.previewShadowed", { store: storeName(previewStore()) })}
          </p>
        </Show>
        <Show when={storeConfig()?.state === "loading"}>
          <Skeleton label={t("common.loading")} rows={2} />
        </Show>
        <Show when={failedMessage(storeConfig())}>
          {(reason) => (
            <Banner
              tone="danger"
              message={t("fees.previewUnread", { store: storeName(previewStore()), reason: reason() })}
            />
          )}
        </Show>
        <Show when={storeConfig()?.state === "ready"}>
          <Show
            when={offers().length > 0}
            fallback={
              <p class="text-sm text-ink-muted">
                {t("fees.previewNoMenu", { store: storeName(previewStore()) })}
              </p>
            }
          >
            <div class="flex flex-col gap-3">
              <Index each={sampleRows()}>
                {(row, index) => (
                  <div class="flex flex-wrap items-end gap-2">
                    <div class="min-w-48 flex-1">
                      <ComboboxField
                        label={t("fees.sampleItem", { position: index + 1 })}
                        value={row().menuItemId}
                        options={offers().map((offer) => ({
                          value: offer.menu_item_id,
                          label:
                            offer.unit_price === null
                              ? offer.display_name
                              : t("fees.sampleOffer", {
                                  name: offer.display_name,
                                  price: formatAmount(offer.unit_price),
                                }),
                        }))}
                        onChange={(value) => setSampleItem(index, value)}
                        placeholder={t("fees.pickSampleItem")}
                        searchLabel={t("fees.searchSampleItems")}
                        emptyLabel={t("picker.noMatch")}
                      />
                    </div>
                    <div class="w-24">
                      <NumberField
                        label={t("fees.sampleQuantity")}
                        value={row().quantity}
                        min={1}
                        max={MOST_UNITS}
                        step={1}
                        onChange={(value) => setSampleQuantity(index, value)}
                      />
                    </div>
                    <Button variant="ghost" size="sm" onClick={() => removeSampleLine(index)}>
                      {t("fees.removeSampleLine")}
                    </Button>
                  </div>
                )}
              </Index>
              <div class="flex flex-wrap gap-2">
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={sampleRows().length >= MOST_SAMPLE_LINES}
                  onClick={addSampleLine}
                >
                  {t("fees.addSampleLine")}
                </Button>
                <Button
                  data-step="previewBill"
                  variant="secondary"
                  size="sm"
                  disabled={
                    busy() ||
                    bill()?.state === "loading" ||
                    previewLines().length === 0 ||
                    draftGap(draft()) !== null
                  }
                  onClick={() => void previewBill()}
                >
                  {t("fees.previewBill")}
                </Button>
              </div>
            </div>
          </Show>
        </Show>
        <Show when={bill()?.state === "loading"}>
          <Skeleton label={t("common.loading")} rows={3} />
        </Show>
        <Show when={failedMessage(bill())}>
          {(message) => <Banner tone="danger" message={message()} />}
        </Show>
        <Show when={readyValue(bill())}>{(shown) => <BillView bill={shown()} />}</Show>
      </Show>
    </FormSection>
  );

  /** The columns of what one store runs: each fee, where its rule comes from, and its standing. */
  const effectiveColumns = (): Column<EffectiveFee>[] => [
    {
      key: "code",
      header: t("fees.column.code"),
      sortValue: (row) => row.rule.code,
      cell: (row) => <span class="font-mono text-sm">{row.rule.code}</span>,
    },
    {
      key: "name",
      header: t("fees.column.name"),
      cell: (row) => row.rule.display_name,
    },
    {
      key: "charge",
      header: t("fees.column.charge"),
      cell: (row) => chargeText(row.rule),
    },
    {
      key: "from",
      header: t("fees.column.from"),
      cell: (row) => fromText(row.scope, row.scope_id),
    },
    {
      key: "standing",
      header: t("fees.column.status"),
      cell: (row) => (
        <Show
          when={(row.faults ?? []).length > 0}
          fallback={
            <StatusBadge
              tone={row.rule.active === false ? "archived" : "active"}
              label={row.rule.active === false ? t("fees.status.paused") : t("fees.status.charged")}
            />
          }
        >
          <div class="flex flex-col items-start gap-1">
            <For each={row.faults ?? []}>
              {(fault) => <StatusBadge tone="danger" label={faultText(fault)} />}
            </For>
          </div>
        </Show>
      ),
    },
  ];

  /** What the chosen store runs, and anything that stops it applying a rule. */
  const EffectiveCard = () => (
    <Card title={t("fees.runsTitle", { store: storeName(target()) })}>
      <div class="flex flex-col gap-3">
        <p class="text-sm text-ink-muted">{t("fees.runsHint")}</p>
        <Show when={effective()?.state === "loading"}>
          <Skeleton label={t("common.loading")} rows={2} />
        </Show>
        <Show when={failedMessage(effective())}>
          {(reason) => (
            <Banner
              tone="danger"
              message={t("fees.runsUnread", { store: storeName(target()), reason: reason() })}
            />
          )}
        </Show>
        <Show when={readyValue(effective())}>
          {(runs) => (
            <>
              <Show when={!runs().publishable}>
                <Banner
                  tone="danger"
                  message={t("fees.notPublishable", { store: storeName(target()) })}
                />
              </Show>
              <DataTable
                columns={effectiveColumns()}
                rows={runs().fees}
                pageSize={CLIENT_PAGE_SIZE}
                empty={<EmptyState title={t("fees.runsEmpty", { store: storeName(target()) })} />}
              />
            </>
          )}
        </Show>
      </div>
    </Card>
  );

  const loaded = () => layout.value() !== null && written.value() !== null;
  const unreadable = () => failureOf(layout) || failureOf(written);

  return (
    <div>
      <PageHeader
        title={t("fees.title")}
        description={t("fees.description")}
        actions={
          <Button
            data-step="openCreate"
            disabled={!loaded() || !chosen() || busy()}
            onClick={openCreate}
          >
            {t("fees.new")}
          </Button>
        }
      />
      <RequireContext need="tenant">
        <Show when={unreadable()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show
          when={loaded()}
          fallback={
            <Show when={!unreadable()}>
              <Skeleton label={t("common.loading")} rows={4} />
            </Show>
          }
        >
          <div class="flex flex-col gap-4">
            <Card title={t("fees.scopeTitle")}>
              <div class="flex max-w-xl flex-col gap-4">
                <SelectField
                  label={t("fees.scopeKind")}
                  value={scope()}
                  options={SCOPES.map((entry) => ({ value: entry.scope, label: t(entry.label) }))}
                  onChange={chooseScope}
                  hint={t("fees.scopeHint")}
                  disabled={busy()}
                />
                <Show when={scope() === BRAND}>
                  <ComboboxField
                    label={t("fees.brand")}
                    value={target()}
                    options={brands()
                      .filter((row) => row.status === "active" || sameId(row.brand_id, target()))
                      .map((row) => ({ value: row.brand_id, label: row.name }))}
                    onChange={chooseTarget}
                    placeholder={t("fees.pickBrand")}
                    disabled={busy()}
                    searchLabel={t("fees.searchBrands")}
                    emptyLabel={t("picker.noMatch")}
                  />
                </Show>
                <Show when={scope() === STORE}>
                  <ComboboxField
                    label={t("fees.store")}
                    value={target()}
                    options={stores()
                      .filter((row) => row.status === "active" || sameId(row.store_id, target()))
                      .map((row) => ({
                        value: row.store_id,
                        label: row.name,
                        keywords: [row.store_id],
                      }))}
                    onChange={chooseTarget}
                    placeholder={t("fees.pickStore")}
                    disabled={busy()}
                    searchLabel={t("fees.searchStores")}
                    emptyLabel={t("picker.noMatch")}
                  />
                </Show>
                <Show when={chosen()}>
                  <p class="text-sm text-ink-muted">
                    {t("fees.reaches", { count: reached().length })}
                  </p>
                  <div class="flex flex-col gap-1 border-t border-line pt-3">
                    <div>
                      <Button
                        variant="secondary"
                        disabled={busy() || reached().length === 0}
                        onClick={republish}
                      >
                        {t("fees.republish")}
                      </Button>
                    </div>
                    <p class="text-sm text-ink-muted">{t("fees.republishHint")}</p>
                  </div>
                </Show>
                <Show when={writing.error()}>
                  {(message) => <Banner tone="danger" message={message()} />}
                </Show>
              </div>
            </Card>

            <ReportCard />

            <Show
              when={chosen()}
              fallback={<p class="text-sm text-ink-muted">{t("fees.chooseTarget")}</p>}
            >
              <Card title={t("fees.writtenHere", { target: targetName() })}>
                <DataTable
                  columns={ruleColumns([statusColumn()])}
                  rows={rulesHere()}
                  pageSize={CLIENT_PAGE_SIZE}
                  searchText={(row) => `${row.rule.code} ${row.rule.display_name}`}
                  empty={
                    <EmptyState title={t("fees.empty")} description={t("fees.emptyHint")} />
                  }
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <RowActions label={t("common.actions")}>
                      <Button
                        size="sm"
                        class="justify-start"
                        variant="ghost"
                        onClick={() => openEdit(row)}
                      >
                        {t("action.edit")}
                      </Button>
                      <Button
                        size="sm"
                        class="justify-start"
                        variant="danger-ghost"
                        onClick={() => setDeleting(row)}
                      >
                        {t("action.delete")}
                      </Button>
                    </RowActions>
                  )}
                />
              </Card>

              <Show when={scope() !== TENANT}>
                <Card title={t("fees.widerTitle")}>
                  <p class="mb-3 text-sm text-ink-muted">
                    {t("fees.widerHint", { target: targetName() })}
                  </p>
                  <DataTable
                    columns={ruleColumns([fromColumn()])}
                    rows={inherited()}
                    pageSize={CLIENT_PAGE_SIZE}
                    empty={<EmptyState title={t("fees.widerEmpty")} />}
                    actionsHeader={t("common.actions")}
                    actions={(row) => (
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={busy()}
                        onClick={() => openOverride(row)}
                      >
                        {t("fees.overrideHere")}
                      </Button>
                    )}
                  />
                </Card>
              </Show>

              <Show when={scope() === STORE && target()}>
                <EffectiveCard />
              </Show>
            </Show>
          </div>
        </Show>

        <Drawer
          open={editor.mode() === "creating" || editor.mode() === "editing"}
          title={editorTitle()}
          closeLabel={t("action.close")}
          onClose={() => editor.close()}
          footer={
            <>
              <Button variant="secondary" onClick={() => editor.close()}>
                {t("action.cancel")}
              </Button>
              <Button
                data-step="saveFee"
                disabled={editor.saving() || draftGap(draft()) !== null}
                onClick={saveFee}
              >
                {t("fees.save")}
              </Button>
            </>
          }
        >
          <div class="flex flex-col gap-4">
            <Show when={editor.error()}>
              {(message) => <Banner tone="danger" message={message()} />}
            </Show>
            <p class="text-sm text-ink-muted">
              {(() => {
                const subject = editor.subject();
                return subject?.overriding === true
                  ? t("fees.writesOver", {
                      target: targetName(),
                      from: phraseOf(subject.rule.scope, subject.rule.scope_id),
                    })
                  : t("fees.writesTo", { target: targetName(), count: reached().length });
              })()}
            </p>

            <FormSection title={t("fees.section.fee")}>
              <TextField
                data-step="setCode"
                label={t("fees.code")}
                value={draft().code}
                onInput={setCode}
                hint={t("fees.codeHint")}
                autocomplete="off"
              />
              <TextField
                data-step="setDisplayName"
                label={t("fees.displayName")}
                value={draft().displayName}
                onInput={setDisplayName}
                hint={t("fees.displayNameHint")}
                autocomplete="off"
              />
              <For each={translationLocales()}>
                {(locale) => (
                  <TextField
                    label={t("fees.nameIn", { language: languageOf(locale) })}
                    value={draft().translations[locale] ?? ""}
                    onInput={(value) => setTranslation(locale, value)}
                    autocomplete="off"
                  />
                )}
              </For>
            </FormSection>

            <FormSection title={t("fees.section.charge")}>
              <div class="flex flex-col gap-1">
                <span class="text-sm font-medium text-ink">{t("fees.kind")}</span>
                <div class="flex flex-wrap gap-2">
                  <For each={FEE_KINDS}>
                    {(kind) => (
                      <Button
                        data-step="setKind"
                        data-step-value={kind}
                        size="sm"
                        variant={draft().kind === kind ? "primary" : "secondary"}
                        aria-pressed={draft().kind === kind}
                        onClick={() => setKind(kind)}
                      >
                        {t(KIND_LABEL[kind])}
                      </Button>
                    )}
                  </For>
                </div>
                <span class="text-sm text-ink-muted">{t("fees.kindHint")}</span>
              </div>
              <Show
                when={draft().kind === "FEE_KIND_PERCENT"}
                fallback={
                  <>
                    <SelectField
                      label={t("fees.currency")}
                      value={draft().currency}
                      options={currencyOptions().map((code) => ({ value: code, label: code }))}
                      onChange={setCurrency}
                      placeholder={t("fees.pickCurrency")}
                      hint={t("fees.currencyHint")}
                    />
                    <MoneyField
                      data-step="setAmount"
                      label={t("fees.amount")}
                      currencyCode={draft().currency}
                      value={draft().amountMinor}
                      onChange={setAmount}
                    />
                  </>
                }
              >
                <TextField
                  label={t("fees.rate")}
                  value={draft().percent}
                  onInput={setRate}
                  hint={t("fees.rateHint")}
                  autocomplete="off"
                />
                <Show when={draft().percent.trim() !== "" && ratioFromPercent(draft().percent) === null}>
                  <p class="text-sm text-danger">{t("fees.rateInvalid")}</p>
                </Show>
                <SelectField
                  label={t("fees.baseDiscounts")}
                  value={draft().baseDiscounted ? "after" : "before"}
                  options={[
                    { value: "after", label: t("fees.base.afterOption") },
                    { value: "before", label: t("fees.base.beforeOption") },
                  ]}
                  onChange={(value) => patch({ baseDiscounted: value !== "before" })}
                />
                <SelectField
                  label={t("fees.baseTax")}
                  value={draft().baseTaxInclusive ? "inclusive" : "net"}
                  options={[
                    { value: "net", label: t("fees.base.netOption") },
                    { value: "inclusive", label: t("fees.base.inclusiveOption") },
                  ]}
                  onChange={(value) => patch({ baseTaxInclusive: value === "inclusive" })}
                />
              </Show>
            </FormSection>

            <FormSection title={t("fees.section.applies")}>
              <div class="flex flex-col gap-1">
                <span class="text-sm font-medium text-ink">{t("fees.channels")}</span>
                <For each={SALES_CHANNELS}>
                  {(channel) => (
                    <CheckboxField
                      label={t(CHANNEL_LABEL[channel])}
                      checked={draft().channels.includes(channel)}
                      onChange={(ticked) => toggleChannel(channel, ticked)}
                    />
                  )}
                </For>
                <span class="text-sm text-ink-muted">{t("fees.channelsHint")}</span>
              </div>
              <SelectField
                label={t("fees.items")}
                value={draft().itemScope}
                options={FEE_ITEM_SCOPES.map((value) => ({ value, label: t(ITEMS_LABEL[value]) }))}
                onChange={(value) =>
                  patch({
                    itemScope: FEE_ITEM_SCOPES.find((each) => each === value) ?? "FEE_ITEMS_ALL",
                  })
                }
              />
              <Show when={draft().itemScope !== "FEE_ITEMS_ALL"}>
                <MultiComboboxField
                  label={t("fees.categories")}
                  values={draft().categoryIds}
                  options={activeCategories().map((row) => ({
                    value: row.item_category_id,
                    label: row.name,
                  }))}
                  onChange={(values) => patch({ categoryIds: values })}
                  searchLabel={t("fees.searchCategories")}
                  emptyLabel={t("picker.noMatch")}
                  removeLabel={t("picker.remove")}
                  hint={t("fees.categoriesHint")}
                />
                <MultiComboboxField
                  label={t("fees.menuItems")}
                  values={draft().menuItemIds}
                  options={activeItems().map((row) => ({
                    value: row.menu_item_id,
                    label: row.name,
                    keywords: Object.values(row.name_translations),
                  }))}
                  onChange={(values) => patch({ menuItemIds: values })}
                  searchLabel={t("fees.searchItems")}
                  emptyLabel={t("picker.noMatch")}
                  removeLabel={t("picker.remove")}
                />
              </Show>
            </FormSection>

            <FormSection title={t("fees.section.tax")}>
              <SelectField
                label={t("fees.tax")}
                value={draft().tax}
                options={FEE_TAXES.map((value) => ({ value, label: t(TAX_LABEL[value]) }))}
                onChange={(value) =>
                  patch({ tax: FEE_TAXES.find((each) => each === value) ?? "FEE_TAX_FOLLOW_LINES" })
                }
                hint={t("fees.taxHint")}
              />
              <Show when={draft().tax === "FEE_TAX_TAX_CLASS"}>
                <SelectField
                  label={t("fees.taxClass")}
                  value={draft().taxClassId}
                  options={activeTaxClasses().map((row) => ({
                    value: row.tax_class_id,
                    label: row.name,
                  }))}
                  onChange={(value) => patch({ taxClassId: value })}
                  placeholder={t("fees.pickTaxClass")}
                />
              </Show>
              <SwitchField
                label={t("fees.waivable")}
                checked={draft().waivable}
                onChange={(waivable) => patch({ waivable })}
                onLabel={t("fees.waivableOn")}
                offLabel={t("fees.waivableOff")}
              />
              <SwitchField
                label={t("fees.active")}
                checked={draft().active}
                onChange={(active) => patch({ active })}
                onLabel={t("fees.activeOn")}
                offLabel={t("fees.activeOff")}
                hint={t("fees.activeHint")}
              />
            </FormSection>

            <PreviewPanel />

            <Show when={draftGap(draft())}>
              {(gap) => <p class="text-sm text-ink-muted">{t(GAP_LABEL[gap()])}</p>}
            </Show>
          </div>
        </Drawer>

        <ConfirmDialog
          open={deleting() !== null}
          title={t("fees.deleteTitle", { code: deleting()?.rule.code ?? "" })}
          message={(() => {
            const rule = deleting();
            if (rule === null) {
              return "";
            }
            const wider = widerFor(rule.rule.fee_id);
            return wider === undefined
              ? t("fees.deleteBody", { target: targetName() })
              : t("fees.deleteBodyWider", {
                  target: targetName(),
                  from: phraseOf(wider.scope, wider.scope_id),
                });
          })()}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={confirmDelete}
          onCancel={() => setDeleting(null)}
        />
      </RequireContext>
    </div>
  );
}
