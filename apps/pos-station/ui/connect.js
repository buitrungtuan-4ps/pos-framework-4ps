// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

// The connect page: an edge address (or a pasted pairing link) and a six-digit code, handed to the
// app's `pair` command. On success the app opens the till already paired and closes this window, so
// there is no success state to draw here. Every refusal arrives as a key into `i18n.js`.

"use strict";

(function () {
  applyStrings(document);
  reportLanguage();

  const form = document.getElementById("form");
  const address = document.getElementById("address");
  const code = document.getElementById("code");
  const submit = document.getElementById("submit");
  const error = document.getElementById("error");
  const notice = document.getElementById("notice");

  const show = (element, text) => {
    element.textContent = text;
    element.hidden = text === "";
  };

  invoke("connect_context")
    .then((context) => {
      if (context.address && address.value === "") {
        address.value = context.address;
      }
      if (context.notice) {
        show(notice, t(context.notice));
      }
    })
    .catch(() => {})
    .finally(() => (address.value === "" ? address : code).focus());

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    show(error, "");
    submit.disabled = true;
    submit.textContent = t("connect.working");
    try {
      await invoke("pair", { address: address.value, code: code.value });
    } catch (refusal) {
      const key = refusal && typeof refusal.key === "string" ? refusal.key : "error.unexpected";
      const seconds = refusal ? refusal.retry_after_seconds : undefined;
      if (key === "connect.error.too_many" && seconds === undefined) {
        show(error, t("connect.error.too_many_later"));
      } else {
        show(error, t(key, { seconds }));
      }
      submit.disabled = false;
      submit.textContent = t("connect.submit");
    }
  });
})();
