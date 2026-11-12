// "The preview matches the till" as something a test can compare (ADR-0209 decision 4).
//
// `marked` wraps each implementation of a table in a box that names its component and draws as
// nothing (`display: contents`), so a drawn tree says where each node begins. `shape` then writes the
// DOM a tree drew as text: every element and text of a component that draws from the package alone,
// and for a region only its name and the nodes inside it, since what a region holds is its host's
// (the till's order lines, the preview's outline). The till's mount and the console's preview each
// write the shape of one committed fixture, and both must equal `fixtures/preview-shape.txt`.
//
// Production never marks a tree: the till's screens draw with the plain table.

import { isBase } from "./base";
import type { ComponentId } from "./components";
import type { Implementation, Implementations } from "./render";

/** The attribute a marked node's box carries: its component id. */
export const NODE_ATTRIBUTE = "data-package-node";

/** `implementations`, each drawn inside a box that names its component. */
export function marked(implementations: Implementations): Implementations {
  const wrap = (id: ComponentId, implementation: Implementation): Implementation =>
    function Marked(props) {
      return (
        <div class="contents" data-package-node={id}>
          {implementation(props)}
        </div>
      );
    };
  const entries = Object.entries(implementations) as [ComponentId, Implementation][];
  return Object.fromEntries(entries.map(([id, implementation]) => [id, wrap(id, implementation)])) as Implementations;
}

/** The attributes `shape` writes; anything else (a handler's, a style) is a front end's own. */
const KEPT = ["class", "type", "disabled", "role", "aria-busy", "data-action-id", "data-package-fallback"];

function attributes(element: Element): string {
  return KEPT.filter((name) => element.hasAttribute(name))
    .map((name) => {
      const value = element.getAttribute(name) ?? "";
      return value === "" ? ` ${name}` : ` ${name}="${value.split(/\s+/).filter(Boolean).join(" ")}"`;
    })
    .join("");
}

function write(node: Node, depth: number, inRegion: boolean, out: string[]): void {
  const indent = "  ".repeat(depth);
  if (node.nodeType === 3) {
    const text = (node.textContent ?? "").trim();
    if (!inRegion && text !== "") {
      out.push(`${indent}${JSON.stringify(text)}`);
    }
    return;
  }
  if (node.nodeType !== 1) {
    return;
  }
  const element = node as Element;
  const id = element.getAttribute(NODE_ATTRIBUTE);
  if (id !== null && isBase(id as ComponentId)) {
    element.childNodes.forEach((child) => write(child, depth, false, out));
    return;
  }
  if (id !== null) {
    out.push(`${indent}[region ${id}]`);
    element.childNodes.forEach((child) => write(child, depth + 1, true, out));
    return;
  }
  if (inRegion) {
    element.childNodes.forEach((child) => write(child, depth, true, out));
    return;
  }
  out.push(`${indent}<${element.tagName.toLowerCase()}${attributes(element)}>`);
  element.childNodes.forEach((child) => write(child, depth + 1, false, out));
}

/** The shape of what a marked tree drew into `root`: one line per element, text or region. */
export function shape(root: Element): string {
  const out: string[] = [];
  root.childNodes.forEach((child) => write(child, 0, false, out));
  return out.join("\n");
}
