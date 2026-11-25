// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

// The guest page (ADR-0224 decision 2): a guest at a table reads the menu and sends an order. It
// asks for nothing personal and keeps the order in this browser tab until it is sent. The table's
// signed token is in the address's `#t=` fragment, which a browser never sends to a server, so it
// reaches the cloud only as the menu read's and the order's own credential.
//
// Everything drawn is built from DOM nodes and `textContent`: no markup is ever made from text the
// server or the address carried. Every string goes through a translation key.

"use strict";

(() => {
  const MAX_LINES = 30;
  const MAX_QUANTITY = 20;
  const LANGUAGES = ["en", "vi"];
  const app = document.getElementById("app");

  let strings = {};
  let menu = null;
  let cart = [];
  let reference = null;
  let sending = false;

  const token = (() => {
    const match = /^#t=([A-Za-z0-9._-]{1,512})$/.exec(window.location.hash);
    return match ? match[1] : null;
  })();
  const tag = /^[A-Za-z0-9-]{1,35}$/.test(navigator.language || "") ? navigator.language : "en";
  const uiLanguage = LANGUAGES.includes(tag.split("-")[0]) ? tag.split("-")[0] : "en";
  let menuLanguage = tag;

  function t(key, values) {
    let text = typeof strings[key] === "string" ? strings[key] : key;
    for (const [name, value] of Object.entries(values || {})) {
      text = text.split(`{${name}}`).join(String(value));
    }
    return text;
  }

  function el(tagName, props, children) {
    const node = document.createElement(tagName);
    for (const [name, value] of Object.entries(props || {})) {
      if (name === "text") node.textContent = value;
      else if (name === "onClick") node.addEventListener("click", value);
      else node.setAttribute(name, value);
    }
    for (const child of children || []) if (child) node.appendChild(child);
    return node;
  }

  function show(...nodes) {
    app.replaceChildren(...nodes);
  }

  function message(key, isError) {
    show(el("p", { class: isError ? "error" : "muted", text: t(key) }));
  }

  // Display only: the integer minor units the server sent, shown in the currency's own digits.
  function price(money) {
    try {
      const format = new Intl.NumberFormat(uiLanguage, { style: "currency", currency: money.currency_code });
      const digits = format.resolvedOptions().maximumFractionDigits;
      return format.format(money.amount_minor / 10 ** digits);
    } catch {
      return String(money.amount_minor);
    }
  }

  function itemById(id) {
    return menu.items.find((item) => item.menu_item_id === id);
  }

  function lineTotal(line) {
    const base = itemById(line.menu_item_id);
    const unit = line.modifier_menu_item_ids.reduce(
      (sum, id) => sum + (itemById(id)?.unit_price.amount_minor ?? 0),
      base.unit_price.amount_minor,
    );
    return unit * line.quantity;
  }

  function saveCart() {
    try {
      window.sessionStorage.setItem(`cart:${token}`, JSON.stringify({ cart, reference }));
    } catch {
      // A browser that keeps nothing keeps the order in this page alone.
    }
  }

  function loadCart() {
    try {
      const saved = JSON.parse(window.sessionStorage.getItem(`cart:${token}`) || "null");
      if (saved && Array.isArray(saved.cart)) {
        cart = saved.cart
          .filter((line) => itemById(line.menu_item_id) && Number.isInteger(line.quantity))
          .filter((line) => line.quantity >= 1 && line.quantity <= MAX_QUANTITY)
          .filter((line) => Array.isArray(line.modifier_menu_item_ids))
          .slice(0, MAX_LINES);
        reference = typeof saved.reference === "string" ? saved.reference : null;
      }
    } catch {
      cart = [];
    }
  }

  function add(item, modifiers) {
    const key = JSON.stringify([item.menu_item_id, [...modifiers].sort()]);
    const same = cart.find((line) => JSON.stringify([line.menu_item_id, [...line.modifier_menu_item_ids].sort()]) === key);
    if (same) same.quantity = Math.min(MAX_QUANTITY, same.quantity + 1);
    else if (cart.length < MAX_LINES) cart.push({ menu_item_id: item.menu_item_id, quantity: 1, modifier_menu_item_ids: modifiers });
    saveCart();
    render();
  }

  function choices(item, row) {
    for (const open of app.querySelectorAll(".choices")) open.remove();
    const groups = item.modifier_group_ids
      .map((id) => menu.modifier_groups.find((group) => group.modifier_group_id === id))
      .filter(Boolean);
    if (groups.length === 0) return add(item, []);
    const picked = new Set();
    const confirm = el("button", { class: "primary", text: t("guest.add") });
    const valid = () =>
      groups.every((group) => {
        const count = group.member_menu_item_ids.filter((id) => picked.has(id)).length;
        return count >= group.min_select && count <= group.max_select;
      });
    const sets = groups.map((group) =>
      el("fieldset", {}, [
        el("legend", { text: `${group.name} · ${t("guest.choices_count", { min: group.min_select, max: group.max_select })}` }),
        ...group.member_menu_item_ids
          .map(itemById)
          .filter((member) => member && member.available)
          .map((member) => {
            const box = el("input", { type: "checkbox" });
            box.addEventListener("change", () => {
              if (box.checked) picked.add(member.menu_item_id);
              else picked.delete(member.menu_item_id);
              confirm.disabled = !valid();
            });
            return el("label", {}, [box, el("span", { text: `${member.name} +${price(member.unit_price)}` })]);
          }),
      ]),
    );
    confirm.disabled = !valid();
    confirm.addEventListener("click", () => add(item, [...picked]));
    row.after(el("div", { class: "choices" }, [...sets, confirm]));
  }

  function render() {
    const items = menu.items.filter((item) => !menu.modifier_groups.some((group) => group.member_menu_item_ids.includes(item.menu_item_id)));
    const languages = el("div", { class: "lang" }, menu.languages.map((language) =>
      // The menu answers in the language asked for, such as `en-US`, and offers bare ones, such as
      // `en`: the button of the language the menu is in is the pressed one either way.
      el("button", { text: language, "aria-pressed": String(language === menu.language.split("-")[0] || language === menu.language), onClick: () => load(language) }),
    ));
    const list = items.map((item) => {
      const row = el("div", { class: "item", "aria-disabled": String(!item.available) }, [
        el("span", { text: item.name }),
        el("span", { class: "muted", text: item.available ? price(item.unit_price) : t("guest.sold_out") }),
      ]);
      const button = el("button", { text: t("guest.add"), onClick: () => choices(item, row) });
      button.disabled = !item.available || cart.length >= MAX_LINES;
      row.appendChild(button);
      return row;
    });
    const lines = cart.map((line, index) => {
      const item = itemById(line.menu_item_id);
      const names = [item.name, ...line.modifier_menu_item_ids.map((id) => itemById(id)?.name ?? "")].join(" + ");
      const fewer = el("button", { text: "−", "aria-label": t("guest.decrease", { name: item.name }), onClick: () => {
        line.quantity -= 1;
        if (line.quantity < 1) cart.splice(index, 1);
        saveCart();
        render();
      } });
      const more = el("button", { text: "+", "aria-label": t("guest.increase", { name: item.name }), onClick: () => {
        line.quantity = Math.min(MAX_QUANTITY, line.quantity + 1);
        saveCart();
        render();
      } });
      return el("div", { class: "line" }, [el("span", { text: `${line.quantity} × ${names}` }), el("span", {}, [fewer, more])]);
    });
    const total = cart.reduce((sum, line) => sum + lineTotal(line), 0);
    const currency = menu.items[0]?.unit_price.currency_code;
    const send = el("button", { class: "primary", text: sending ? t("guest.sending") : t("guest.send"), onClick: submit });
    send.disabled = sending || cart.length === 0;
    // `null` stands for a part not shown, and is left out: `replaceChildren` would draw it as text.
    show(...[
      languages,
      el("h1", { text: t("guest.menu") }),
      ...(list.length ? list : [el("p", { class: "muted", text: t("guest.menu_empty") })]),
      el("h2", { text: t("guest.cart") }),
      ...(lines.length ? lines : [el("p", { class: "muted", text: t("guest.cart_empty") })]),
      cart.length >= MAX_LINES ? el("p", { class: "muted", text: t("guest.cart_full") }) : null,
      el("p", { class: "muted", text: `${t("guest.total_note")} ${t("guest.pay_at_counter")}` }),
      el("div", { class: "bar" }, [el("div", {}, [
        el("span", { text: currency ? `${t("guest.estimated_total")}: ${price({ currency_code: currency, amount_minor: total })}` : "" }),
        send,
      ])]),
    ].filter((node) => node !== null));
  }

  const REFUSALS = {
    PERMISSION_DENIED: "guest.no_table",
    NOT_FOUND: "guest.no_ordering",
    UNAVAILABLE: "guest.offline",
    FAILED_PRECONDITION: "guest.closed",
    RESOURCE_EXHAUSTED: "guest.too_many",
    INVALID_ARGUMENT: "guest.invalid",
  };

  async function refusalOf(response) {
    try {
      const body = await response.json();
      return REFUSALS[body?.error?.status] || "guest.error";
    } catch {
      return "guest.error";
    }
  }

  async function submit() {
    if (sending || cart.length === 0) return;
    sending = true;
    reference = reference || window.crypto.randomUUID();
    saveCart();
    render();
    try {
      const response = await fetch("/v1/qr/orders", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          table_token: token,
          external_reference: reference,
          lines: cart.map((line) => ({
            menu_item_id: line.menu_item_id,
            quantity_milli: line.quantity * 1000,
            modifier_menu_item_ids: line.modifier_menu_item_ids,
          })),
          placed_at_ms: Date.now(),
        }),
      });
      sending = false;
      if (!response.ok) {
        const key = await refusalOf(response);
        render();
        app.prepend(el("p", { class: "error", role: "alert", text: t(key) }));
        return;
      }
      cart = [];
      reference = null;
      saveCart();
      show(
        el("p", { text: t("guest.sent") }),
        el("p", { class: "muted", text: t("guest.pay_at_counter") }),
        el("button", { class: "primary", text: t("guest.order_more"), onClick: render }),
      );
    } catch {
      sending = false;
      render();
      app.prepend(el("p", { class: "error", role: "alert", text: t("guest.offline") }));
    }
  }

  async function load(language) {
    menuLanguage = language;
    const query = new URLSearchParams({ table_token: token, lang: menuLanguage });
    const response = await fetch(`/v1/qr/menu?${query}`).catch(() => null);
    if (!response) return message("guest.offline", true);
    if (!response.ok) return message(await refusalOf(response), true);
    menu = await response.json();
    loadCart();
    render();
  }

  async function start() {
    document.documentElement.lang = uiLanguage;
    try {
      const response = await fetch(`i18n/${uiLanguage}.json`);
      strings = response.ok ? await response.json() : {};
    } catch {
      strings = {};
    }
    document.title = t("guest.menu");
    if (!token) return message("guest.no_table", true);
    message("guest.loading", false);
    await load(menuLanguage);
  }

  start();
})();
