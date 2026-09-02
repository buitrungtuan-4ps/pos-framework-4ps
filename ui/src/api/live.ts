// The live link: one WebSocket to the edge's `/ws`, reconnecting on its own, feeding committed
// events into a handler. This is what makes a second device see a change the first one made
// (ADR-0018) — the fan-out is the store's shared truth, and being offline (from the cloud) is a
// normal working state, so the only "connected" this reports is to the edge on the LAN.

import { deviceToken } from "./client";

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
  onStatus: (status: LinkStatus) => void;
}

interface TaggedMessage {
  type?: string;
  event_type?: string;
  payload?: unknown;
}

export class LiveLink {
  #socket: WebSocket | null = null;
  #closed = false;
  #retry = 0;
  #timer: ReturnType<typeof setTimeout> | null = null;
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
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    // The device token travels as a WebSocket subprotocol, because the browser `WebSocket` API
    // cannot set an `Authorization` header and `/ws` is behind the paired-device gate (roadmap-v3
    // S0c). A query parameter was the alternative and is worse: the edge logs the request path, so
    // the token would end up in a log. The server selects only the protocol *name*, never the token.
    const token = deviceToken();
    const protocols = token === null ? [SUBPROTOCOL] : [SUBPROTOCOL, token];
    const socket = new WebSocket(`${scheme}://${window.location.host}/ws`, protocols);
    this.#socket = socket;

    socket.addEventListener("open", () => {
      this.#retry = 0;
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
      if (message.type === "event" && typeof message.event_type === "string") {
        this.#handlers.onEvent({ eventType: message.event_type, payload: message.payload });
      } else if (message.type === "resync") {
        this.#handlers.onResync();
      }
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
