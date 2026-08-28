// The dashboard shell and routing (ADR-0060). On load we ask the server whether a super-admin
// session is live (`GET /admin/session`); until that resolves the app shows a neutral loading line,
// then either the authenticated area (inside the nav Shell) or the public login/setup screens. The
// guard is reactive: logging in or out flips `authed` and the routes follow.

import { createEffect, createSignal, onMount, type ParentProps, Show } from "solid-js";
import { Navigate, Route, Router } from "@solidjs/router";

import { api } from "./api/client";
import { Shell } from "./components/Shell";
import { locale, t } from "./i18n";
import { authed, setAuthed } from "./state/session";
import { AcceptInvite } from "./screens/AcceptInvite";
import { Activation } from "./screens/Activation";
import { Admins } from "./screens/Admins";
import { Alerts } from "./screens/Alerts";
import { Audit } from "./screens/Audit";
import { ApiKeys } from "./screens/ApiKeys";
import { Catalog } from "./screens/Catalog";
import { Config } from "./screens/Config";
import { Layout } from "./screens/Layout";
import { Devices } from "./screens/Devices";
import { Fleet } from "./screens/Fleet";
import { Floor } from "./screens/Floor";
import { Login } from "./screens/Login";
import { Media } from "./screens/Media";
import { MySecurity } from "./screens/MySecurity";
import { MySessions } from "./screens/MySessions";
import { NewStore } from "./screens/NewStore";
import { People } from "./screens/People";
import { Reports } from "./screens/Reports";
import { Setup } from "./screens/Setup";
import { Stations } from "./screens/Stations";
import { StoreSettings } from "./screens/StoreSettings";
import { Stores } from "./screens/Stores";
import { TaxRates } from "./screens/TaxRates";
import { Translations } from "./screens/Translations";
import { Webhooks } from "./screens/Webhooks";

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
          <Route path="/audit" component={Audit} />
          <Route path="/alerts" component={Alerts} />
          <Route path="/stores" component={Stores} />
          <Route path="/stores/new" component={NewStore} />
          <Route path="/catalog" component={Catalog} />
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
          <Route path="/activation" component={Activation} />
          <Route path="/admins" component={Admins} />
          <Route path="/my-sessions" component={MySessions} />
          <Route path="/my-security" component={MySecurity} />
        </Route>
      </Router>
    </Show>
  );
}
