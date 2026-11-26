// Publishing a store's integrations (phase 10 open item O5), apart from `client.ts`, which is at its
// size limit, as the package routes are in `packages.ts`.

import { requestJson } from "./client";
import type { IntegrationsPublished } from "./schemas";

/**
 * Publishes the connections that serve one store as its `integrations` node, and its delivery
 * connections as its `vendors` node (ADR-0215 decision 5).
 */
export const publishIntegrations = (tenantId: string, storeId: string) =>
  requestJson<IntegrationsPublished>("PUT", "/admin/config/integrations", {
    tenant_id: tenantId,
    store_id: storeId,
  });
