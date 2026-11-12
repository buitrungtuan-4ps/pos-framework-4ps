// The floor's table list: the form view of the plan, and its accessible fallback (D17).

import type { FloorTable } from "../../api/schemas";
import { type Column, TechnicalDetails } from "../../components/kit";
import { StatusBadge } from "../../components/ui";
import { t } from "../../i18n";

/** The table list's columns, naming each table's area by `areaName`. */
export function tableColumns(areaName: (id: string) => string): Column<FloorTable>[] {
  return [
    {
      key: "label",
      header: t("floor.label"),
      sortValue: (row) => row.label,
      cell: (row) => <span>{row.label}</span>,
    },
    {
      key: "area",
      header: t("floor.area"),
      sortValue: (row) => areaName(row.area_id),
      cell: (row) => <span class="text-ink-muted">{areaName(row.area_id)}</span>,
    },
    {
      key: "seats",
      header: t("floor.seats"),
      sortValue: (row) => row.seats,
      cell: (row) => <span class="text-ink-muted">{row.seats}</span>,
    },
    {
      key: "position",
      header: t("floor.position"),
      cell: (row) => (
        <span class="text-ink-muted">
          {row.position
            ? t("floor.gridAt", { column: row.position.column, row: row.position.row })
            : t("floor.unplaced")}
        </span>
      ),
    },
    {
      key: "status",
      header: t("floor.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "archived" ? "archived" : "active"}
          label={row.status === "archived" ? t("status.archived") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.table_id}</TechnicalDetails>
      ),
    },
  ];
}
