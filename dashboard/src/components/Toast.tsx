// Toasts and the notification center (ADR-0060, Track F1). One module-level store: any screen calls
// `toast.ok(msg)` / `toast.error(msg)` for a transient confirmation or error. Each shows as an
// auto-dismissing toast AND is kept in a short history the top-bar bell reveals, so a message the
// operator missed is not lost. The primitive lives here; the CRUD kit (F2) routes write outcomes
// through it. Message text is already translated by the caller.

import { createSignal, For, Show } from "solid-js";

import { t } from "../i18n";

type Tone = "ok" | "danger";
type Note = { id: number; tone: Tone; message: string };

const DISMISS_MS = 5000;
const HISTORY_MAX = 50;

const [history, setHistory] = createSignal<Note[]>([]);
const [live, setLive] = createSignal<Note[]>([]);
let sequence = 0;

function dismiss(id: number): void {
  setLive((current) => current.filter((note) => note.id !== id));
}

function push(tone: Tone, message: string): void {
  sequence += 1;
  const note: Note = { id: sequence, tone, message };
  setHistory((current) => [note, ...current].slice(0, HISTORY_MAX));
  setLive((current) => [...current, note]);
  setTimeout(() => dismiss(note.id), DISMISS_MS);
}

/** The app-wide toast API. */
export const toast = {
  ok: (message: string) => push("ok", message),
  error: (message: string) => push("danger", message),
};

function toneClass(tone: Tone): string {
  return tone === "ok" ? "border-ok" : "border-danger";
}

/** The fixed stack of live toasts; rendered once, at the shell root. */
export function ToastHost() {
  return (
    <div class="pointer-events-none fixed inset-x-0 bottom-0 z-50 flex flex-col items-center gap-2 p-4">
      <For each={live()}>
        {(note) => (
          <div
            role={note.tone === "danger" ? "alert" : "status"}
            class={`pointer-events-auto flex max-w-md items-start gap-3 rounded-token border bg-surface px-3 py-2 text-sm shadow-lg ${toneClass(note.tone)}`}
          >
            <span class="flex-1 text-ink">{note.message}</span>
            <button
              type="button"
              aria-label={t("toast.dismiss")}
              class="shrink-0 text-ink-muted hover:text-ink"
              onClick={() => dismiss(note.id)}
            >
              <span aria-hidden="true">✕</span>
            </button>
          </div>
        )}
      </For>
    </div>
  );
}

/** The top-bar bell: a count of recent notifications and a dropdown of their history. */
export function NotificationBell() {
  const [open, setOpen] = createSignal(false);
  return (
    <div class="relative">
      <button
        type="button"
        aria-label={t("notifications.open")}
        aria-expanded={open()}
        onClick={() => setOpen((value) => !value)}
        class="flex min-h-touch items-center gap-1 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
      >
        <span aria-hidden="true">🔔</span>
        <Show when={history().length > 0}>
          <span class="rounded-full bg-accent px-1.5 text-xs font-medium text-accent-ink">
            {history().length}
          </span>
        </Show>
      </button>
      <Show when={open()}>
        <div class="absolute right-0 z-30 mt-1 w-80 rounded-token border border-line bg-surface shadow-lg">
          <div class="flex items-center justify-between border-b border-line px-3 py-2">
            <span class="text-sm font-semibold text-ink">{t("notifications.open")}</span>
            <button
              type="button"
              class="text-sm text-ink-muted hover:text-ink"
              onClick={() => setHistory([])}
            >
              {t("notifications.clear")}
            </button>
          </div>
          <Show
            when={history().length > 0}
            fallback={<p class="px-3 py-3 text-sm text-ink-muted">{t("notifications.empty")}</p>}
          >
            <ul class="max-h-72 overflow-y-auto p-2">
              <For each={history()}>
                {(note) => (
                  <li class="flex items-start gap-2 rounded-token px-2 py-1 text-sm">
                    <span
                      aria-hidden="true"
                      class={`mt-1.5 h-2 w-2 shrink-0 rounded-full ${note.tone === "ok" ? "bg-ok" : "bg-danger"}`}
                    />
                    <span class="text-ink">{note.message}</span>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </div>
      </Show>
    </div>
  );
}
