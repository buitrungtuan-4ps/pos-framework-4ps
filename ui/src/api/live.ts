// The live link: one WebSocket to the edge's `/ws`, reconnecting on its own, feeding committed
// events into a handler. This is what makes a second device see a change the first one made
// (ADR-0018) — the fan-out is the store's shared truth, and being offline (from the cloud) is a
// normal working state, so the only "connected" this reports is to the edge on the LAN.
//
// # A link that drops picks up where it left off
//
// Every frame the edge publishes carries its `stream_id` and `sequence`, and this remembers the last
// one it applied. On a reconnect it names that position (`/ws?stream_id=…&after_sequence=…`) and the
// edge sends what happened meanwhile before the live stream carries on, so a kitchen display that
// lost Wi-Fi for twenty seconds still gets the fires from those twenty seconds. Before this it came
// back to the live stream alone and never knew what it had missed.
//
// Whenever that cannot be done completely the answer is the reload a `resync` already makes: the
// edge says `resync` when it cannot replay from the position (the device was away too long, or the
// edge restarted), and this treats as a gap any frame that does not continue the stream it holds.

import { deviceToken, liveSocketUrl } from "./credentials";

// The subprotocol name the edge selects. It must match `pos_edge::http::ws::SUBPROTOCOL`.
const SUBPROTOCOL = "pos-edge.v1";

export interface ServerEvent {
  eventType: string;
  payload: unknown;
}

export type LinkStatus = "connecting" | "open" | "closed";

export interface LiveLinkHandlers {
  onEvent: (event: ServerEvent) => void;
  onResync: () => void;
  // The edge applied a new store configuration (ADR-0160 item 7): reload what the till draws from
  // it — the floor, the menu, the layout, the locale and the reason codes — without a new sign-in.
  onConfigApplied: () => void;
  onStatus: (status: LinkStatus) => void;
}

interface TaggedMessage {
  type?: string;
  event_type?: string;
  payload?: unknown;
  stream_id?: unknown;
  sequence?: unknown;
}

// Where this device is in the edge's fan-out: the stream it is reading and the last frame on it
// that it applied.
interface Position {
  streamId: string;
  sequence: number;
}

// A frame's own position, or `null` for one that carries none — the `resync` the edge sends one
// device on its own, and every frame from an edge that predates resume.
function positionOf(message: TaggedMessage): Position | null {
  const { stream_id: streamId, sequence } = message;
  if (typeof streamId !== "string" || typeof sequence !== "number") {
    return null;
  }
  return { streamId, sequence };
}

// Whether a frame skipped something since `held`.
//
// A frame continues the stream when it is the next one on the stream the device holds, and anything
// else is a gap: another stream, a sequence that jumps, or a frame with no position after the device
// named one — an edge that predates resume ignored it and sent the live stream alone. A device with
// no position has nothing to compare against, so it adopts whatever comes.
function skipped(held: Position | null, next: Position | null): boolean {
  if (held === null) {
    return false;
  }
  return next === null || next.streamId !== held.streamId || next.sequence !== held.sequence + 1;
}

// The `/ws` URL for a (re)connect.
//
// The first connect names nothing, and is served from the next frame as every device always was —
// the boot gate has just read what is open. A reconnect names the position it holds. One that holds
// none — its link dropped before it saw a single frame — cannot say what it missed, so it asks to
// resume from nothing: `after_sequence=0` with no stream, which the edge answers `resync` once the
// socket is subscribed. That, and not a reload started on `open`, is what makes the read land after
// the subscription, so a frame published while it runs is not lost between the two.
function socketUrl(reconnecting: boolean, position: Position | null): string {
  const base = liveSocketUrl();
  if (!reconnecting) {
    return base;
  }
  const url = new URL(base);
  if (position === null) {
    url.searchParams.set("after_sequence", "0");
  } else {
    url.searchParams.set("stream_id", position.streamId);
    url.searchParams.set("after_sequence", String(position.sequence));
  }
  return url.toString();
}

export class LiveLink {
  #socket: WebSocket | null = null;
  #closed = false;
  #retry = 0;
  #timer: ReturnType<typeof setTimeout> | null = null;
  // Whether a socket of this link has been open before, so the next one is a reconnect that may
  // have missed something.
  #wasOpen = false;
  #position: Position | null = null;
  readonly #handlers: LiveLinkHandlers;

  constructor(handlers: LiveLinkHandlers) {
    this.#handlers = handlers;
  }

  start(): void {
    this.#closed = false;
    this.#connect();
  }

  stop(): void {
    this.#closed = true;
    if (this.#timer !== null) {
      clearTimeout(this.#timer);
      this.#timer = null;
    }
    this.#socket?.close();
    this.#socket = null;
  }

  #connect(): void {
    this.#handlers.onStatus("connecting");
    // The device token travels as a WebSocket subprotocol, because the browser `WebSocket` API
    // cannot set an `Authorization` header and `/ws` is behind the paired-device gate (roadmap-v3
    // S0c). A query parameter was the alternative and is worse: the edge logs the request path, so
    // the token would end up in a log. The server selects only the protocol *name*, never the token.
    const token = deviceToken();
    const protocols = token === null ? [SUBPROTOCOL] : [SUBPROTOCOL, token];
    // Derived from the same base the domain calls use (ADR-0111): an in-store till gets today's
    // expression unchanged, and a device paired against a named edge opens the socket there rather
    // than against whatever origin happened to serve the page. The position rides in the query and
    // not beside the token: it is not a secret, and the edge logs the path, never the query.
    const socket = new WebSocket(socketUrl(this.#wasOpen, this.#position), protocols);
    this.#socket = socket;

    socket.addEventListener("open", () => {
      this.#retry = 0;
      this.#wasOpen = true;
      this.#handlers.onStatus("open");
    });

    socket.addEventListener("message", (event: MessageEvent<unknown>) => {
      if (typeof event.data !== "string") {
        return;
      }
      let message: TaggedMessage;
      try {
        message = JSON.parse(event.data) as TaggedMessage;
      } catch {
        return;
      }
      const next = positionOf(message);
      if (message.type === "resync") {
        // What the device holds is about to be replaced by the reload, so whatever position it had
        // is spent: the next frame starts a fresh one. A resync published to every device carries
        // its own position, and one sent to this device alone carries none.
        this.#position = next;
        this.#handlers.onResync();
        return;
      }
      if (skipped(this.#position, next)) {
        this.#handlers.onResync();
      }
      if (message.type === "event" && typeof message.event_type === "string") {
        this.#handlers.onEvent({ eventType: message.event_type, payload: message.payload });
      } else if (message.type === "config_applied") {
        this.#handlers.onConfigApplied();
      }
      // Recorded once the frame is applied, because the position is the last frame this device
      // has, not the last one it was sent.
      this.#position = next;
    });

    socket.addEventListener("close", () => {
      this.#handlers.onStatus("closed");
      this.#socket = null;
      this.#scheduleReconnect();
    });

    // An error is always followed by a close, which is where the reconnect is scheduled.
    socket.addEventListener("error", () => socket.close());
  }

  #scheduleReconnect(): void {
    if (this.#closed) {
      return;
    }
    // Back off to a ceiling, so a store that has lost its edge does not spin.
    const delay = Math.min(500 * 2 ** this.#retry, 5000);
    this.#retry += 1;
    this.#timer = setTimeout(() => this.#connect(), delay);
  }
}
