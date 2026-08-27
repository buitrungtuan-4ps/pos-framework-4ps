// My security (ADR-0067, Track G1): self-service account recovery for any signed-in admin — re-enrol
// the authenticator (TOTP) and (re)generate one-time recovery codes. Both are session-gated, so they
// carry no tenant/store context. Re-enrolment re-confirms the current password (the knowledge factor)
// before rotating the possession factor, so a session-only attacker cannot lock the owner out. The
// new TOTP enrolment and the recovery codes are each returned exactly once and never stored in the
// clear — the page shows them this once and keeps nothing.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Enrolment } from "../api/types";
import { t } from "../i18n";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import { ConfirmDialog } from "../components/kit";
import { toast } from "../components/Toast";

export function MySecurity() {
  const [password, setPassword] = createSignal("");
  const [enrolment, setEnrolment] = createSignal<Enrolment | null>(null);
  const [totpError, setTotpError] = createSignal("");
  const [totpBusy, setTotpBusy] = createSignal(false);

  const [remaining, setRemaining] = createSignal<number | null>(null);
  const [codes, setCodes] = createSignal<string[]>([]);
  const [codesError, setCodesError] = createSignal("");
  const [codesBusy, setCodesBusy] = createSignal(false);
  const [confirmRegen, setConfirmRegen] = createSignal(false);

  const loadStatus = async () => {
    try {
      const status = await api.recoveryCodesStatus();
      setRemaining(status.remaining);
    } catch (caught) {
      setCodesError(caught instanceof ApiError ? caught.message : String(caught));
    }
  };

  void loadStatus();

  const reenrol = async () => {
    if (!password()) {
      setTotpError(t("security.passwordRequired"));
      return;
    }
    setTotpError("");
    setEnrolment(null);
    setTotpBusy(true);
    try {
      setEnrolment(await api.reenrolTotp(password()));
      setPassword("");
      toast.ok(t("security.totpRotated"));
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setTotpError(message);
      toast.error(message);
    } finally {
      setTotpBusy(false);
    }
  };

  const generate = async () => {
    setCodesError("");
    setConfirmRegen(false);
    setCodesBusy(true);
    try {
      const generated = await api.generateRecoveryCodes();
      setCodes(generated.codes);
      setRemaining(generated.remaining);
      toast.ok(t("security.codesGenerated"));
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setCodesError(message);
      toast.error(message);
    } finally {
      setCodesBusy(false);
    }
  };

  // Regenerating invalidates any previous set, so confirm first when codes already exist; the very
  // first generation (none yet) needs no warning.
  const onGenerateClicked = () => {
    if ((remaining() ?? 0) > 0) {
      setConfirmRegen(true);
    } else {
      void generate();
    }
  };

  return (
    <div>
      <PageHeader title={t("security.title")} description={t("security.description")} />
      <div class="grid gap-6 lg:grid-cols-2">
        <Card title={t("security.totpTitle")}>
          <p class="mb-4 text-sm text-ink-muted">{t("security.totpDescription")}</p>
          <Show
            when={enrolment()}
            fallback={
              <div class="flex flex-col gap-4">
                <TextField
                  label={t("auth.password")}
                  type="password"
                  autocomplete="current-password"
                  value={password()}
                  onInput={setPassword}
                />
                <Show when={totpError()}>
                  {(message) => <Banner tone="danger" message={message()} />}
                </Show>
                <Button disabled={totpBusy()} onClick={() => void reenrol()}>
                  {t("security.reenrol")}
                </Button>
              </div>
            }
          >
            {(done) => (
              <div class="flex flex-col gap-3">
                <Banner tone="ok" message={t("security.totpScanHint")} />
                <div>
                  <span class="mb-1 block text-sm font-medium text-ink">
                    {t("auth.setup.otpauth")}
                  </span>
                  <code class="block break-all rounded-token border border-line bg-surface-raised p-2 text-xs text-ink">
                    {done().otpauth_uri}
                  </code>
                </div>
                <div>
                  <span class="mb-1 block text-sm font-medium text-ink">
                    {t("auth.setup.secret")}
                  </span>
                  <code class="block break-all rounded-token border border-line bg-surface-raised p-2 text-sm text-ink">
                    {done().secret_base32}
                  </code>
                </div>
                <Button variant="secondary" onClick={() => setEnrolment(null)}>
                  {t("action.done")}
                </Button>
              </div>
            )}
          </Show>
        </Card>

        <Card title={t("security.codesTitle")}>
          <p class="mb-4 text-sm text-ink-muted">{t("security.codesDescription")}</p>
          <Show when={remaining() !== null}>
            {(_present) => (
              <p class="mb-3 text-sm text-ink">
                {t("security.codesRemaining", { count: remaining() ?? 0 })}
              </p>
            )}
          </Show>
          <Show when={codesError()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={codes().length > 0}>
            <div class="my-3">
              <Banner tone="ok" message={t("security.codesOnce")} />
              <ul class="mt-2 grid grid-cols-2 gap-1">
                <For each={codes()}>
                  {(code) => (
                    <li class="break-all rounded-token border border-line bg-surface-raised p-2 text-center font-mono text-sm text-ink">
                      {code}
                    </li>
                  )}
                </For>
              </ul>
            </div>
          </Show>
          <Button disabled={codesBusy()} onClick={onGenerateClicked}>
            {(remaining() ?? 0) > 0 ? t("security.regenerate") : t("security.generate")}
          </Button>
        </Card>
      </div>

      <ConfirmDialog
        open={confirmRegen()}
        title={t("security.regenTitle")}
        message={t("security.regenMessage")}
        confirmLabel={t("security.regenerate")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        danger
        busy={codesBusy()}
        onConfirm={() => void generate()}
        onCancel={() => setConfirmRegen(false)}
      />
    </div>
  );
}
