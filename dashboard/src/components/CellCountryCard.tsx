// A store whose country differs from its cell's (ADR-0179 C6): the warning, and the answer to it.
//
// Its own read and its own form, so the store hub mounts it in one line. It draws nothing at all for
// a store that agrees with its cell, or that has no country, or whose cell has none: there is no
// difference there to warn about. A difference warns and never blocks, exactly as the region card
// beside it does (ADR-0114): whether a store's data may rest in a cell homed in another country is a
// legal judgement the operator records, not a rule the console enforces.

import { createSignal, Show } from "solid-js";

import {
  acknowledgeCell,
  type CellAcknowledgement,
  type CellStanding,
  getCellStanding,
} from "../api/cell";
import { t } from "../i18n";
import { apiMessage } from "../lib/errors";
import { formatRelativeAge } from "../lib/format";
import { onScopedContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Button, Card, TextArea } from "./ui";

/** Whole seconds since a Unix-ms instant, clamped at zero. */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

/** The recorded answer: its reason, and how long ago. Who answered is in the audit trail. */
function Answered(props: { answer: CellAcknowledgement }) {
  return (
    <div class="mt-3 space-y-1 border-t border-line pt-3">
      <p class="text-sm font-medium">{t("hub.region.acknowledged")}</p>
      <p class="text-sm text-ink-muted">{props.answer.reason}</p>
      <p class="text-xs text-ink-muted">
        {t("hub.region.acknowledgedWhen", {
          when: formatRelativeAge(ageSeconds(props.answer.acknowledged_at_ms)),
        })}
      </p>
    </div>
  );
}

export function CellCountryCard() {
  const [standing, setStanding] = createSignal<CellStanding | null>(null);
  const [reason, setReason] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [refusal, setRefusal] = createSignal<string | null>(null);

  const read = async (tenant: string, store: string) => {
    try {
      setStanding(await getCellStanding(tenant, store));
    } catch {
      // A failed read draws nothing rather than a warning it cannot back: the hub's other cards
      // already say when the store cannot be read.
      setStanding(null);
    }
  };
  onScopedContext("store", (tenant, store) => {
    setStanding(null);
    void read(tenant, store);
  });

  // Records the answer, then re-reads, so the card redraws from the server's verdict.
  const acknowledge = async (current: CellStanding) => {
    const tenant = tenantId();
    const store = storeId();
    if (tenant === null || store === null) return;
    setBusy(true);
    setRefusal(null);
    try {
      await acknowledgeCell(tenant, store, {
        profile_country: current.profile_country ?? "",
        cell_country: current.cell_country ?? "",
        reason: reason(),
      });
      setReason("");
      await read(tenant, store);
    } catch (error) {
      setRefusal(apiMessage(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show when={standing()?.differs ? standing() : null}>
      {(current) => (
        <Card title={t("hub.cell.title")}>
          <p class="text-sm text-ink">
            {t("hub.cell.differs", {
              cell: current().cell_id,
              cellCountry: current().cell_country ?? "",
              country: current().profile_country ?? "",
            })}
          </p>
          <Show
            when={current().acknowledgement}
            fallback={
              <div class="mt-3 space-y-2 border-t border-line pt-3">
                <TextArea
                  label={t("hub.region.acknowledgeLabel")}
                  rows={2}
                  value={reason()}
                  onInput={setReason}
                  hint={t("hub.region.acknowledgeHint")}
                />
                <Show when={refusal()}>
                  {(message) => <p class="text-xs text-danger">{message()}</p>}
                </Show>
                <Button
                  disabled={busy() || reason().trim() === ""}
                  onClick={() => void acknowledge(current())}
                >
                  {t("hub.region.acknowledgeAction")}
                </Button>
              </div>
            }
          >
            {(answer) => <Answered answer={answer()} />}
          </Show>
        </Card>
      )}
    </Show>
  );
}
