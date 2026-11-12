// A screen the running package version adds (ADR-0190 decision 1): one whose `screen_id` the
// release's default package does not have, so no screen of the till hosts it. It is drawn at
// `/screen/:id`, where an `ui.screen.open` act opens it (`../components/ScreenReactions.tsx`), with
// no regions and the host every package screen gets. A screen of the release's own is drawn at its
// own route, never here; an id the version does not declare draws the slot that says the till
// needs an update.

import { Show } from "solid-js";
import { useParams } from "@solidjs/router";

import { NeedsUpdate } from "../package/render";
import { NO_ACTS, PackageScreen, defaultScreen } from "../package/screen";

export function VersionScreen() {
  const params = useParams<{ id: string }>();
  return (
    <Show when={defaultScreen(params.id) === undefined} fallback={<NeedsUpdate />}>
      <PackageScreen screenId={params.id} host={NO_ACTS} />
    </Show>
  );
}
