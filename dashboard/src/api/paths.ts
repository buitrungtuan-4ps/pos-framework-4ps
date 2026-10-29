// The `/admin` routes the console may call, as types drawn from the generated OpenAPI paths
// (ADR-0177 decision 3). Types only: nothing here exists at run time, so the client gains no
// dependency and no byte.
//
// `AdminCall` is every `[method, url]` pair the document declares, with each `{param}` of a route
// opened to any text and any query string allowed after it. `client.ts` types each helper's
// `method` and `path` with it, so a route that is not in the document — a typo, a removed route,
// a method the route does not answer — fails `tsc` instead of reaching the cloud.

import type { paths } from "./generated/admin";

/** A route as the document names it, `{param}` placeholders and all. */
export type AdminPath = keyof paths;

/** The methods a route can declare, as the document spells them. */
type OperationKey = "get" | "put" | "post" | "delete" | "patch";

/** An HTTP method, as `fetch` takes it. */
export type AdminMethod = Uppercase<OperationKey>;

/** `route` with each `{param}` opened to any text, so a filled-in URL matches it. */
type Filled<Route extends string> = Route extends `${infer Head}{${string}}${infer Tail}`
  ? `${Head}${string}${Filled<Tail>}`
  : Route;

/** A URL that calls `route`: its params filled in, with or without a query string. */
export type AdminUrl<Route extends AdminPath = AdminPath> =
  | Filled<Route>
  | `${Filled<Route>}?${string}`;

/** Whether `route` declares an operation for `key`. */
type Declares<Route extends AdminPath, Key extends OperationKey> = paths[Route][Key] extends
  | undefined
  | never
  ? false
  : true;

/** Every `[method, url]` pair the document declares. */
export type AdminCall = {
  [Route in AdminPath]: {
    [Key in OperationKey]: Declares<Route, Key> extends true
      ? [Uppercase<Key>, AdminUrl<Route>]
      : never;
  }[OperationKey];
}[AdminPath];

/** A URL that `method` may be sent to. */
export type AdminUrlFor<Method extends AdminMethod> = Extract<AdminCall, [Method, string]>[1];
