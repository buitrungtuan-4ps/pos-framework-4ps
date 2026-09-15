// Media library (ADR-0075, Track M5). The tenant's uploaded images, each stored as two bounded JPEG
// renditions (the pipeline re-encodes on upload; the original is never kept). The operator uploads a
// new image, previews the library, and deletes an asset. Deleting one an item still references is
// allowed — the item then shows a placeholder, never an error (the never-blank posture). Tenant-scoped
// (RLS on the server); upload/delete need console.media.manage (owner/admin), which the server
// re-checks — the gate here only hides what a role cannot do.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import type { MediaSummary } from "../api/types";
import { locale, t } from "../i18n";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { actingAdmin, tenantId } from "../state/session";
import { ConfirmDialog, EmptyState, Pager, TechnicalDetails } from "../components/kit";
import { MediaThumbnail } from "../components/ImagePicker";
import { Banner, Button, Card, FileButton, PageHeader } from "../components/ui";
import { toast } from "../components/Toast";
import { apiMessage } from "../lib/errors";

/**
 * How many assets one page of the grid carries.
 *
 * Divisible by 2, 3 and 4 — the grid's three breakpoints — so no page ends in a ragged row at any
 * width. The read is paged because the grid mounts an `<img>` per asset and each one fetches a
 * thumbnail rendition: on a library of eight hundred that is eight hundred requests when the screen
 * opens, which is the cost paging actually saves here (ADR-0098).
 */
const PAGE_SIZE = 24;

export function Media() {
  // The window the operator is looking at. A view parameter, not load state: the resource reads it
  // when it runs, so moving the pager is `setOffset` then `refetch`, and a tenant switch resets it
  // because the offset that fitted one library means nothing in another.
  const [offset, setOffset] = createSignal(0);
  const [saving, setSaving] = createSignal(false);
  const [pendingDelete, setPendingDelete] = createSignal<MediaSummary | null>(null);

  const library = createAdminResource(
    (tenant) => {
      const from = offset();
      return api.listMediaPage(tenant, { limit: PAGE_SIZE, offset: from });
    },
    { scope: "tenant" },
  );

  /** Move the pager and read that window. */
  const show = async (from: number) => {
    setOffset(Math.max(0, from));
    await library.refetch();
  };

  // console.media.manage → owner/admin (mirrors the backend role set; the server re-checks).
  const canManage = () => {
    const role = actingAdmin()?.role;
    return role === "owner" || role === "admin";
  };

  const upload = async (file: File) => {
    setSaving(true);
    try {
      await api.uploadMedia(tenantId(), file);
      toast.ok(t("media.uploaded"));
      // The read is newest-first, so the upload is on the first page — which is where the operator
      // expects to see the thing they just added.
      await show(0);
    } catch (caught) {
      toast.error(apiMessage(caught));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    const asset = pendingDelete();
    if (!asset) {
      return;
    }
    setSaving(true);
    try {
      await api.deleteMedia(tenantId(), asset.media_id);
      setPendingDelete(null);
      toast.ok(t("media.deleted"));
      // Deleting the last asset on a page leaves the pager past the end of a library that just got
      // shorter, and a re-read at that offset comes back empty over a non-zero count — which reads
      // as "your images are gone". Step back to the previous window instead.
      const page = library.value();
      const lastOnPage = page !== null && page.items.length === 1 && offset() > 0;
      await show(lastOnPage ? offset() - PAGE_SIZE : offset());
    } catch (caught) {
      toast.error(apiMessage(caught));
    } finally {
      setSaving(false);
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
          {/* A refusal is its own state, not an empty grid: a role that cannot read the library has
              not been told the library is empty (D5). */}
          <Show when={failureOf(library)}>
            {(message) => <Banner tone="danger" message={message()} />}
          </Show>

          <Card
            title={t("media.library")}
            actions={
              <div class="flex gap-2">
                {/* Uploading is not a form — there is no field to fill in, only a file to choose —
                    so it is a button in the header rather than a `FormPanel`, and the picker it
                    opens is the operating system's (ADR-0121 §5). */}
                <Show when={canManage()}>
                  <FileButton
                    label={t("media.upload")}
                    accept="image/*"
                    variant="primary"
                    disabled={saving()}
                    onPick={(file) => void upload(file)}
                  />
                </Show>
              </div>
            }
          >
            <Show
              when={library.value()}
              fallback={<p class="text-sm text-ink-muted">{t("media.loading")}</p>}
            >
              {(page) => (
                <Show
                  when={page().items.length > 0}
                  fallback={<EmptyState title={t("media.empty")} description={t("media.emptyHint")} />}
                >
                  <Show when={canManage()}>
                    <p class="mb-3 text-sm text-ink-muted">{t("media.uploadHint")}</p>
                  </Show>
                  <div class="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
                    <For each={page().items}>
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
                              variant="danger-ghost"
                              disabled={saving()}
                              onClick={() => setPendingDelete(asset)}
                            >
                              {t("action.delete")}
                            </Button>
                          </Show>
                        </div>
                      )}
                    </For>
                  </div>
                  <div class="pt-4">
                    <Pager
                      offset={page().offset}
                      limit={PAGE_SIZE}
                      total={page().total}
                      shown={page().items.length}
                      onOffset={(next) => void show(next)}
                    />
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
        busy={saving()}
        danger
        onConfirm={() => void remove()}
        onCancel={() => setPendingDelete(null)}
      />
    </div>
  );
}
