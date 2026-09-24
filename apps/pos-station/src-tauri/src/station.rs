// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The app's state and the few things it does with windows, the tray and the sidecar.
//!
//! # Two modes, chosen by where the edge is (ADR-0147)
//!
//! - **Station** — the edge answers on `127.0.0.1:8787`, or the device paired with a loopback
//!   address: this is the store PC. A tray summarises the edge, the cloud link, the outbox and the
//!   printers, a notification marks each change, and the tray opens the till, the status page and
//!   the pairing QR.
//! - **Terminal** — the edge is another machine. The till, and the print agent as a sidecar started
//!   with this terminal's own token. The tray is smaller (till, status, quit): a full-screen till has
//!   no window chrome, and an operator still needs a way to the status page and out.
//!
//! The mode is decided once, at start. A machine that becomes the other kind is restarted.
//!
//! # Where the till comes from
//!
//! Always from the edge's own URL, so the till is always the version its edge serves and every
//! request is same-origin. The window's initialization script ([`crate::init_script`]) hands it the
//! token first; its navigation handler keeps it on that origin; and no capability names it, so it
//! can invoke nothing.
//!
//! # Locking
//!
//! A `Station` mutex is never held across a Tauri call. Several Tauri calls hop to the main thread
//! and wait, and the main thread may be waiting on the same mutex — so everything below copies what
//! it needs out, drops the guard, and only then touches a window, the tray or a notification.

use std::error::Error;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder, Wry};
use tauri_plugin_notification::NotificationExt as _;

use crate::address::{self, EdgeOrigin, LOCAL_EDGE};
use crate::config::{self, Settings};
use crate::edge::EdgeClient;
use crate::health::{EdgeReading, Notice, PairingReading, Snapshot};
use crate::i18n::{self, Key, Lang};
use crate::monitor::{Monitor, Target};
use crate::sidecar::{self, AgentStatus, Launch, Supervisor};
use crate::{init_script, vault};

/// The window labels. The capability file grants commands to `connect` and `status` only.
const CONNECT: &str = "connect";
const STATUS: &str = "status";
const TILL: &str = "till";
const PAIRING: &str = "pairing";

/// The tray menu item ids.
const MENU_SUMMARY: &str = "summary";
const MENU_OPEN_TILL: &str = "open_till";
const MENU_STATUS: &str = "status";
const MENU_PAIRING_QR: &str = "pairing_qr";
const MENU_QUIT: &str = "quit";

/// The tray icon: the 32 px app icon.
const TRAY_ICON: &[u8] = include_bytes!("../icons/32x32.png");

/// Which kind of machine this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum Mode {
    /// The store PC, running its own edge.
    #[serde(rename = "MODE_STATION")]
    Station,
    /// Another machine, reaching the edge over the LAN (or a hosted origin).
    #[serde(rename = "MODE_TERMINAL")]
    Terminal,
}

/// Station when the edge answers locally or the device paired with a loopback address; Terminal
/// otherwise. The second clause is what keeps a store PC a Station when the app starts at login
/// before its edge's service has finished starting.
pub(crate) fn decide_mode(local_edge_answers: bool, paired_with: Option<&EdgeOrigin>) -> Mode {
    if local_edge_answers || paired_with.is_some_and(EdgeOrigin::is_loopback) {
        Mode::Station
    } else {
        Mode::Terminal
    }
}

/// What the connect page says above the form, as a key into `ui/i18n.js`.
pub(crate) const NOTICE_PAIRING_LOST: &str = "connect.notice.pairing_lost";

struct Paths {
    settings: PathBuf,
    data: PathBuf,
    logs: PathBuf,
}

struct Inner {
    mode: Mode,
    settings: Settings,
    lang: Lang,
    origin: Option<EdgeOrigin>,
    token: Option<String>,
    snapshot: Snapshot,
    till_pending: bool,
    waiting_shown: bool,
    notice: Option<&'static str>,
    tray_text: String,
    /// Whether the token in use is in the OS credential store, or only in memory for this run.
    token_saved: bool,
}

struct Tray {
    icon: TrayIcon<Wry>,
    summary: MenuItem<Wry>,
    labelled: Vec<(MenuItem<Wry>, Key)>,
}

/// The app's managed state.
pub(crate) struct Station {
    paths: Paths,
    inner: Mutex<Inner>,
    tray: Mutex<Option<Tray>>,
    monitor: Mutex<Option<Monitor>>,
    supervisor: Mutex<Option<Supervisor>>,
    _instance: File,
}

impl core::fmt::Debug for Station {
    /// Names the parts and never the token.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Station").finish_non_exhaustive()
    }
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Station {
    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut guard(&self.inner))
    }

    fn agent_status(&self) -> Option<AgentStatus> {
        guard(&self.supervisor).as_ref().map(Supervisor::status)
    }

    fn wake_monitor(&self) {
        if let Some(monitor) = guard(&self.monitor).as_ref() {
            monitor.wake();
        }
    }
}

/// Everything that happens before the first window: called from Tauri's `setup` hook, on the main
/// thread, so it only claims the instance lock and hands the rest to a thread.
///
/// It never returns an error, because Tauri 2.11 turns a setup error into a `panic!` — and this
/// crate's release profile aborts on one. A second copy asks the event loop to exit with `0`, which
/// is the ordinary outcome of the installer's autostart meeting an operator's double-click; anything
/// else that stops the start is logged and exits with `1`.
pub(crate) fn start(app: &AppHandle) {
    match try_start(app) {
        Ok(true) => {}
        Ok(false) => {
            log::warn!("another POS Station is already running for this user; this one exits");
            app.exit(0);
        }
        Err(error) => {
            log::error!("POS Station could not start: {error}");
            app.exit(1);
        }
    }
}

/// [`start`]'s work. `Ok(false)` when another copy holds the instance lock.
fn try_start(app: &AppHandle) -> Result<bool, Box<dyn Error>> {
    let paths = Paths {
        settings: app.path().app_config_dir()?.join("station.json"),
        data: app.path().app_data_dir()?,
        logs: app.path().app_log_dir()?,
    };
    let Some(instance) = claim_instance(&paths.data)? else {
        return Ok(false);
    };
    let settings = config::load(&paths.settings);
    let lang = settings
        .language
        .as_deref()
        .map_or_else(Lang::from_environment, Lang::from_tag);
    let origin = settings
        .edge_origin
        .as_deref()
        .and_then(|text| address::parse(text).ok())
        .map(|input| input.origin);
    app.manage(Station {
        paths,
        inner: Mutex::new(Inner {
            mode: Mode::Terminal,
            settings,
            lang,
            origin,
            token: None,
            snapshot: Snapshot::unknown(),
            till_pending: false,
            waiting_shown: false,
            notice: None,
            tray_text: String::new(),
            token_saved: true,
        }),
        tray: Mutex::new(None),
        monitor: Mutex::new(None),
        supervisor: Mutex::new(None),
        _instance: instance,
    });
    let app = app.clone();
    std::thread::Builder::new()
        .name("station-start".to_owned())
        .spawn(move || begin(&app))?;
    Ok(true)
}

/// One running copy per user: two would mean two trays and, on a terminal, two print agents.
fn claim_instance(dir: &Path) -> std::io::Result<Option<File>> {
    std::fs::create_dir_all(dir)?;
    let file = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("station.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Decides the mode, builds the tray, and opens either the connect page or (through the monitor) the
/// till.
fn begin(app: &AppHandle) {
    let station = app.state::<Station>();
    let local = EdgeClient::new().health(&EdgeOrigin::local()).is_ok();
    let origin = station.with(|inner| inner.origin.clone());
    let mode = decide_mode(local, origin.as_ref());
    let token = origin.as_ref().and_then(|origin| {
        vault::load(&origin.to_string()).unwrap_or_else(|error| {
            log::error!("{error}");
            None
        })
    });
    let paired = origin.is_some() && token.is_some();
    let lang = station.with(|inner| {
        inner.mode = mode;
        inner.token = token;
        inner.till_pending = paired;
        inner.lang
    });
    log::info!(
        "starting in {mode:?} mode; paired with {}",
        origin
            .as_ref()
            .map_or_else(|| "nothing".to_owned(), ToString::to_string)
    );
    match build_tray(app, mode, lang) {
        Ok(tray) => *guard(&station.tray) = Some(tray),
        Err(error) => log::error!("no tray icon: {error}"),
    }
    if !paired {
        open_connect(app);
    }
    match Monitor::start(app.clone()) {
        Ok(monitor) => *guard(&station.monitor) = Some(monitor),
        Err(error) => log::error!("the monitor could not start: {error}"),
    }
}

fn build_tray(app: &AppHandle, mode: Mode, lang: Lang) -> tauri::Result<Tray> {
    let item =
        |id: &str, key: Key| MenuItem::with_id(app, id, i18n::text(lang, key), true, None::<&str>);
    let summary = MenuItem::with_id(
        app,
        MENU_SUMMARY,
        i18n::text(lang, Key::AppName),
        false,
        None::<&str>,
    )?;
    let open_till = item(MENU_OPEN_TILL, Key::MenuOpenTill)?;
    let status = item(MENU_STATUS, Key::MenuStatus)?;
    let quit = item(MENU_QUIT, Key::MenuQuit)?;
    let mut labelled = vec![
        (open_till.clone(), Key::MenuOpenTill),
        (status.clone(), Key::MenuStatus),
        (quit.clone(), Key::MenuQuit),
    ];
    let first = PredefinedMenuItem::separator(app)?;
    let last = PredefinedMenuItem::separator(app)?;
    let menu = if mode == Mode::Station {
        let pairing = item(MENU_PAIRING_QR, Key::MenuPairingQr)?;
        labelled.push((pairing.clone(), Key::MenuPairingQr));
        Menu::with_items(
            app,
            &[
                &summary, &first, &open_till, &status, &pairing, &last, &quit,
            ],
        )?
    } else {
        Menu::with_items(app, &[&summary, &first, &open_till, &status, &last, &quit])?
    };
    let icon = TrayIconBuilder::with_id("station")
        .icon(Image::from_bytes(TRAY_ICON)?)
        .tooltip(i18n::text(lang, Key::AppName))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| on_menu(app, &event))
        .build(app)?;
    Ok(Tray {
        icon,
        summary,
        labelled,
    })
}

fn on_menu(app: &AppHandle, event: &MenuEvent) {
    match event.id().as_ref() {
        MENU_OPEN_TILL => {
            let paired = app.state::<Station>().with(|inner| inner.token.is_some());
            if paired {
                open_till(app);
            } else {
                open_connect(app);
            }
        }
        MENU_STATUS => open_status(app),
        MENU_PAIRING_QR => open_pairing_qr(app),
        MENU_QUIT => app.exit(0),
        _ => {}
    }
}

/// Whether `url` is one of the app's own bundled pages.
pub(crate) fn is_local_url(url: &Url) -> bool {
    url.scheme() == "tauri"
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"))
}

/// Whether a navigation from a till window to `url` stays on the edge's origin.
fn stays_on(origin: &str, url: &Url) -> bool {
    url.scheme() == "about" || url.origin().ascii_serialization() == origin
}

fn focus_existing(app: &AppHandle, label: &str) -> bool {
    let Some(window) = app.get_webview_window(label) else {
        return false;
    };
    let _shown = window.show();
    let _unminimized = window.unminimize();
    let _focused = window.set_focus();
    true
}

fn open_local(app: &AppHandle, label: &str, page: &str, title: Key) {
    if focus_existing(app, label) {
        return;
    }
    let lang = app.state::<Station>().with(|inner| inner.lang);
    let built = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title(i18n::text(lang, title))
        .inner_size(560.0, 700.0)
        .center()
        .on_navigation(is_local_url)
        .build();
    if let Err(error) = built {
        log::error!("could not open {page}: {error}");
    }
}

/// Opens (or focuses) the connect page.
pub(crate) fn open_connect(app: &AppHandle) {
    open_local(app, CONNECT, "connect.html", Key::WindowConnect);
}

/// Opens (or focuses) the status page.
pub(crate) fn open_status(app: &AppHandle) {
    open_local(app, STATUS, "status.html", Key::WindowStatus);
}

/// Opens a page of the edge's own UI with the token handed over, or focuses the window already
/// showing it.
fn open_remote(app: &AppHandle, label: &str, path: &str, title: Key, full_screen: Option<bool>) {
    if focus_existing(app, label) {
        return;
    }
    let (origin, token, lang, settings_full_screen) = app.state::<Station>().with(|inner| {
        (
            inner.origin.clone(),
            inner.token.clone(),
            inner.lang,
            inner.settings.till_full_screen,
        )
    });
    let (Some(origin), Some(token)) = (origin, token) else {
        open_connect(app);
        return;
    };
    let url = match Url::parse(&origin.url(path)) {
        Ok(url) => url,
        Err(error) => {
            log::error!("{origin}{path} is not a URL: {error}");
            return;
        }
    };
    let allowed = origin.to_string();
    let built = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(i18n::text(lang, title))
        .inner_size(1280.0, 800.0)
        .fullscreen(full_screen.unwrap_or(settings_full_screen))
        .initialization_script(init_script::build(&origin, &token))
        .on_navigation(move |url| stays_on(&allowed, url))
        .build();
    match built {
        Ok(_) => app.state::<Station>().wake_monitor(),
        Err(error) => log::error!("could not open {path}: {error}"),
    }
}

/// Opens (or focuses) the till, full screen unless the settings say otherwise.
pub(crate) fn open_till(app: &AppHandle) {
    open_remote(app, TILL, "/", Key::WindowTill, None);
}

/// Opens the till's Devices screen, where a manager mints the next pairing code and its QR.
pub(crate) fn open_pairing_qr(app: &AppHandle) {
    open_remote(app, PAIRING, "/devices", Key::WindowPairingQr, Some(false));
}

fn close(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label)
        && let Err(error) = window.close()
    {
        log::warn!("could not close {label}: {error}");
    }
}

/// What the monitor asks about this round.
pub(crate) fn target(app: &AppHandle) -> Target {
    let (origin, token, till_pending) = app.state::<Station>().with(|inner| {
        (
            inner.origin.clone(),
            inner.token.clone(),
            inner.till_pending,
        )
    });
    let till_open =
        app.get_webview_window(TILL).is_some() || app.get_webview_window(PAIRING).is_some();
    Target {
        origin,
        token,
        till_open,
        till_pending,
    }
}

/// Takes one round's result: stores it, refreshes the tray, notifies, and acts on the pairing.
pub(crate) fn publish(app: &AppHandle, snapshot: &Snapshot, notices: &[Notice]) {
    let station = app.state::<Station>();
    let agent = station.agent_status();
    let (mode, lang, pending, waiting_shown) = station.with(|inner| {
        inner.snapshot = snapshot.clone();
        (
            inner.mode,
            inner.lang,
            inner.till_pending,
            inner.waiting_shown,
        )
    });
    refresh_tray(app, lang, snapshot, agent.as_ref());
    for notice in notices {
        log::info!("{notice:?}");
        if mode == Mode::Station {
            let (title, body) = i18n::notice(lang, notice);
            if let Err(error) = app.notification().builder().title(title).body(body).show() {
                log::warn!("the notification could not be shown: {error}");
            }
        }
    }
    let edge_up = matches!(snapshot.edge, EdgeReading::Up { .. });
    match snapshot.pairing {
        PairingReading::Lost => pairing_lost(app),
        PairingReading::Paired => {
            if pending && edge_up {
                station.with(|inner| {
                    inner.till_pending = false;
                    inner.waiting_shown = false;
                });
                open_till(app);
                if waiting_shown {
                    close(app, STATUS);
                }
            }
            if mode == Mode::Terminal {
                ensure_sidecar(app);
            }
        }
        PairingReading::Unknown => {
            if pending && !edge_up && !waiting_shown {
                station.with(|inner| inner.waiting_shown = true);
                open_status(app);
            }
        }
    }
}

fn refresh_tray(app: &AppHandle, lang: Lang, snapshot: &Snapshot, agent: Option<&AgentStatus>) {
    let station = app.state::<Station>();
    let text = i18n::summary(lang, snapshot, agent);
    let changed = station.with(|inner| {
        let changed = inner.tray_text != text;
        inner.tray_text.clone_from(&text);
        changed
    });
    if !changed {
        return;
    }
    let handles = guard(&station.tray)
        .as_ref()
        .map(|tray| (tray.icon.clone(), tray.summary.clone()));
    if let Some((icon, summary)) = handles {
        if let Err(error) = summary.set_text(&text) {
            log::warn!("the tray summary could not be updated: {error}");
        }
        if let Err(error) = icon.set_tooltip(Some(&text)) {
            log::warn!("the tray tooltip could not be updated: {error}");
        }
    }
}

/// The edge refused the token: forget it everywhere, stop the agent, and ask for a new code.
fn pairing_lost(app: &AppHandle) {
    let station = app.state::<Station>();
    let origin = station.with(|inner| {
        inner.token = None;
        inner.till_pending = false;
        inner.notice = Some(NOTICE_PAIRING_LOST);
        inner.origin.clone()
    });
    log::warn!(
        "the edge no longer accepts this device's token; pairing again ({})",
        origin
            .as_ref()
            .map_or_else(String::new, ToString::to_string)
    );
    if let Some(origin) = origin
        && let Err(error) = vault::forget(&origin.to_string())
    {
        log::error!("{error}");
    }
    stop_sidecar(app);
    close(app, TILL);
    close(app, PAIRING);
    open_connect(app);
}

/// A successful pairing from the connect page: remember it, open the till, start the agent.
/// `saved` says whether the credential store took the token or it lives only for this run.
pub(crate) fn paired(app: &AppHandle, origin: &EdgeOrigin, token: String, saved: bool) {
    let station = app.state::<Station>();
    let (previous, settings, mode) = station.with(|inner| {
        let previous = inner.origin.replace(origin.clone());
        inner.token = Some(token);
        inner.token_saved = saved;
        inner.settings.edge_origin = Some(origin.to_string());
        inner.notice = None;
        inner.till_pending = false;
        (previous, inner.settings.clone(), inner.mode)
    });
    if let Err(error) = config::save(&station.paths.settings, &settings) {
        log::error!(
            "could not save {}: {error}",
            station.paths.settings.display()
        );
    }
    if let Some(previous) = previous.filter(|previous| previous != origin)
        && let Err(error) = vault::forget(&previous.to_string())
    {
        log::warn!("the token for {previous} could not be forgotten: {error}");
    }
    log::info!("paired with {origin}");
    // Pairing happens from the connect page, which is only shown with no till open (at first start,
    // or after `pairing_lost` closed it), so there is no till holding an old token to replace here.
    stop_sidecar(app);
    open_till(app);
    close(app, CONNECT);
    if mode == Mode::Terminal {
        ensure_sidecar(app);
    }
    station.wake_monitor();
}

fn ensure_sidecar(app: &AppHandle) {
    let station = app.state::<Station>();
    let mut supervisor = guard(&station.supervisor);
    if supervisor.is_some() {
        return;
    }
    let (origin, token) = station.with(|inner| (inner.origin.clone(), inner.token.clone()));
    let (Some(origin), Some(token)) = (origin, token) else {
        return;
    };
    let program = match sidecar::beside_current_exe() {
        Ok(program) => program,
        Err(error) => {
            log::error!("the print agent's location is unknown: {error}");
            return;
        }
    };
    let launch = Launch {
        program,
        config_path: station.paths.data.join("print-agent.toml"),
        state_path: station.paths.data.join("print-agent-state.json"),
        log_path: station.paths.logs.join("print-agent.log"),
        edge_url: origin.to_string(),
        token,
    };
    match Supervisor::start(launch) {
        Ok(started) => *supervisor = Some(started),
        Err(error) => log::error!("the print agent supervisor could not start: {error}"),
    }
}

fn stop_sidecar(app: &AppHandle) {
    let running = guard(&app.state::<Station>().supervisor).take();
    if let Some(supervisor) = running {
        supervisor.stop();
    }
}

/// The language the bundled pages report; relabels the tray when it changes.
pub(crate) fn set_language(app: &AppHandle, lang: Lang) {
    let station = app.state::<Station>();
    let (changed, settings) = station.with(|inner| {
        let changed = inner.lang != lang;
        inner.lang = lang;
        inner.settings.language = Some(lang.tag().to_owned());
        (changed, inner.settings.clone())
    });
    if !changed {
        return;
    }
    if let Err(error) = config::save(&station.paths.settings, &settings) {
        log::warn!("could not save the language: {error}");
    }
    let labelled = guard(&station.tray)
        .as_ref()
        .map(|tray| tray.labelled.clone())
        .unwrap_or_default();
    for (item, key) in labelled {
        if let Err(error) = item.set_text(i18n::text(lang, key)) {
            log::warn!("the tray could not be relabelled: {error}");
        }
    }
    station.with(|inner| inner.tray_text.clear());
    let snapshot = station.with(|inner| inner.snapshot.clone());
    refresh_tray(app, lang, &snapshot, station.agent_status().as_ref());
}

/// What the connect page shows before the operator types.
#[derive(Debug, Serialize)]
pub(crate) struct ConnectContext {
    /// The address to prefill: the edge last paired with, or the store PC's own edge in Station mode.
    address: String,
    /// A key into `ui/i18n.js` for a notice above the form, such as a lost pairing.
    notice: Option<&'static str>,
}

/// The connect page's prefill.
pub(crate) fn connect_context(app: &AppHandle) -> ConnectContext {
    app.state::<Station>().with(|inner| ConnectContext {
        address: match (&inner.origin, inner.mode) {
            (Some(origin), _) => origin.to_string(),
            (None, Mode::Station) => LOCAL_EDGE.to_owned(),
            (None, Mode::Terminal) => String::new(),
        },
        notice: inner.notice,
    })
}

/// What the status page draws.
#[derive(Debug, Serialize)]
pub(crate) struct StatusView {
    mode: Mode,
    edge_origin: Option<String>,
    snapshot: Snapshot,
    print_agent: Option<AgentStatus>,
    /// `false` when the credential store refused the token and it lasts only until the app quits.
    pairing_saved: bool,
    app_version: &'static str,
}

/// The status page's data.
pub(crate) fn status_view(app: &AppHandle) -> StatusView {
    let station = app.state::<Station>();
    let print_agent = station.agent_status();
    station.with(|inner| StatusView {
        mode: inner.mode,
        edge_origin: inner.origin.as_ref().map(ToString::to_string),
        snapshot: inner.snapshot.clone(),
        print_agent,
        pairing_saved: inner.token.is_none() || inner.token_saved,
        app_version: env!("CARGO_PKG_VERSION"),
    })
}

/// The event loop's hook: closing the last window keeps the tray (when there is one), and quitting
/// stops the print agent.
///
/// The state is looked up with `try_state`: a copy that found another one running exits before it
/// manages any, and has nothing to stop.
pub(crate) fn on_run_event(app: &AppHandle, event: &RunEvent) {
    let Some(station) = app.try_state::<Station>() else {
        return;
    };
    if let RunEvent::ExitRequested {
        code: None, api, ..
    } = event
    {
        let has_tray = guard(&station.tray).is_some();
        if has_tray {
            api.prevent_exit();
        }
    }
    if let RunEvent::Exit = event {
        let monitor = guard(&station.monitor).take();
        if let Some(monitor) = monitor {
            monitor.stop();
        }
        stop_sidecar(app);
        log::info!("stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, decide_mode, is_local_url, stays_on};
    use crate::address::parse;
    use tauri::Url;

    #[test]
    fn the_mode_follows_where_the_edge_is() {
        let lan = parse("192.168.1.10:8080").unwrap().origin;
        let local = parse("127.0.0.1:8080").unwrap().origin;
        assert_eq!(decide_mode(true, None), Mode::Station);
        assert_eq!(decide_mode(true, Some(&lan)), Mode::Station);
        assert_eq!(decide_mode(false, Some(&local)), Mode::Station);
        assert_eq!(decide_mode(false, Some(&lan)), Mode::Terminal);
        assert_eq!(decide_mode(false, None), Mode::Terminal);
    }

    #[test]
    fn only_the_bundled_pages_are_local() {
        for local in [
            "tauri://localhost/connect.html",
            "http://tauri.localhost/status.html",
        ] {
            assert!(is_local_url(&Url::parse(local).unwrap()), "{local}");
        }
        for remote in [
            "http://192.168.1.10:8080/",
            "https://tauri.localhost.example/",
            "http://localhost/",
        ] {
            assert!(!is_local_url(&Url::parse(remote).unwrap()), "{remote}");
        }
    }

    #[test]
    fn a_till_window_stays_on_its_edge() {
        let origin = "http://192.168.1.10:8080";
        assert!(stays_on(
            origin,
            &Url::parse("http://192.168.1.10:8080/pay?x=1").unwrap()
        ));
        assert!(stays_on(origin, &Url::parse("about:blank").unwrap()));
        assert!(!stays_on(
            origin,
            &Url::parse("http://192.168.1.10:8081/").unwrap()
        ));
        assert!(!stays_on(
            origin,
            &Url::parse("https://192.168.1.10:8080/").unwrap()
        ));
        assert!(!stays_on(
            origin,
            &Url::parse("http://evil.example/").unwrap()
        ));
    }
}
