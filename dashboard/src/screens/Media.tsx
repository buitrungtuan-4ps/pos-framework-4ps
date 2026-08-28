// Media library (ADR-0075, Track M5). The tenant's uploaded images, each stored as two bounded JPEG
// renditions (the pipeline re-encodes on upload; the original is never kept). The operator uploads a
// new image, previews the library, and deletes an asset. Deleting one an item still references is
// allowed — the item then shows a placeholder, never an error (the never-blank posture). Tenant-scoped
// (RLS on the server); upload/delete need console.media.manage (owner/admin), which the server
// re-checks — the gate here only hides what a role cannot do.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { MediaSummary } from "../api/types";
import { locale, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { actingAdmin, tenantId } from "../state/session";
import { ConfirmDialog, EmptyState, TechnicalDetails } from "../components/kit";
import { MediaThumbnail } from "../components/ImagePicker";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import { toast } from "../components/Toast";

export function Media() {
  const [assets, setAssets] = createSignal<MediaSummary[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [pendingDelete, setPendingDelete] = createSignal<MediaSummary | null>(null);

  // console.media.manage → owner/admin (mirrors the backend role set; the server re-checks).
  const canManage = () => {
    const role = actingAdmin()?.role;
    return role === "owner" || role === "admin";
  };

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setAssets(await api.listMedia(tenantId()));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const upload = async (file: File) => {
    setError("");
    setBusy(true);
    try {
      await api.uploadMedia(tenantId(), file);
      toast.ok(t("media.uploaded"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    const asset = pendingDelete();
    if (!asset) {
      return;
    }
    setBusy(true);
    try {
      await api.deleteMedia(tenantId(), asset.media_id);
      setPendingDelete(null);
      toast.ok(t("media.deleted"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const sizeKb = (bytes: number) => t("media.sizeKb", { kb: Math.max(1, Math.round(bytes / 1024)) });
  const createdAt = (ms: number) =>
    new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeStyle: "short" }).format(
      new Date(ms),
    );

  return (
    <div>
      <PageHeader title={t("media.title")} description={t("media.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          <Show when={canManage()}>
            <Card title={t("media.upload")}>
              <label class="flex flex-col gap-1 text-sm">
                <span class="font-medium text-ink">{t("media.chooseFile")}</span>
                <input
                  type="file"
                  accept="image/*"
                  disabled={busy()}
                  class="text-sm text-ink"
                  onChange={(event) => {
                    const file = event.currentTarget.files?.[0];
                    if (file) {
                      void upload(file);
                    }
                    event.currentTarget.value = "";
                  }}
                />
                <span class="text-xs text-ink-muted">{t("media.uploadHint")}</span>
              </label>
            </Card>
          </Show>

          <Card
            title={t("media.library")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show
              when={assets()}
              fallback={<p class="text-sm text-ink-muted">{t("media.loading")}</p>}
            >
              {(loaded) => (
                <Show
                  when={loaded().length > 0}
                  fallback={<EmptyState title={t("media.empty")} description={t("media.emptyHint")} />}
                >
                  <div class="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
                    <For each={loaded()}>
                      {(asset) => (
                        <div class="flex flex-col gap-2 rounded-token border border-line p-3">
                          <MediaThumbnail
                            tenantId={tenantId()}
                            mediaId={asset.media_id}
                            alt={t("media.imageAlt")}
                            sizeClass="h-28 w-full"
                          />
                          <div class="flex flex-col gap-0.5 text-xs text-ink-muted">
                            <span>{sizeKb(asset.detail_bytes)}</span>
                            <span>{createdAt(asset.created_at_ms)}</span>
                          </div>
                          <TechnicalDetails label={t("common.technicalDetails")}>
                            {asset.media_id}
                          </TechnicalDetails>
                          <Show when={canManage()}>
                            <Button
                              variant="danger"
                              disabled={busy()}
                              onClick={() => setPendingDelete(asset)}
                            >
                              {t("action.delete")}
                            </Button>
                          </Show>
                        </div>
                      )}
                    </For>
                  </div>
                </Show>
              )}
            </Show>
          </Card>
        </div>
      </RequireContext>

      <ConfirmDialog
        open={pendingDelete() !== null}
        title={t("media.deleteTitle")}
        message={t("media.deleteMessage")}
        confirmLabel={t("action.delete")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        busy={busy()}
        danger
        onConfirm={() => void remove()}
        onCancel={() => setPendingDelete(null)}
      />
    </div>
  );
}
