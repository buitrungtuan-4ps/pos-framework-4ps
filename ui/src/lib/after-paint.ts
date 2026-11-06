// Resolves once the frame that shows what was just painted has been produced (ADR-0208): the next
// animation frame, then the first task after it, which runs once that frame's style, layout and
// paint are done. Work that can wait for the screen, such as an edge write's request, starts here
// rather than inside the tap, so the tap's frame carries only the tap's change.
//
// A hidden page produces no frames, so a short timer resolves it anyway: the request of a till whose
// screen is off still leaves.

/** The longest the wait for a frame may take before the work goes ahead without one, in ms. */
const NO_FRAME_MS = 50;

export function afterPaint(): Promise<void> {
  return new Promise((resolve) => {
    let done = false;
    const go = () => {
      if (!done) {
        done = true;
        resolve();
      }
    };
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
