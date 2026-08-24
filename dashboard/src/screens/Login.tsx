// The super-admin sign-in: password + the current TOTP code (ADR-0034). On success the server sets
// the session cookie and we mark the client authed and go to the dashboard. There is one super
// admin and no username, matching the server's LoginRequest.

import { createSignal, Show } from "solid-js";
import { A, useNavigate } from "@solidjs/router";

import { api, ApiError } from "../api/client";
import { t } from "../i18n";
import { setAuthed } from "../state/session";
import { Banner, Button, Card, TextField } from "../components/ui";

export function Login() {
  const navigate = useNavigate();
  const [password, setPassword] = createSignal("");
  const [totp, setTotp] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const submit = async (event: Event) => {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      await api.login(password(), totp());
      setAuthed(true);
      navigate("/", { replace: true });
    } catch (caught) {
      setError(caught instanceof ApiError ? t("auth.login.failed") : String(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="mx-auto flex min-h-full max-w-md flex-col justify-center p-4">
      <h1 class="mb-6 text-center text-xl font-semibold text-ink">{t("app.title")}</h1>
      <Card title={t("auth.login.title")}>
        <form class="flex flex-col gap-4" onSubmit={(event) => void submit(event)}>
          <TextField
            label={t("auth.password")}
            type="password"
            autocomplete="current-password"
            value={password()}
            onInput={setPassword}
          />
          <TextField
            label={t("auth.totp")}
            type="text"
            autocomplete="one-time-code"
            value={totp()}
            onInput={setTotp}
          />
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Button type="submit" disabled={busy()}>
            {t("action.signIn")}
          </Button>
        </form>
        <p class="mt-4 text-center text-sm text-ink-muted">
          <A href="/setup" class="text-accent underline">
            {t("auth.toSetup")}
          </A>
        </p>
      </Card>
    </div>
  );
}
