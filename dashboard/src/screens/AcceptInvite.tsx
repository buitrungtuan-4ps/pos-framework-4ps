// Accept an invitation and self-enrol (ADR-0067, Track G1) — the invitee's counterpart to the
// owner/admin invite. Public and pre-auth: the single-use token from the invite link *is* the
// authorisation, so there is no session here (mirroring /setup). The invitee pastes the token (the
// link prefills it) and chooses their own password; the server mints their credential and returns the
// one-time TOTP enrolment, which they add to an authenticator before signing in. No admin ever sets
// or learns the password.

import { createSignal, Show } from "solid-js";
import { A, useSearchParams } from "@solidjs/router";

import { api, ApiError } from "../api/client";
import type { Enrolment } from "../api/types";
import { t } from "../i18n";
import { Banner, Button, Card, TextField } from "../components/ui";

export function AcceptInvite() {
  const [searchParams] = useSearchParams();
  const initialToken = typeof searchParams.token === "string" ? searchParams.token : "";
  const [token, setToken] = createSignal(initialToken);
  const [password, setPassword] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [enrolment, setEnrolment] = createSignal<Enrolment | null>(null);

  const submit = async (event: Event) => {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      setEnrolment(await api.acceptInvite(token().trim(), password()));
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
          <Card title={t("invite.title")}>
            <p class="mb-4 text-sm text-ink-muted">{t("invite.description")}</p>
            <form class="flex flex-col gap-4" onSubmit={(event) => void submit(event)}>
              <TextField label={t("invite.token")} value={token()} onInput={setToken} />
              <TextField
                label={t("invite.password")}
                type="password"
                autocomplete="new-password"
                value={password()}
                onInput={setPassword}
              />
              <Show when={error()}>
                {(message) => <Banner tone="danger" message={message()} />}
              </Show>
              <Button type="submit" disabled={busy()}>
                {t("invite.accept")}
              </Button>
            </form>
            <p class="mt-4 text-center text-sm text-ink-muted">
              <A href="/login" class="text-accent underline">
                {t("auth.toLogin")}
              </A>
            </p>
          </Card>
        }
      >
        {(done) => (
          <Card title={t("invite.enrolled")}>
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
