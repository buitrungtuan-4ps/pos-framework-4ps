// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

// The status page: what the app's monitor last learned about the edge, redrawn every three seconds
// from the `status` command. Every label is a key built from the edge's own wire tokens
// (`status.cloud.CLOUD_LINK_ONLINE`, …), so a token this page does not know shows as its key.

"use strict";

(function () {
  applyStrings(document);
  reportLanguage();

  const byId = (id) => document.getElementById(id);

  function renderPrinters(printers) {
    const cell = byId("printers");
    cell.replaceChildren();
    if (printers.length === 0) {
      cell.textContent = t("status.printers.none");
      return;
    }
    const list = document.createElement("ul");
    for (const printer of printers) {
      const item = document.createElement("li");
      item.textContent = printer.name;
      list.append(item);
    }
    cell.append(list);
  }

  function render(view) {
    const { edge, pairing, store } = view.snapshot;
    byId("mode").textContent = t(`status.mode.${view.mode}`, { origin: view.edge_origin || "" });
    byId("edge").textContent = t(`status.edge.${edge.state}`, { version: edge.version || "" });
    byId("pairing").textContent = t(`status.pairing.${pairing}`);
    byId("unsaved").hidden = view.pairing_saved;
    if (store.state === "STORE_READ") {
      byId("cloud").textContent = t(`status.cloud.${store.cloud_link}`);
      const level = t(`status.outbox.${store.outbox_level}`);
      const depth = t("status.outbox.value", {
        depth: store.outbox_depth,
        planned: store.outbox_planned_depth,
      });
      byId("outbox").textContent = level === "" ? depth : `${depth} · ${level}`;
      renderPrinters(store.printers);
    } else {
      byId("cloud").textContent = t(`status.not_read.${store.reason}`);
      byId("outbox").textContent = t("status.unknown");
      byId("printers").textContent = t("status.unknown");
    }
    const agent = view.print_agent;
    byId("agent-label").hidden = !agent;
    byId("agent").hidden = !agent;
    if (agent) {
      byId("agent").textContent = t(`status.agent.${agent.state}`, {
        restarts: agent.restarts ?? 0,
        seconds: agent.retry_in_seconds ?? 0,
      });
    }
    byId("version").textContent = t("status.app_version", { version: view.app_version });
  }

  async function refresh() {
    try {
      render(await invoke("status"));
    } catch {
      // Keep the last drawing; the next refresh tries again.
    }
  }

  refresh();
  setInterval(refresh, 3000);
})();
