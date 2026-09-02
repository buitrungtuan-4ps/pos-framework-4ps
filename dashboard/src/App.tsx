// The dashboard shell and routing (ADR-0060). On load we ask the server whether a super-admin
// session is live (`GET /admin/session`); until that resolves the app shows a neutral loading line,
// then either the authenticated area (inside the nav Shell) or the public login/setup screens. The
// guard is reactive: logging in or out flips `authed` and the routes follow.
//
// # Every guarded screen is a separate chunk (roadmap v3 Q2)
//
// The three public screens are imported eagerly, because they are all an unauthenticated visitor
// can reach. Everything behind the auth guard is `lazy()`, so it is fetched when its route is first
// matched and not before.
//
// Before this, one 540 kB bundle held every screen, and the login page paid for all of them: the
// Reports charts, the Layout editor, all the Catalog sub-screens — downloaded before anyone could
// type a password. Vite had been printing its "chunks are larger than 500 kB" warning and
// recommending exactly this.
//
// The `.then` unwrapping is because the screens use named exports and `lazy` wants a default. It is
// ceremony, but renaming every screen's export to satisfy the loader would be a larger and less
// reversible change than one line of it each.

import { createEffect, createSignal, lazy, onMount, type ParentProps, Show } from "solid-js";
import { Navigate, Route, Router } from "@solidjs/router";

import { api } from "./api/client";
import { Shell } from "./components/Shell";
import { locale, t } from "./i18n";
import { authed, setAuthed } from "./state/session";

// Public: reachable without a session, so they ship in the initial chunk.
import { AcceptInvite } from "./screens/AcceptInvite";
import { Login } from "./screens/Login";
import { Setup } from "./screens/Setup";

// Guarded: one chunk each, fetched when the route is first matched.
const Activation = lazy(() =>
  import("./screens/Activation").then((module) => ({ default: module.Activation })),
);
const Admins = lazy(() =>
  import("./screens/Admins").then((module) => ({ default: module.Admins })),
);
const Alerts = lazy(() =>
  import("./screens/Alerts").then((module) => ({ default: module.Alerts })),
);
const Audit = lazy(() =>
  import("./screens/Audit").then((module) => ({ default: module.Audit })),
);
const ApiKeys = lazy(() =>
  import("./screens/ApiKeys").then((module) => ({ default: module.ApiKeys })),
);
const Campaigns = lazy(() =>
  import("./screens/Campaigns").then((module) => ({ default: module.Campaigns })),
);
const CatalogShell = lazy(() =>
  import("./screens/catalog/CatalogShell").then((module) => ({ default: module.CatalogShell })),
);
const Channels = lazy(() =>
  import("./screens/Channels").then((module) => ({ default: module.Channels })),
);
const Config = lazy(() =>
  import("./screens/Config").then((module) => ({ default: module.Config })),
);
const Layout = lazy(() =>
  import("./screens/Layout").then((module) => ({ default: module.Layout })),
);
const Devices = lazy(() =>
  import("./screens/Devices").then((module) => ({ default: module.Devices })),
);
const Fleet = lazy(() =>
  import("./screens/Fleet").then((module) => ({ default: module.Fleet })),
);
const Inventory = lazy(() =>
  import("./screens/Inventory").then((module) => ({ default: module.Inventory })),
);
const Ota = lazy(() =>
  import("./screens/Ota").then((module) => ({ default: module.Ota })),
);
const Reconcile = lazy(() =>
  import("./screens/Reconcile").then((module) => ({ default: module.Reconcile })),
);
const Floor = lazy(() =>
  import("./screens/Floor").then((module) => ({ default: module.Floor })),
);
const Media = lazy(() =>
  import("./screens/Media").then((module) => ({ default: module.Media })),
);
const MySecurity = lazy(() =>
  import("./screens/MySecurity").then((module) => ({ default: module.MySecurity })),
);
const MySessions = lazy(() =>
  import("./screens/MySessions").then((module) => ({ default: module.MySessions })),
);
const NewStore = lazy(() =>
  import("./screens/NewStore").then((module) => ({ default: module.NewStore })),
);
const People = lazy(() =>
  import("./screens/People").then((module) => ({ default: module.People })),
);
const Reports = lazy(() =>
  import("./screens/Reports").then((module) => ({ default: module.Reports })),
);
const Stations = lazy(() =>
  import("./screens/Stations").then((module) => ({ default: module.Stations })),
);
const StoreSettings = lazy(() =>
  import("./screens/StoreSettings").then((module) => ({ default: module.StoreSettings })),
);
const Stores = lazy(() =>
  import("./screens/Stores").then((module) => ({ default: module.Stores })),
);
const Subjects = lazy(() =>
  import("./screens/Subjects").then((module) => ({ default: module.Subjects })),
);
const TaxRates = lazy(() =>
  import("./screens/TaxRates").then((module) => ({ default: module.TaxRates })),
);
const Translations = lazy(() =>
  import("./screens/Translations").then((module) => ({ default: module.Translations })),
);
const Webhooks = lazy(() =>
  import("./screens/Webhooks").then((module) => ({ default: module.Webhooks })),
);


// The authenticated area: render the nav Shell around the matched child route, or bounce to login.
function Guarded(props: ParentProps) {
  return (
    <Show when={authed()} fallback={<Navigate href="/login" />}>
      <Shell>{props.children}</Shell>
    </Show>
  );
}

export function App() {
  const [ready, setReady] = createSignal(false);

  // Keep the document title and `<html lang>` in step with the active locale — set on first render
  // (not only on a later switch) so a deep-linked, freshly loaded tab is already correct (F1).
  createEffect(() => {
    document.title = t("app.title");
    document.documentElement.lang = locale();
  });

  onMount(() => {
    void api
      .session()
      .then(() => setAuthed(true))
      .catch(() => setAuthed(false))
      .finally(() => setReady(true));
  });

  return (
    <Show
      when={ready()}
      fallback={<p class="p-6 text-sm text-ink-muted">{t("common.loading")}</p>}
    >
      <Router>
        <Route path="/login" component={Login} />
        <Route path="/setup" component={Setup} />
        <Route path="/invite" component={AcceptInvite} />
        <Route path="/" component={Guarded}>
          <Route path="/" component={Reports} />
          <Route path="/fleet" component={Fleet} />
          <Route path="/ota" component={Ota} />
          <Route path="/reconcile" component={Reconcile} />
          <Route path="/audit" component={Audit} />
          <Route path="/alerts" component={Alerts} />
          <Route path="/stores" component={Stores} />
          <Route path="/stores/new" component={NewStore} />
          <Route path="/catalog" component={CatalogShell} />
          <Route path="/campaigns" component={Campaigns} />
          <Route path="/inventory" component={Inventory} />
          <Route path="/channels" component={Channels} />
          <Route path="/media" component={Media} />
          <Route path="/layout" component={Layout} />
          <Route path="/floor" component={Floor} />
          <Route path="/stations" component={Stations} />
          <Route path="/config" component={Config} />
          <Route path="/api-keys" component={ApiKeys} />
          <Route path="/devices" component={Devices} />
          <Route path="/webhooks" component={Webhooks} />
          <Route path="/translations" component={Translations} />
          <Route path="/tax-rates" component={TaxRates} />
          <Route path="/store-settings" component={StoreSettings} />
          <Route path="/people" component={People} />
          <Route path="/subjects" component={Subjects} />
          <Route path="/activation" component={Activation} />
          <Route path="/admins" component={Admins} />
          <Route path="/my-sessions" component={MySessions} />
          <Route path="/my-security" component={MySecurity} />
        </Route>
      </Router>
    </Show>
  );
}
