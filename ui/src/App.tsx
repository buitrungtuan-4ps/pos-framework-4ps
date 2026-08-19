import { onCleanup, onMount, type ParentProps } from "solid-js";
import { Route, Router } from "@solidjs/router";

import { LiveLink } from "./api/live";
import { StatusBar } from "./components/StatusBar";
import { Expo } from "./screens/Expo";
import { Floor } from "./screens/Floor";
import { Kds } from "./screens/Kds";
import { Order } from "./screens/Order";
import { Pairing } from "./screens/Pairing";
import { Pay } from "./screens/Pay";
import { Shift } from "./screens/Shift";
import { Today } from "./screens/Today";
import { fold, setLink } from "./state/store";

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
  onMount(() => link.start());
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
    </Router>
  );
}
