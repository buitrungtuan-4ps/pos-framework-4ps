// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

// The bundled pages' strings, in English and Vietnamese, and the three helpers both pages use.
//
// The markup carries only keys (`data-i18n`, `data-i18n-placeholder`); `applyStrings` fills them in
// the language `navigator.language` names — the operating system's, as the webview sees it. A key
// with no entry shows as itself, which is ugly on purpose: a missing string should be noticed.
//
// Keys the Rust side sends (a refusal from `pair`, the lost-pairing notice) are held to this file by
// a test in `src-tauri/src/commands.rs`, which expects each exactly once per language.
//
// `invoke` reaches the app's commands through `__TAURI_INTERNALS__`, which Tauri injects into every
// page. The app keeps `withGlobalTauri` off so the till — a remote page — gets no `__TAURI__` API
// object; the capability file is what actually stops it invoking anything.

"use strict";

const STRINGS = {
  en: {
    "connect.title": "Connect to the store",
    "connect.lead":
      "Pair this computer with the store server. You need its address and a six-digit pairing code: a manager gets one on the till's Devices screen, and the store server shows one when it starts.",
    "connect.address_label": "Store server address or pairing link",
    "connect.address_placeholder": "192.168.1.10:8787",
    "connect.address_hint":
      "For example 192.168.1.10:8787. Or paste the whole link from the pairing QR — its code comes with it.",
    "connect.code_label": "Pairing code",
    "connect.code_placeholder": "123456",
    "connect.code_hint": "Six digits. Leave it empty if you pasted the pairing link.",
    "connect.submit": "Connect",
    "connect.working": "Connecting…",
    "connect.notice.pairing_lost": "The store no longer accepts this computer. Enter a new pairing code.",
    "connect.error.address_empty": "Enter the store server's address.",
    "connect.error.address_scheme": "The address must start with http:// or https://, or have no prefix at all.",
    "connect.error.address_credentials": "The address must not contain a user name or password.",
    "connect.error.address_host": "That is not a valid address. Use host:port, for example 192.168.1.10:8787.",
    "connect.error.address_port": "The port must be a number from 1 to 65535.",
    "connect.error.code_missing": "Enter the six-digit pairing code.",
    "connect.error.code_format": "The pairing code is six digits.",
    "connect.error.code_rejected":
      "The store did not accept that code. It may have expired or been used already — ask for a new one.",
    "connect.error.too_many": "Too many wrong codes. Wait {seconds} seconds and try again.",
    "connect.error.too_many_later": "Too many wrong codes. Wait a moment and try again.",
    "connect.error.unavailable": "The store server cannot pair a device right now. Try again in a moment.",
    "connect.error.unreachable":
      "No store server answered at that address. Check the address, and that this computer is on the store's network.",
    "status.pairing_not_saved":
      "This computer's credential store refused the pairing, so it lasts only until POS Station quits. After a restart, pair again.",
    "error.not_allowed": "This page may not do that.",
    "error.unexpected": "Something unexpected happened. Try again.",
    "status.title": "Store status",
    "status.loading": "Loading…",
    "status.unknown": "—",
    "status.mode.MODE_STATION": "This computer runs the store server.",
    "status.mode.MODE_TERMINAL": "This computer is a till for the store server at {origin}.",
    "status.edge": "Store server",
    "status.edge.EDGE_UNSPECIFIED": "Checking…",
    "status.edge.EDGE_UP": "Running, version {version}",
    "status.edge.EDGE_DOWN": "Not responding",
    "status.pairing": "Pairing",
    "status.pairing.PAIRING_UNSPECIFIED": "Not confirmed yet",
    "status.pairing.PAIRING_PAIRED": "Paired",
    "status.pairing.PAIRING_LOST": "No longer paired",
    "status.cloud": "Cloud link",
    "status.cloud.CLOUD_LINK_ONLINE": "Connected",
    "status.cloud.CLOUD_LINK_OFFLINE": "Offline — selling normally",
    "status.cloud.CLOUD_LINK_UNSPECIFIED": "Not tried yet",
    "status.outbox": "Waiting to sync",
    "status.outbox.value": "{depth} of {planned} planned",
    "status.outbox.OUTBOX_LEVEL_NORMAL": "normal",
    "status.outbox.OUTBOX_LEVEL_ELEVATED": "behind: half the planned backlog",
    "status.outbox.OUTBOX_LEVEL_HIGH": "far behind: 80% of the planned backlog",
    "status.outbox.OUTBOX_LEVEL_BEYOND": "past the planned backlog",
    "status.outbox.OUTBOX_LEVEL_UNSPECIFIED": "",
    "status.printers": "Printers",
    "status.printers.none": "The store has published no printers.",
    "status.printers.note":
      "The store server publishes which printers exist, not whether each one is switched on.",
    "status.not_read.NOT_READ_TILL_CLOSED": "Open the till to see the cloud link, the backlog and the printers.",
    "status.not_read.NOT_READ_SIGN_IN_NEEDED":
      "Sign in on the till to see the cloud link, the backlog and the printers.",
    "status.not_read.NOT_READ_EDGE_DOWN": "Unknown while the store server is not responding.",
    "status.not_read.NOT_READ_NOT_PAIRED": "Unknown until this computer is paired.",
    "status.not_read.NOT_READ_FAILED": "The store server's answer could not be read.",
    "status.agent": "Print agent",
    "status.agent.PRINT_AGENT_STARTING": "Starting",
    "status.agent.PRINT_AGENT_RUNNING": "Running · restarts: {restarts}",
    "status.agent.PRINT_AGENT_WAITING": "Stopped; starting again in {seconds} s · restarts: {restarts}",
    "status.agent.PRINT_AGENT_MISSING": "Not installed with this build",
    "status.agent.PRINT_AGENT_STOPPED": "Stopped",
    "status.app_version": "POS Station {version}",
  },
  vi: {
    "connect.title": "Kết nối với cửa hàng",
    "connect.lead":
      "Ghép nối máy tính này với máy chủ cửa hàng. Bạn cần địa chỉ của máy chủ và mã ghép nối sáu chữ số: quản lý lấy mã trên màn hình Thiết bị của máy bán hàng, và máy chủ cửa hàng hiện một mã khi khởi động.",
    "connect.address_label": "Địa chỉ máy chủ cửa hàng hoặc link ghép nối",
    "connect.address_placeholder": "192.168.1.10:8787",
    "connect.address_hint":
      "Ví dụ 192.168.1.10:8787. Hoặc dán cả link từ mã QR ghép nối — mã ghép nối đi kèm theo link.",
    "connect.code_label": "Mã ghép nối",
    "connect.code_placeholder": "123456",
    "connect.code_hint": "Sáu chữ số. Để trống nếu bạn đã dán link ghép nối.",
    "connect.submit": "Kết nối",
    "connect.working": "Đang kết nối…",
    "connect.notice.pairing_lost": "Cửa hàng không còn chấp nhận máy tính này. Hãy nhập mã ghép nối mới.",
    "connect.error.address_empty": "Hãy nhập địa chỉ máy chủ cửa hàng.",
    "connect.error.address_scheme": "Địa chỉ phải bắt đầu bằng http:// hoặc https://, hoặc không có tiền tố nào.",
    "connect.error.address_credentials": "Địa chỉ không được chứa tên đăng nhập hoặc mật khẩu.",
    "connect.error.address_host": "Địa chỉ không hợp lệ. Hãy nhập dạng máy:cổng, ví dụ 192.168.1.10:8787.",
    "connect.error.address_port": "Cổng phải là một số từ 1 đến 65535.",
    "connect.error.code_missing": "Hãy nhập mã ghép nối sáu chữ số.",
    "connect.error.code_format": "Mã ghép nối gồm sáu chữ số.",
    "connect.error.code_rejected":
      "Cửa hàng không chấp nhận mã này. Mã có thể đã hết hạn hoặc đã được dùng — hãy lấy mã mới.",
    "connect.error.too_many": "Nhập sai quá nhiều lần. Hãy đợi {seconds} giây rồi thử lại.",
    "connect.error.too_many_later": "Nhập sai quá nhiều lần. Hãy đợi một lát rồi thử lại.",
    "connect.error.unavailable": "Máy chủ cửa hàng tạm thời không ghép nối được thiết bị. Hãy thử lại sau giây lát.",
    "connect.error.unreachable":
      "Không có máy chủ cửa hàng nào phản hồi ở địa chỉ này. Hãy kiểm tra địa chỉ và chắc rằng máy tính này đang ở trong mạng của cửa hàng.",
    "status.pairing_not_saved":
      "Kho thông tin đăng nhập của máy tính này không lưu được việc ghép nối, nên nó chỉ có hiệu lực đến khi thoát POS Station. Sau khi khởi động lại, hãy ghép nối lại.",
    "error.not_allowed": "Trang này không được phép làm việc đó.",
    "error.unexpected": "Đã xảy ra lỗi không mong muốn. Hãy thử lại.",
    "status.title": "Trạng thái cửa hàng",
    "status.loading": "Đang tải…",
    "status.unknown": "—",
    "status.mode.MODE_STATION": "Máy tính này chạy máy chủ cửa hàng.",
    "status.mode.MODE_TERMINAL": "Máy tính này là máy bán hàng của máy chủ cửa hàng tại {origin}.",
    "status.edge": "Máy chủ cửa hàng",
    "status.edge.EDGE_UNSPECIFIED": "Đang kiểm tra…",
    "status.edge.EDGE_UP": "Đang chạy, phiên bản {version}",
    "status.edge.EDGE_DOWN": "Không phản hồi",
    "status.pairing": "Ghép nối",
    "status.pairing.PAIRING_UNSPECIFIED": "Chưa xác nhận",
    "status.pairing.PAIRING_PAIRED": "Đã ghép nối",
    "status.pairing.PAIRING_LOST": "Không còn được ghép nối",
    "status.cloud": "Kết nối cloud",
    "status.cloud.CLOUD_LINK_ONLINE": "Đã kết nối",
    "status.cloud.CLOUD_LINK_OFFLINE": "Mất kết nối — vẫn bán bình thường",
    "status.cloud.CLOUD_LINK_UNSPECIFIED": "Chưa thử kết nối",
    "status.outbox": "Chờ đồng bộ",
    "status.outbox.value": "{depth} trên mức dự kiến {planned}",
    "status.outbox.OUTBOX_LEVEL_NORMAL": "bình thường",
    "status.outbox.OUTBOX_LEVEL_ELEVATED": "chậm: đã đến một nửa mức dự kiến",
    "status.outbox.OUTBOX_LEVEL_HIGH": "rất chậm: đã đến 80% mức dự kiến",
    "status.outbox.OUTBOX_LEVEL_BEYOND": "đã vượt mức dự kiến",
    "status.outbox.OUTBOX_LEVEL_UNSPECIFIED": "",
    "status.printers": "Máy in",
    "status.printers.none": "Cửa hàng chưa có máy in nào.",
    "status.printers.note":
      "Máy chủ cửa hàng chỉ cho biết những máy in đã được cấu hình, không cho biết máy nào đang bật.",
    "status.not_read.NOT_READ_TILL_CLOSED": "Mở máy bán hàng để xem kết nối cloud, số sự kiện chờ và máy in.",
    "status.not_read.NOT_READ_SIGN_IN_NEEDED":
      "Đăng nhập trên máy bán hàng để xem kết nối cloud, số sự kiện chờ và máy in.",
    "status.not_read.NOT_READ_EDGE_DOWN": "Chưa rõ khi máy chủ cửa hàng không phản hồi.",
    "status.not_read.NOT_READ_NOT_PAIRED": "Chưa rõ cho đến khi máy tính này được ghép nối.",
    "status.not_read.NOT_READ_FAILED": "Không đọc được phản hồi của máy chủ cửa hàng.",
    "status.agent": "Chương trình in",
    "status.agent.PRINT_AGENT_STARTING": "Đang khởi động",
    "status.agent.PRINT_AGENT_RUNNING": "Đang chạy · số lần khởi động lại: {restarts}",
    "status.agent.PRINT_AGENT_WAITING":
      "Đã dừng; khởi động lại sau {seconds} giây · số lần khởi động lại: {restarts}",
    "status.agent.PRINT_AGENT_MISSING": "Bản cài đặt này không có chương trình in",
    "status.agent.PRINT_AGENT_STOPPED": "Đã dừng",
    "status.app_version": "POS Station {version}",
  },
};

const LANG = (navigator.language || "en").toLowerCase().startsWith("vi") ? "vi" : "en";

// The string for `key`, with each `{name}` replaced from `values`.
function t(key, values) {
  const table = STRINGS[LANG];
  let text = Object.prototype.hasOwnProperty.call(table, key) ? table[key] : key;
  for (const [name, value] of Object.entries(values || {})) {
    text = text.split(`{${name}}`).join(String(value));
  }
  return text;
}

// Fills every `data-i18n` element's text and every `data-i18n-placeholder` input's placeholder.
function applyStrings(root) {
  document.documentElement.lang = LANG;
  for (const element of root.querySelectorAll("[data-i18n]")) {
    element.textContent = t(element.dataset.i18n);
  }
  for (const element of root.querySelectorAll("[data-i18n-placeholder]")) {
    element.placeholder = t(element.dataset.i18nPlaceholder);
  }
}

// Calls one of the app's commands. Rejects with the command's refusal (`{ key, … }`).
function invoke(command, args) {
  return window.__TAURI_INTERNALS__.invoke(command, args || {});
}

// Tells the app the webview's language, so the tray and notifications use it too.
function reportLanguage() {
  invoke("set_language", { language: navigator.language || "en" }).catch(() => {});
}
