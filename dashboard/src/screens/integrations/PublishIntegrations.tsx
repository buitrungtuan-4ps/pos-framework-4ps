// Publishing a store's integrations from the console (phase 10 open item O5): the connections that
// serve the store in context become its `integrations` node, and its delivery connections its
// `vendors` node (ADR-0215 decision 5), through `PUT /admin/config/integrations`. Until this card
// the route had no screen, and a connection or a delivery setting changed in the console reached no
// store.

import { createSignal, Show } from "solid-js";

import { publishIntegrations } from "../../api/integrations";
import type { IntegrationsPublished } from "../../api/schemas";
import { PublishBar } from "../../components/kit";
import { toast } from "../../components/Toast";
import { Card } from "../../components/ui";
import { t } from "../../i18n";
import { useEntityCrud } from "../../lib/entity-crud";
import { describePublish } from "../../lib/publish-copy";
import { lastEditedMs, usePublishedNodes } from "../../lib/published";
import { storeId, storeName, tenantId } from "../../state/session";

/** The card: publish to the store in context, and what the last publish carried. */
export function PublishIntegrations(props: {
  connections: readonly { readonly updated_at_ms?: number }[] | null | undefined;
}) {
  const published = usePublishedNodes();
  const publishing = useEntityCrud<never>();
  const [last, setLast] = createSignal<IntegrationsPublished | null>(null);
  const publish = () => {
    void publishing
      .run(async () => {
        const result = await publishIntegrations(tenantId(), storeId());
        setLast(result);
        toast.ok(
          t("integrations.published", {
            connections: String(result.integrations),
            vendors: String(result.vendors),
          }),
        );
        await published.refresh();
      })
      .then((ok) => {
        if (!ok) {
          // A delivery connection without its choices is named in the refusal's details.
          toast.error(publishing.error());
        }
      });
  };
  return (
    <Card title={t("integrations.publishTitle")}>
      <p class="mb-3 text-sm text-ink-muted">{t("integrations.publishHint")}</p>
      <div class="flex flex-col gap-3" data-outcome="integrations-publish">
        <PublishBar
          label={t("integrations.publishTo", { store: storeName() })}
          publishedAtMs={published.publishedAtMs("integrations")}
          editedAtMs={lastEditedMs(props.connections)}
          describe={describePublish}
          publishLabel={t("integrations.publish")}
          busy={publishing.saving()}
          disabled={!storeId()}
          disabledReason={t("integrations.publishNeedsStore")}
          onPublish={publish}
        />
        <Show when={last()}>
          {(result) => (
            <p class="text-sm" data-outcome="integrations-published">
              {t("integrations.published", {
                connections: String(result().integrations),
                vendors: String(result().vendors),
              })}
            </p>
          )}
        </Show>
      </div>
    </Card>
  );
}
