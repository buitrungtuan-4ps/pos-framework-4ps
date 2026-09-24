import { Show, createResource } from "solid-js";

// A QR code for a link, drawn as SVG in the page's own colours (ADR-0139).
//
// The encoder is `qrcode-generator`, loaded on demand: only the screens that show a pairing link
// import this component, so the till's main bundle does not carry the library. The module matrix is
// drawn as one path of unit squares, with a quiet zone of four modules, which is what the standard
// asks for and what a phone camera needs to find the code against a busy screen.

interface Props {
  // What the code encodes — the whole content, exactly as the text beside it shows.
  text: string;
  // What a screen reader announces instead of the pattern.
  label: string;
}

const QUIET_ZONE = 4;

async function matrix(text: string): Promise<boolean[][]> {
  const { default: qrcode } = await import("qrcode-generator");
  // Type 0 picks the smallest version that fits; `M` tolerates a smudged or glaring screen.
  const code = qrcode(0, "M");
  code.addData(text);
  code.make();
  const size = code.getModuleCount();
  return Array.from({ length: size }, (_, row) =>
    Array.from({ length: size }, (_, col) => code.isDark(row, col)),
  );
}

export function QrCode(props: Props) {
  const [modules] = createResource(() => props.text, matrix);
  const path = () => {
    const grid = modules();
    if (grid === undefined) {
      return "";
    }
    let d = "";
    grid.forEach((cells, row) =>
      cells.forEach((dark, col) => {
        if (dark) {
          d += `M${col + QUIET_ZONE} ${row + QUIET_ZONE}h1v1h-1z`;
        }
      }),
    );
    return d;
  };
  const extent = () => (modules()?.length ?? 0) + QUIET_ZONE * 2;

  return (
    <Show when={modules()}>
      <svg
        role="img"
        aria-label={props.label}
        viewBox={`0 0 ${extent()} ${extent()}`}
        class="mx-auto block h-48 w-48 rounded-token bg-surface"
        shape-rendering="crispEdges"
        data-outcome="pairing-qr"
      >
        <path d={path()} class="fill-ink" />
      </svg>
    </Show>
  );
}
