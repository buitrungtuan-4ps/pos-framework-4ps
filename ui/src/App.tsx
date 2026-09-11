import { Show, onCleanup, onMount, type ParentProps } from "solid-js";
import { Route, Router } from "@solidjs/router";

import { ApiError, api, deviceToken } from "./api/client";
import { MINIMUM_EDGE_VERSION, edgeIsBehind, edgeVersion } from "./api/edgeVersion";
import { edgeIsSuperseded, edgeLeaseIsAhead } from "./api/leaseStanding";
import { LiveLink } from "./api/live";
import { StatusBar } from "./components/StatusBar";
import { t } from "./i18n";
import { Devices } from "./screens/Devices";
import { Expo } from "./screens/Expo";
import { Floor } from "./screens/Floor";
import { Kds } from "./screens/Kds";
import { Order } from "./screens/Order";
import { Pairing } from "./screens/Pairing";
import { Pay } from "./screens/Pay";
import { Setup } from "./screens/Setup";
import { Shift } from "./screens/Shift";
import { Confirm } from "./screens/Confirm";
import { Takeaway } from "./screens/Takeaway";
import { SignIn } from "./screens/SignIn";
import { Today } from "./screens/Today";
import { fold, loadStore, setLink } from "./state/store";

// Shown when this app is newer than the store server answering it (ADR-0111). It names both
// versions and nothing else: the operator cannot fix it, and the person who can needs the two
// numbers. It never blocks a sale — ADR-0024 settled that principle one tier down, and a version
// string is not a reason to refuse a customer.
function VersionDrift() {
  return (
    <Show when={edgeIsBehind()}>
      <div
        role="status"
        class="border-b border-line border-l-4 border-l-accent bg-surface-raised px-4 py-2"
      >
        <p class="text-sm font-semibold text-ink">{t("version.behind_title")}</p>
        <p class="text-sm text-ink-muted">
          {t("version.behind_detail", {
            required: MINIMUM_EDGE_VERSION,
            running: edgeVersion() ?? "",
          })}
        </p>
      </div>
    </Show>
  );
}

// Shown when a replacement machine has taken this shop (ADR-0123). Unlike the version banner this
// one describes a refusal the operator will actually meet: this box will not seat a table, open a
// counter order or open a shift, so it says which of those still work here and where the new ones
// go. It still does not block anything on the screen — every control is needed to finish the
// tables this box already holds, and the refusal itself lives on the store server.
function Superseded() {
  return (
    <Show when={edgeIsSuperseded()}>
      <div
        role="alert"
        class="border-b border-line border-l-4 border-l-danger bg-surface-raised px-4 py-2"
      >
        <p class="text-sm font-semibold text-ink">{t("lease.superseded_title")}</p>
        <p class="text-sm text-ink-muted">{t("lease.superseded_detail")}</p>
        <p class="text-sm text-ink-muted">{t("lease.superseded_recovery")}</p>
      </div>
    </Show>
  );
}

// And the quieter one: this box holds a lease generation ahead of the cloud's. It keeps selling
// (ADR-0123 decision 2), because the likeliest cause is a config rollback and refusing would stop
// every till in the shop at once — but somebody should look at it, and the till is where a person
// is.
function LeaseAhead() {
  return (
    <Show when={edgeLeaseIsAhead()}>
      <div
        role="status"
        class="border-b border-line border-l-4 border-l-awaiting bg-surface-raised px-4 py-2"
      >
        <p class="text-sm font-semibold text-ink">{t("lease.ahead_title")}</p>
        <p class="text-sm text-ink-muted">{t("lease.ahead_detail")}</p>
      </div>
    </Show>
  );
}

// The shell every screen sits inside: the status bar, then the routed view. It is the Router's root
// so navigation from the status bar works, while the live link runs above it for the app's lifetime.
function Shell(props: ParentProps) {
  return (
    <div class="flex min-h-full flex-col">
      <StatusBar />
      <Superseded />
      <LeaseAhead />
      <VersionDrift />
      <main class="flex-1 overflow-y-auto">{props.children}</main>
    </div>
  );
}

// Send the browser somewhere without stacking a history entry, and without looping when it is already
// there — the boot gate's one way of moving the operator on.
function sendTo(path: string): void {
  if (window.location.pathname !== path) {
    window.location.replace(path);
  }
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
  // The device half of the boot gate, once the store server itself is known to be usable: pair, then
  // sign in, then draw the store.
  const routeDevice = () => {
    // An unpaired device cannot reach the edge at all (ADR-0084); send it to pair before it tries to
    // draw the store, rather than letting the first call fail.
    if (deviceToken() === null) {
      sendTo("/pair");
      return;
    }
    // Paired, but a command needs a signed-in employee (S0b): confirm one is signed in before drawing
    // the store, and route to sign-in if not. A `401` here means the token is stale — the device must
    // re-pair.
    void api
      .session()
      .then((session) => {
        if (!session.signed_in) {
          sendTo("/signin");
          return;
        }
        // Signed in: draw the store's real floor, its own price book, the console's button plan and
        // its money settings (ADR-0072, ADR-0066, ADR-0105, E5). A failure or an empty node leaves
        // the never-blank fallback in place. The sign-in screen loads the same set on success,
        // because a device that signs in navigates client-side and never reaches this again.
        void loadStore();
      })
      .catch((caught) => {
        if (caught instanceof ApiError && caught.isUnauthorized) {
          sendTo("/pair");
        }
      });
  };

  onMount(() => {
    link.start();
    // A store server provisioned for a cloud must be activated once before it can sync (ADR-0050,
    // ADR-0086); until then the operator belongs on `/setup`. Activation is checked ahead of pairing
    // because it is the box's own standing, not this device's — but it never blocks trading: a store
    // server with no cloud does not mount the route at all, and a keyring that cannot be read answers
    // an error, so a rejection here carries straight on to the counter (ADR-0001).
    void api
      .activation()
      .then((standing) => {
        if (!standing.activated) {
          sendTo("/setup");
          return;
        }
        routeDevice();
      })
      .catch(() => routeDevice());
  });
  onCleanup(() => link.stop());

  return (
    <Router root={Shell}>
      <Route path="/" component={Floor} />
      <Route path="/table/:id" component={Order} />
      <Route path="/table/:id/pay" component={Pay} />
      <Route path="/kds" component={Kds} />
      <Route path="/expo" component={Expo} />
      <Route path="/counter" component={Takeaway} />
      {/* The staff-confirmation queue (ADR-0116) — the screen the QR hold had no way to reach. */}
      <Route path="/guests" component={Confirm} />
      <Route path="/today" component={Today} />
      <Route path="/shift" component={Shift} />
      <Route path="/pair" component={Pairing} />
      <Route path="/devices" component={Devices} />
      <Route path="/setup" component={Setup} />
      <Route path="/signin" component={SignIn} />
    </Router>
  );
}
