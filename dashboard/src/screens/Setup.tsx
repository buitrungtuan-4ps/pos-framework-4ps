// First-run enrolment (ADR-0034): exchange the one-time setup token (bootstrap.sh minted it into
// cloud.toml) and a chosen password for the super-admin credential. The server returns the TOTP
// enrolment once — the otpauth:// URI and the base32 secret — which the operator adds to an
// authenticator app before signing in. We show it exactly once and never store it.

import { createSignal, Show } from "solid-js";
import { A } from "@solidjs/router";

import { api, ApiError } from "../api/client";
import type { Enrolment } from "../api/types";
import { t } from "../i18n";
import { Banner, Button, Card, TextField } from "../components/ui";

export function Setup() {
  const [token, setToken] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [enrolment, setEnrolment] = createSignal<Enrolment | null>(null);

  const submit = async (event: Event) => {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      setEnrolment(await api.setup(token(), password()));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="mx-auto flex min-h-full max-w-md flex-col justify-center p-4">
      <h1 class="mb-6 text-center text-xl font-semibold text-ink">{t("app.title")}</h1>
      <Show
        when={enrolment()}
        fallback={
          <Card title={t("auth.setup.title")}>
            <p class="mb-4 text-sm text-ink-muted">{t("auth.setup.description")}</p>
            <form class="flex flex-col gap-4" onSubmit={(event) => void submit(event)}>
              <TextField
                label={t("auth.setupToken")}
                value={token()}
                onInput={setToken}
              />
              <TextField
                label={t("auth.password")}
                type="password"
                autocomplete="new-password"
                value={password()}
                onInput={setPassword}
              />
              <Show when={error()}>
                {(message) => <Banner tone="danger" message={message()} />}
              </Show>
              <Button type="submit" disabled={busy()}>
                {t("action.enrol")}
              </Button>
            </form>
          </Card>
        }
      >
        {(done) => (
          <Card title={t("auth.setup.enrolled")}>
            <p class="mb-4 text-sm text-ink-muted">{t("auth.setup.scanHint")}</p>
            <div class="mb-3">
              <span class="mb-1 block text-sm font-medium text-ink">{t("auth.setup.otpauth")}</span>
              <code class="block break-all rounded-token border border-line bg-surface-raised p-2 text-xs text-ink">
                {done().otpauth_uri}
              </code>
            </div>
            <div class="mb-4">
              <span class="mb-1 block text-sm font-medium text-ink">{t("auth.setup.secret")}</span>
              <code class="block break-all rounded-token border border-line bg-surface-raised p-2 text-sm text-ink">
                {done().secret_base32}
              </code>
            </div>
            <A href="/login" class="text-accent underline">
              {t("auth.toLogin")}
            </A>
          </Card>
        )}
      </Show>
    </div>
  );
}
