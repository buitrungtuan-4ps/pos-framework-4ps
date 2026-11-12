// A tenant's gateway endpoints (ADR-0198): the https targets a store's lookups go to through the
// cloud. Secret headers are write-only: sent once, sealed by the cloud, never returned. Typed from
// the generated OpenAPI schema (ADR-0177).

import { requestJson, requestJsonIfMatch, requestVoid } from "./client";
import type { GatewayEndpoint, GatewayEndpointRequest } from "./schemas";
import type { ETag } from "./types";

/** What the screen sends: the request but the tenant, which the module adds. */
export type GatewayEndpointInput = Omit<GatewayEndpointRequest, "tenant_id">;

const ROOT = "/admin/integrations/gateway-endpoints";

/** `GET`: the tenant's endpoints, in id order. */
export const listGatewayEndpoints = (tenantId: string): Promise<GatewayEndpoint[]> =>
  requestJson<GatewayEndpoint[]>("GET", `${ROOT}?tenant_id=${encodeURIComponent(tenantId)}`);

/** `POST`: a new endpoint, its id minted by the server. */
export const createGatewayEndpoint = (tenantId: string, input: GatewayEndpointInput): Promise<GatewayEndpoint> =>
  requestJson<GatewayEndpoint>("POST", ROOT, { tenant_id: tenantId, ...input });

/** `PUT`, at the version it was read at (ADR-0094). */
export const updateGatewayEndpoint = (
  tenantId: string,
  id: string,
  etag: ETag,
  input: GatewayEndpointInput,
): Promise<GatewayEndpoint> =>
  requestJsonIfMatch<GatewayEndpoint>("PUT", `${ROOT}/${encodeURIComponent(id)}`, etag, {
    tenant_id: tenantId,
    ...input,
  });

/** `DELETE`: the endpoint and its sealed headers. */
export const deleteGatewayEndpoint = (tenantId: string, id: string): Promise<void> =>
  requestVoid("DELETE", `${ROOT}/${encodeURIComponent(id)}?tenant_id=${encodeURIComponent(tenantId)}`);
