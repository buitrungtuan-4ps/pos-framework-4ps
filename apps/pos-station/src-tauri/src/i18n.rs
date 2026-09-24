// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The strings the native side shows — tray, window titles, notifications — in English and
//! Vietnamese.
//!
//! The bundled pages carry their own table (`ui/i18n.js`); the two share no key, so neither is a
//! copy of the other. Here a key is an enum and each arm holds both languages side by side, so a
//! string added in one language and not the other does not compile.
//!
//! The language is the one the bundled pages report from `navigator.language` — the operating
//! system's, as the webview sees it — remembered in the app's settings, with `LC_ALL`, `LC_MESSAGES`
//! and `LANG` as the fallback before any page has run.

use crate::health::{CloudLink, EdgeReading, NotRead, Notice, OutboxLevel, Snapshot, StoreReading};
use crate::sidecar::AgentStatus;

/// A language the app speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    /// English, the fallback.
    En,
    /// Vietnamese.
    Vi,
}

impl Lang {
    /// From a BCP 47 tag such as `vi-VN` or `en-US`, or a POSIX locale such as `vi_VN.UTF-8`.
    pub(crate) fn from_tag(tag: &str) -> Self {
        if tag.trim().to_ascii_lowercase().starts_with("vi") {
            Self::Vi
        } else {
            Self::En
        }
    }

    /// From the POSIX locale variables, before any page has reported the webview's language.
    pub(crate) fn from_environment() -> Self {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.trim().is_empty())
            .map_or(Self::En, |value| Self::from_tag(&value))
    }

    /// The tag stored in the settings.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Vi => "vi",
        }
    }

    fn pick(self, en: &'static str, vi: &'static str) -> &'static str {
        match self {
            Self::En => en,
            Self::Vi => vi,
        }
    }
}

/// A fixed native string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    /// The app's name in the tray and the notification source.
    AppName,
    /// Tray menu: open the till.
    MenuOpenTill,
    /// Tray menu: open the status page.
    MenuStatus,
    /// Tray menu: open the pairing QR (the till's Devices screen).
    MenuPairingQr,
    /// Tray menu: quit.
    MenuQuit,
    /// Window title: the till.
    WindowTill,
    /// Window title: the status page.
    WindowStatus,
    /// Window title: the connect page.
    WindowConnect,
    /// Window title: the pairing QR.
    WindowPairingQr,
}

/// The text for `key` in `lang`.
pub(crate) fn text(lang: Lang, key: Key) -> &'static str {
    match key {
        Key::AppName => lang.pick("POS Station", "POS Station"),
        Key::MenuOpenTill => lang.pick("Open till", "Mở máy bán hàng"),
        Key::MenuStatus => lang.pick("Status", "Trạng thái"),
        Key::MenuPairingQr => lang.pick("Pairing QR", "Mã QR ghép nối"),
        Key::MenuQuit => lang.pick("Quit", "Thoát"),
        Key::WindowTill => lang.pick("Till", "Máy bán hàng"),
        Key::WindowStatus => lang.pick("Store status", "Trạng thái cửa hàng"),
        Key::WindowConnect => lang.pick("Connect to the store", "Kết nối với cửa hàng"),
        Key::WindowPairingQr => lang.pick("Pair a device", "Ghép nối thiết bị"),
    }
}

/// The one-line summary the tray shows: its tooltip where the platform has one, and the first,
/// disabled menu item everywhere (Linux's `AppIndicator` shows no tooltip).
pub(crate) fn summary(lang: Lang, snapshot: &Snapshot, agent: Option<&AgentStatus>) -> String {
    let mut parts = vec![edge_part(lang, &snapshot.edge)];
    match &snapshot.store {
        StoreReading::Read(facts) => {
            parts.push(cloud_part(lang, facts.cloud_link));
            parts.push(
                lang.pick("Waiting to sync: {n}", "Chờ đồng bộ: {n}")
                    .replace("{n}", &facts.outbox_depth.to_string()),
            );
            parts.push(
                lang.pick("Printers: {n}", "Máy in: {n}")
                    .replace("{n}", &facts.printers.len().to_string()),
            );
        }
        StoreReading::NotRead { reason } => parts.push(not_read_part(lang, *reason).to_owned()),
    }
    if let Some(agent) = agent {
        parts.push(agent_part(lang, agent));
    }
    parts.join(" · ")
}

fn edge_part(lang: Lang, edge: &EdgeReading) -> String {
    match edge {
        EdgeReading::Unknown => lang
            .pick(
                "Store server: checking…",
                "Máy chủ cửa hàng: đang kiểm tra…",
            )
            .to_owned(),
        EdgeReading::Up { version } => lang
            .pick(
                "Store server: running {v}",
                "Máy chủ cửa hàng: đang chạy {v}",
            )
            .replace("{v}", version),
        EdgeReading::Down => lang
            .pick(
                "Store server: not responding",
                "Máy chủ cửa hàng: không phản hồi",
            )
            .to_owned(),
    }
}

fn cloud_part(lang: Lang, link: CloudLink) -> String {
    match link {
        CloudLink::Online => lang.pick("Cloud: connected", "Cloud: đã kết nối"),
        CloudLink::Offline => lang.pick(
            "Cloud: offline — selling normally",
            "Cloud: mất kết nối — vẫn bán bình thường",
        ),
        CloudLink::NotTried => lang.pick("Cloud: not tried yet", "Cloud: chưa thử kết nối"),
    }
    .to_owned()
}

fn not_read_part(lang: Lang, reason: NotRead) -> &'static str {
    match reason {
        NotRead::TillClosed => lang.pick(
            "Open the till to see the cloud link",
            "Mở máy bán hàng để xem kết nối cloud",
        ),
        NotRead::SignInNeeded => lang.pick(
            "Sign in on the till to see the cloud link",
            "Đăng nhập trên máy bán hàng để xem kết nối cloud",
        ),
        NotRead::EdgeDown => lang.pick("Cloud link unknown", "Chưa rõ kết nối cloud"),
        NotRead::NotPaired => lang.pick(
            "This computer is not paired",
            "Máy tính này chưa được ghép nối",
        ),
        NotRead::Failed => lang.pick(
            "The store server's answer could not be read",
            "Không đọc được phản hồi của máy chủ cửa hàng",
        ),
    }
}

fn agent_part(lang: Lang, agent: &AgentStatus) -> String {
    match agent {
        AgentStatus::Starting => lang
            .pick("Print agent: starting", "Chương trình in: đang khởi động")
            .to_owned(),
        AgentStatus::Running { .. } => lang
            .pick("Print agent: running", "Chương trình in: đang chạy")
            .to_owned(),
        AgentStatus::Waiting {
            retry_in_seconds, ..
        } => lang
            .pick(
                "Print agent: restarting in {s} s",
                "Chương trình in: khởi động lại sau {s} giây",
            )
            .replace("{s}", &retry_in_seconds.to_string()),
        AgentStatus::Missing => lang
            .pick(
                "Print agent: not installed",
                "Chương trình in: chưa được cài đặt",
            )
            .to_owned(),
        AgentStatus::Stopped => lang
            .pick("Print agent: stopped", "Chương trình in: đã dừng")
            .to_owned(),
    }
}

/// A notification's title and body.
pub(crate) fn notice(lang: Lang, notice: &Notice) -> (String, String) {
    let (title, body) = match notice {
        Notice::EdgeDown => (
            lang.pick("Store server not responding", "Máy chủ cửa hàng không phản hồi"),
            lang.pick(
                "Tills on this store cannot reach it. Check that this computer's store server is running.",
                "Các máy bán hàng không kết nối được. Hãy kiểm tra máy chủ cửa hàng trên máy tính này.",
            ),
        ),
        Notice::EdgeBack => (
            lang.pick("Store server is back", "Máy chủ cửa hàng đã hoạt động lại"),
            lang.pick("Tills can reach it again.", "Các máy bán hàng đã kết nối lại được."),
        ),
        Notice::PairingLost => (
            lang.pick("This computer is no longer paired", "Máy tính này không còn được ghép nối"),
            lang.pick(
                "Enter a new pairing code from the store to use the till again.",
                "Hãy nhập mã ghép nối mới từ cửa hàng để dùng lại máy bán hàng.",
            ),
        ),
        Notice::CloudLost { waiting } => {
            return (
                lang.pick("Cloud link lost", "Mất kết nối cloud").to_owned(),
                lang.pick(
                    "The store keeps selling. {n} events are waiting to sync.",
                    "Cửa hàng vẫn bán bình thường. {n} sự kiện đang chờ đồng bộ.",
                )
                .replace("{n}", &waiting.to_string()),
            );
        }
        Notice::CloudRestored => (
            lang.pick("Cloud link restored", "Đã kết nối lại cloud"),
            lang.pick("Waiting events will now sync.", "Các sự kiện đang chờ sẽ được đồng bộ."),
        ),
        Notice::OutboxRose { level, depth, planned } => {
            return (
                outbox_title(lang, *level).to_owned(),
                lang.pick(
                    "{n} of {planned} events are waiting to sync. Selling continues; check the internet.",
                    "{n} trên {planned} sự kiện đang chờ đồng bộ. Vẫn bán bình thường; hãy kiểm tra internet.",
                )
                .replace("{n}", &depth.to_string())
                .replace("{planned}", &planned.to_string()),
            );
        }
        Notice::OutboxNormal => (
            lang.pick("Sync has caught up", "Đồng bộ đã bắt kịp"),
            lang.pick("Fewer than half the planned events are waiting.", "Số sự kiện chờ đã dưới một nửa mức dự kiến."),
        ),
        Notice::PrinterAdded { name } => {
            return (
                lang.pick("Printer added", "Đã thêm máy in").to_owned(),
                name.clone(),
            );
        }
        Notice::PrinterRemoved { name } => {
            return (
                lang.pick("Printer removed", "Đã gỡ máy in").to_owned(),
                name.clone(),
            );
        }
    };
    (title.to_owned(), body.to_owned())
}

fn outbox_title(lang: Lang, level: OutboxLevel) -> &'static str {
    match level {
        OutboxLevel::Elevated => lang.pick(
            "Sync is behind: half the planned backlog",
            "Đồng bộ chậm: đã đến một nửa mức dự kiến",
        ),
        OutboxLevel::High => lang.pick(
            "Sync is far behind: 80% of the planned backlog",
            "Đồng bộ rất chậm: đã đến 80% mức dự kiến",
        ),
        OutboxLevel::Beyond => lang.pick(
            "Sync is past the planned backlog",
            "Đồng bộ đã vượt mức dự kiến",
        ),
        OutboxLevel::Normal | OutboxLevel::Unspecified => {
            lang.pick("Sync status changed", "Trạng thái đồng bộ đã thay đổi")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Key, Lang, notice, summary, text};
    use crate::health::{
        CloudLink, EdgeReading, NotRead, Notice, OutboxLevel, PairingReading, Printer, Snapshot,
        StoreFacts, StoreReading,
    };
    use crate::sidecar::AgentStatus;

    #[test]
    fn the_language_follows_the_tag() {
        assert_eq!(Lang::from_tag("vi-VN"), Lang::Vi);
        assert_eq!(Lang::from_tag("VI"), Lang::Vi);
        assert_eq!(Lang::from_tag("vi_VN.UTF-8"), Lang::Vi);
        assert_eq!(Lang::from_tag("en-US"), Lang::En);
        assert_eq!(Lang::from_tag("ja-JP"), Lang::En);
        assert_eq!(Lang::from_tag(""), Lang::En);
        assert_eq!(Lang::from_tag(Lang::Vi.tag()), Lang::Vi);
        assert_eq!(Lang::from_tag(Lang::En.tag()), Lang::En);
    }

    #[test]
    fn menu_labels_differ_by_language() {
        assert_eq!(text(Lang::En, Key::MenuQuit), "Quit");
        assert_eq!(text(Lang::Vi, Key::MenuQuit), "Thoát");
    }

    fn read_snapshot() -> Snapshot {
        Snapshot {
            edge: EdgeReading::Up {
                version: "1.2.3".to_owned(),
            },
            pairing: PairingReading::Paired,
            store: StoreReading::Read(StoreFacts {
                cloud_link: CloudLink::Offline,
                outbox_depth: 42,
                outbox_planned_depth: 100_000,
                outbox_level: OutboxLevel::Normal,
                printers: vec![Printer {
                    device_id: "p1".to_owned(),
                    name: "Kitchen".to_owned(),
                }],
            }),
        }
    }

    #[test]
    fn the_summary_names_each_fact_it_has() {
        assert_eq!(
            summary(Lang::En, &read_snapshot(), None),
            "Store server: running 1.2.3 · Cloud: offline — selling normally · Waiting to sync: 42 · Printers: 1"
        );
        let vi = summary(
            Lang::Vi,
            &read_snapshot(),
            Some(&AgentStatus::Waiting {
                restarts: 2,
                retry_in_seconds: 4,
            }),
        );
        assert!(vi.contains("Chờ đồng bộ: 42"), "{vi}");
        assert!(
            vi.ends_with("Chương trình in: khởi động lại sau 4 giây"),
            "{vi}"
        );
    }

    #[test]
    fn the_summary_says_why_the_cloud_link_is_not_shown() {
        let mut snapshot = read_snapshot();
        snapshot.store = StoreReading::NotRead {
            reason: NotRead::TillClosed,
        };
        assert_eq!(
            summary(Lang::En, &snapshot, None),
            "Store server: running 1.2.3 · Open the till to see the cloud link"
        );
    }

    #[test]
    fn notices_carry_their_numbers_and_names() {
        let (title, body) = notice(Lang::En, &Notice::CloudLost { waiting: 7 });
        assert_eq!(title, "Cloud link lost");
        assert!(body.contains("7 events"), "{body}");
        let (title, body) = notice(
            Lang::Vi,
            &Notice::OutboxRose {
                level: OutboxLevel::High,
                depth: 80_000,
                planned: 100_000,
            },
        );
        assert!(title.contains("80%"), "{title}");
        assert!(body.starts_with("80000 trên 100000"), "{body}");
        let (_, body) = notice(
            Lang::En,
            &Notice::PrinterAdded {
                name: "Bar".to_owned(),
            },
        );
        assert_eq!(body, "Bar");
    }
}
