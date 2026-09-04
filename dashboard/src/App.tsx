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

import {
  type Component,
  createEffect,
  createSignal,
  lazy,
  onMount,
  type ParentProps,
  Show,
} from "solid-js";
import {
  Navigate,
  Route,
  Router,
  useParams,
  useSearchParams,
} from "@solidjs/router";

import { api } from "./api/client";
import { Shell } from "./components/Shell";
import { locale, t } from "./i18n";
import {
  authed,
  setAuthed,
  setStoreId,
  setTenantId,
  storeId,
  tenantId,
} from "./state/session";
import { SCREENS, type ScreenId, screenHref, TENANT_PREFIX } from "./state/screens";

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

// Every guarded screen, by id. A `Record<ScreenId, …>` rather than a list, so a screen added to
// `SCREENS` without a component here does not compile — the router cannot fall behind the table.
const COMPONENTS: Record<ScreenId, Component> = {
  reports: Reports,
  fleet: Fleet,
  ota: Ota,
  reconcile: Reconcile,
  alerts: Alerts,
  audit: Audit,
  stores: Stores,
  newStore: NewStore,
  catalog: CatalogShell,
  campaigns: Campaigns,
  inventory: Inventory,
  channels: Channels,
  media: Media,
  layout: Layout,
  floor: Floor,
  stations: Stations,
  people: People,
  config: Config,
  storeSettings: StoreSettings,
  taxRates: TaxRates,
  translations: Translations,
  subjects: Subjects,
  apiKeys: ApiKeys,
  devices: Devices,
  activation: Activation,
  admins: Admins,
  webhooks: Webhooks,
  mySessions: MySessions,
  mySecurity: MySecurity,
};

const TENANT_SCOPED = (Object.keys(SCREENS) as ScreenId[]).filter(
  (id) => SCREENS[id].tenantScoped,
);
const CONSOLE_LEVEL = (Object.keys(SCREENS) as ScreenId[]).filter(
  (id) => !SCREENS[id].tenantScoped,
);

// Reads the working context out of the URL and into the signals every screen already reads.
//
// The URL is the source of truth while a tenant-scoped screen is mounted: that is what makes a
// console link shareable and lets two tabs sit on different tenants, which localStorage alone could
// never do (it is per-origin, so a second tab would fight the first). localStorage stays as the
// *memory* of the last context, for the redirect that turns a bare `/people` back into a real URL.
function TenantContext(props: ParentProps) {
  const params = useParams<{ tenant: string }>();
  const [search] = useSearchParams<{ store?: string }>();
  createEffect(() => {
    const fromUrl = params.tenant ?? "";
    if (fromUrl && fromUrl !== tenantId()) {
      setTenantId(fromUrl);
    }
    // An absent `?store=` clears the store rather than leaving the previous one in place: the URL
    // says what the context is, and a link without a store means "no store", not "whatever was
    // there before".
    const store = search.store ?? "";
    if (store !== storeId()) {
      setStoreId(store);
    }
  });
  return <>{props.children}</>;
}

// A bare screen path — an old bookmark, a link made before this shape existed — redirected to the
// remembered tenant. With no tenant remembered it lands on the index, where the context picker is
// the first thing an operator sees (F0).
function LegacyRedirect(props: { screen: ScreenId }) {
  return <Navigate href={screenHref(props.screen, tenantId(), storeId())} />;
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
          {/* Tenant-scoped screens live under `/t/<tenant>`, so a link carries the context it was
              read under. The store rides in `?store=` where a screen uses one. */}
          <Route path={`${TENANT_PREFIX}/:tenant`} component={TenantContext}>
            {TENANT_SCOPED.map((id) => (
              <Route path={SCREENS[id].path} component={COMPONENTS[id]} />
            ))}
          </Route>
          {/* Console-level screens span every tenant, so they keep a bare path. */}
          {CONSOLE_LEVEL.map((id) => (
            <Route path={SCREENS[id].path} component={COMPONENTS[id]} />
          ))}
          {/* The pre-context landing: no tenant chosen yet, or none remembered. The Shell's picker
              is the first thing here, which is what F0's context gate exists for. */}
          <Route path="/" component={COMPONENTS.reports} />
          {/* Old bookmarks. Every tenant-scoped path that used to be absolute still resolves, now by
              redirecting through the remembered tenant. */}
          {TENANT_SCOPED.filter((id) => SCREENS[id].path !== "/").map((id) => (
            <Route path={SCREENS[id].path} component={() => <LegacyRedirect screen={id} />} />
          ))}
        </Route>
      </Router>
    </Show>
  );
}
