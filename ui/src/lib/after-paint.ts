// Resolves once the frame that shows what was just painted has been produced (ADR-0208): the next
// animation frame, then the first task after it, which runs once that frame's style, layout and
// paint are done. Work that can wait for the screen, such as an edge write's request, starts here
// rather than inside the tap, so the tap's frame carries only the tap's change.
//
// A hidden page produces no frames, so a short timer resolves it anyway: the request of a till whose
// screen is off still leaves.
//
// # A page that is leaving
//
// A write painted in the frame before a reload, a navigation or the page closing would otherwise
// never be sent: its request waited for a frame the page will not draw. So `pagehide` resolves every
// wait at once, and the request, sent `keepalive` like every write (`../api/client.ts`), outlives
// the page. The edge then has the write, and what the page painted is read back by the page that
// loads next.

/** The longest the wait for a frame may take before the work goes ahead without one, in ms. */
const NO_FRAME_MS = 50;

/** Every wait not yet resolved, to resolve at once if the page leaves. */
const waiting = new Set<() => void>();
let leaving = false;

if (typeof addEventListener === "function") {
  addEventListener("pagehide", () => {
    leaving = true;
    for (const go of [...waiting]) {
      go();
    }
  });
  // A page kept in the back-forward cache and shown again is not leaving any more.
  addEventListener("pageshow", () => {
    leaving = false;
  });
}

export function afterPaint(): Promise<void> {
  return new Promise((resolve) => {
    let done = false;
    const go = () => {
      if (!done) {
        done = true;
        waiting.delete(go);
        resolve();
      }
    };
    if (leaving) {
      go();
      return;
    }
    waiting.add(go);
    const timer = setTimeout(go, NO_FRAME_MS);
    if (typeof requestAnimationFrame !== "function" || typeof MessageChannel !== "function") {
      clearTimeout(timer);
      go();
      return;
    }
    requestAnimationFrame(() => {
      const channel = new MessageChannel();
      channel.port1.onmessage = () => {
        clearTimeout(timer);
        go();
      };
      channel.port2.postMessage(null);
    });
  });
}
