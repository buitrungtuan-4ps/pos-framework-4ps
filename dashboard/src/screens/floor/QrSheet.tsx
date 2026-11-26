// The table QR sheet (ADR-0253, ADR-0224 decision 2): one card per active table, its name, its QR code
// and the address the code holds, `<this console's origin>/qr/#t=<token>`, so a phone without a camera
// can type it. Print opens the browser's print dialog with only the sheet on the page. The encoder is
// `qrcode-generator`, the till's, loaded on demand so the console's main bundle does not carry it.

import { createResource, For, Show } from "solid-js";

import type { TableQrToken } from "../../api/schemas";
import { t } from "../../i18n";
import { Button } from "../../components/ui";

const QUIET_ZONE = 4;

/** The guest address a table's code holds, at `origin`. */
export function tableAddress(origin: string, token: string): string {
  return `${origin.replace(/\/+$/, "")}/qr/#t=${token}`;
}

/** The SVG path of a module matrix, one unit square per dark module, inside a quiet zone of four. */
export function modulePath(grid: readonly (readonly boolean[])[]): string {
  let d = "";
  grid.forEach((cells, row) =>
    cells.forEach((dark, col) => {
      if (dark) d += `M${col + QUIET_ZONE} ${row + QUIET_ZONE}h1v1h-1z`;
    }),
  );
  return d;
}

async function matrix(text: string): Promise<boolean[][]> {
  const { default: qrcode } = await import("qrcode-generator");
  // `M` tolerates a scuffed table card; type 0 picks the smallest version that fits.
  const code = qrcode(0, "M");
  code.addData(text);
  code.make();
  const size = code.getModuleCount();
  return Array.from({ length: size }, (_, row) => Array.from({ length: size }, (_, col) => code.isDark(row, col)));
}

function TableCode(props: { label: string; address: string }) {
  const [grid] = createResource(() => props.address, matrix);
  const extent = () => (grid()?.length ?? 0) + QUIET_ZONE * 2;
  return (
    <figure class="flex break-inside-avoid flex-col items-center gap-2 rounded-token border border-line p-4" data-table-qr={props.label}>
      <figcaption class="text-lg font-semibold">{t("floor.qrCardTitle", { table: props.label })}</figcaption>
      <Show when={grid()}>
        {(cells) => (
          <svg role="img" aria-label={t("floor.qrCardAlt", { table: props.label })} viewBox={`0 0 ${extent()} ${extent()}`}
            class="h-48 w-48 bg-white" shape-rendering="crispEdges">
            <path d={modulePath(cells())} fill="#000" />
          </svg>
        )}
      </Show>
      <p class="break-all text-center font-mono text-xs text-ink-muted">{props.address}</p>
    </figure>
  );
}

export function QrSheet(props: { tokens: readonly TableQrToken[] }) {
  const origin = () => window.location.origin;
  return (
    <div class="flex flex-col gap-3" data-outcome="qr-sheet">
      <div class="print:hidden">
        <Button variant="secondary" onClick={() => window.print()}>{t("floor.qrPrint")}</Button>
      </div>
      <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3 print:grid-cols-3">
        <For each={props.tokens}>{(row) => <TableCode label={row.label} address={tableAddress(origin(), row.token)} />}</For>
      </div>
    </div>
  );
}
