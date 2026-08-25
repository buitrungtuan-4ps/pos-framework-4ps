// The dashboard shell and routing (ADR-0060). On load we ask the server whether a super-admin
// session is live (`GET /admin/session`); until that resolves the app shows a neutral loading line,
// then either the authenticated area (inside the nav Shell) or the public login/setup screens. The
// guard is reactive: logging in or out flips `authed` and the routes follow.

import { createSignal, onMount, type ParentProps, Show } from "solid-js";
import { Navigate, Route, Router } from "@solidjs/router";

import { api } from "./api/client";
import { Shell } from "./components/Shell";
import { t } from "./i18n";
import { authed, setAuthed } from "./state/session";
import { Activation } from "./screens/Activation";
import { ApiKeys } from "./screens/ApiKeys";
import { Config } from "./screens/Config";
import { Devices } from "./screens/Devices";
import { Login } from "./screens/Login";
import { NewStore } from "./screens/NewStore";
import { Reports } from "./screens/Reports";
import { Setup } from "./screens/Setup";
import { Stores } from "./screens/Stores";
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
        <Route path="/" component={Guarded}>
          <Route path="/" component={Reports} />
          <Route path="/stores" component={Stores} />
          <Route path="/stores/new" component={NewStore} />
          <Route path="/config" component={Config} />
          <Route path="/api-keys" component={ApiKeys} />
          <Route path="/devices" component={Devices} />
          <Route path="/webhooks" component={Webhooks} />
          <Route path="/translations" component={Translations} />
          <Route path="/activation" component={Activation} />
        </Route>
      </Router>
    </Show>
  );
}
