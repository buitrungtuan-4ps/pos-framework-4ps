// Image picker + inline upload widget (ADR-0075, Track M5). A compact control showing an entity's
// current image thumbnail with the affordances to pick one from the tenant's media library, upload a
// new image, or remove the reference. Never-blank: a reference to a missing/deleted asset resolves to
// a placeholder tile, not a broken image or an error. Write affordances appear only when the operator
// holds console.media.manage; the server re-checks every media route regardless.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { MediaSummary } from "../api/types";
import { t } from "../i18n";
import { Modal } from "./kit";
import { toast } from "./Toast";
import { Button } from "./ui";

/** A media thumbnail that degrades to a placeholder tile when there is no id or the asset fails to
 *  load — the never-blank posture (ADR-0075). `sizeClass` sets the box (default a 64px square). */
export function MediaThumbnail(props: {
  tenantId: string;
  mediaId: string | null;
  alt: string;
  sizeClass?: string;
}) {
  const [broken, setBroken] = createSignal(false);
  const box = () => props.sizeClass ?? "h-16 w-16";
  const src = () => (props.mediaId ? api.mediaThumbnailUrl(props.tenantId, props.mediaId) : "");
  return (
    <Show
      when={props.mediaId && !broken()}
      fallback={
        <div
          class={`flex items-center justify-center rounded-token border border-line bg-surface-raised text-ink-muted ${box()}`}
          aria-label={t("media.noImage")}
          title={t("media.noImage")}
        >
          <span aria-hidden="true">🖼️</span>
        </div>
      }
    >
      <img
        src={src()}
        alt={props.alt}
        class={`rounded-token border border-line object-cover ${box()}`}
        onError={() => setBroken(true)}
      />
    </Show>
  );
}

/** The image field for an item (and, later, a brand): a thumbnail plus Change / Remove, backed by a
 *  modal that lists the tenant's media and accepts a new upload. `onChange(null)` clears the ref. */
export function ImagePicker(props: {
  tenantId: string;
  value: string | null;
  onChange: (mediaId: string | null) => void;
  canManage: boolean;
  disabled?: boolean;
}) {
  const [open, setOpen] = createSignal(false);
  const [media, setMedia] = createSignal<MediaSummary[] | null>(null);
  const [busy, setBusy] = createSignal(false);

  const fail = (caught: unknown) =>
    toast.error(caught instanceof ApiError ? caught.message : String(caught));

  const loadMedia = async () => {
    setBusy(true);
    try {
      setMedia(await api.listMedia(props.tenantId));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const openPicker = () => {
    setOpen(true);
    void loadMedia();
  };

  const upload = async (file: File) => {
    setBusy(true);
    try {
      const uploaded = await api.uploadMedia(props.tenantId, file);
      toast.ok(t("media.uploaded"));
      props.onChange(uploaded.media_id);
      setOpen(false);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="flex items-center gap-3">
      <MediaThumbnail tenantId={props.tenantId} mediaId={props.value} alt={t("media.imageAlt")} />
      <Show when={props.canManage}>
        <div class="flex flex-col gap-1">
          <Button variant="secondary" disabled={props.disabled} onClick={openPicker}>
            {props.value ? t("media.change") : t("media.setImage")}
          </Button>
          <Show when={props.value}>
            <Button
              variant="secondary"
              disabled={props.disabled}
              onClick={() => props.onChange(null)}
            >
              {t("media.remove")}
            </Button>
          </Show>
        </div>
      </Show>

      <Modal
        open={open()}
        title={t("media.pickTitle")}
        closeLabel={t("action.close")}
        onClose={() => setOpen(false)}
      >
        <div class="flex flex-col gap-4">
          <label class="flex flex-col gap-1 text-sm">
            <span class="font-medium text-ink">{t("media.uploadNew")}</span>
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

          <Show
            when={media()}
            fallback={<p class="text-sm text-ink-muted">{t("media.loading")}</p>}
          >
            {(loaded) => (
              <Show
                when={loaded().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("media.empty")}</p>}
              >
                <div class="grid grid-cols-4 gap-2">
                  <For each={loaded()}>
                    {(asset) => (
                      <button
                        type="button"
                        aria-label={t("media.selectThisImage")}
                        class="rounded-token border border-line p-1 hover:border-accent"
                        onClick={() => {
                          props.onChange(asset.media_id);
                          setOpen(false);
                        }}
                      >
                        <img
                          src={api.mediaThumbnailUrl(props.tenantId, asset.media_id)}
                          alt=""
                          class="h-16 w-full rounded-token object-cover"
                        />
                      </button>
                    )}
                  </For>
                </div>
              </Show>
            )}
          </Show>
        </div>
      </Modal>
    </div>
  );
}
