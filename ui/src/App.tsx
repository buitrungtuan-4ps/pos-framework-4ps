import { onCleanup, onMount, type ParentProps } from "solid-js";
import { Route, Router } from "@solidjs/router";

import { ApiError, api, deviceToken } from "./api/client";
import { LiveLink } from "./api/live";
import { StatusBar } from "./components/StatusBar";
import { Expo } from "./screens/Expo";
import { Floor } from "./screens/Floor";
import { Kds } from "./screens/Kds";
import { Order } from "./screens/Order";
import { Pairing } from "./screens/Pairing";
import { Pay } from "./screens/Pay";
import { Shift } from "./screens/Shift";
import { SignIn } from "./screens/SignIn";
import { Today } from "./screens/Today";
import { fold, loadFloor, setLink } from "./state/store";

// The shell every screen sits inside: the status bar, then the routed view. It is the Router's root
// so navigation from the status bar works, while the live link runs above it for the app's lifetime.
function Shell(props: ParentProps) {
  return (
    <div class="flex min-h-full flex-col">
      <StatusBar />
      <main class="flex-1 overflow-y-auto">{props.children}</main>
    </div>
  );
}

export function App() {
  const link = new LiveLink({
    onEvent: fold,
    onResync: () => {
      // A resync tells the client its view may be stale; the projection rebuild that answers it is a
      // follow-up. For now the next committed events re-establish the live state.
    },
    onStatus: setLink,
  });
  onMount(() => {
    link.start();
    // An unpaired device cannot reach the edge at all (ADR-0084); send it to pair before it tries to
    // draw the store, rather than letting the first call fail.
    if (deviceToken() === null) {
      if (window.location.pathname !== "/pair") {
        window.location.replace("/pair");
      }
      return;
    }
    // Paired, but a command needs a signed-in employee (S0b): confirm one is signed in before drawing
    // the store, and route to sign-in if not. A `401` here means the token is stale — the device must
    // re-pair.
    void api
      .session()
      .then((session) => {
        if (!session.signed_in) {
          if (window.location.pathname !== "/signin") {
            window.location.replace("/signin");
          }
          return;
        }
        // Signed in: draw the store's real floor and resolve fires to its default station (ADR-0072);
        // a failure or an empty plan leaves the never-blank fallback in place.
        void loadFloor();
      })
      .catch((caught) => {
        if (caught instanceof ApiError && caught.isUnauthorized) {
          if (window.location.pathname !== "/pair") {
            window.location.replace("/pair");
          }
        }
      });
  });
  onCleanup(() => link.stop());

  return (
    <Router root={Shell}>
      <Route path="/" component={Floor} />
      <Route path="/table/:id" component={Order} />
      <Route path="/table/:id/pay" component={Pay} />
      <Route path="/kds" component={Kds} />
      <Route path="/expo" component={Expo} />
      <Route path="/today" component={Today} />
      <Route path="/shift" component={Shift} />
      <Route path="/pair" component={Pairing} />
      <Route path="/signin" component={SignIn} />
    </Router>
  );
}
