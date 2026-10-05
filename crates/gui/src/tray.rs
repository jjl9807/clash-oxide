use crate::desktop::Desktop;
use anyhow::Result;
use oxide_i18n::tr;
use oxide_model::{Command, Phase};
use std::{
    sync::mpsc::{self, Receiver},
    time::Duration,
};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};

pub enum Action {
    Open,
    Command(Command),
    Quit,
    Shutdown,
}

pub struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    open: MenuItem,
    routing: Submenu,
    reload: MenuItem,
    system_proxy: CheckMenuItem,
    tun: CheckMenuItem,
    modes: Vec<CheckMenuItem>,
    quit: MenuItem,
    shutdown: MenuItem,
    last: Option<Presentation>,
}

#[derive(PartialEq, Eq)]
struct Presentation {
    status: String,
    locale: String,
    mode: String,
    connected: bool,
    system_proxy: bool,
    tun: bool,
    busy: bool,
    shutting_down: bool,
}

impl Tray {
    pub fn new() -> Result<Self> {
        let menu = Menu::new();
        let status = MenuItem::new(tr!("status.connecting"), false, None);
        let open = MenuItem::with_id("open", tr!("tray.open"), true, None);
        let reload = MenuItem::with_id("reload", tr!("common.reload"), false, None);
        let system_proxy = CheckMenuItem::with_id(
            "system-proxy",
            tr!("sidebar.system_proxy"),
            false,
            false,
            None,
        );
        let tun = CheckMenuItem::with_id("tun", tr!("sidebar.tun"), false, false, None);
        let routing = Submenu::new(tr!("settings.routing"), true);
        let modes: Vec<_> = ["rule", "global", "direct"]
            .into_iter()
            .map(|mode| CheckMenuItem::with_id(mode, oxide_i18n::mode(mode), false, false, None))
            .collect();
        for mode in &modes {
            routing.append(mode)?;
        }
        let quit = MenuItem::with_id("quit", tr!("tray.quit"), true, None);
        let shutdown = MenuItem::with_id("shutdown", tr!("tray.shutdown"), false, None);
        menu.append_items(&[
            &status,
            &open,
            &PredefinedMenuItem::separator(),
            &reload,
            &system_proxy,
            &tun,
            &routing,
            &PredefinedMenuItem::separator(),
            &quit,
            &shutdown,
        ])?;
        let image = crate::icon::render(32)?;
        let icon = TrayIconBuilder::new()
            .with_id("clash-oxide")
            .with_title("Clash Oxide")
            .with_tooltip(format!("Clash Oxide — {}", tr!("status.connecting")))
            .with_icon(Icon::from_rgba(image.into_raw(), 32, 32)?)
            .with_menu(Box::new(menu))
            .build()?;
        Ok(Self {
            icon,
            status,
            open,
            routing,
            reload,
            system_proxy,
            tun,
            modes,
            quit,
            shutdown,
            last: None,
        })
    }

    pub fn sync(&mut self, desktop: &Desktop) {
        let connected = desktop.connected();
        let status = if desktop.shutting_down {
            tr!("status.stopping_daemon")
        } else if let Some(message) = &desktop.connecting {
            tr!(message)
        } else if !connected {
            tr!("status.disconnected")
        } else if let Some(error) = desktop
            .error
            .as_ref()
            .map(oxide_i18n::diagnostic)
            .or_else(|| oxide_i18n::snapshot_error(&desktop.state))
        {
            tr!(
                "common.error",
                detail = error.chars().take(160).collect::<String>()
            )
        } else {
            match desktop.state.phase {
                Phase::Running => tr!("status.proxy_running"),
                Phase::Stopped => tr!("status.proxy_stopped"),
                Phase::Starting => tr!("status.proxy_starting"),
                Phase::Stopping => tr!("status.proxy_stopping"),
                Phase::Reloading => tr!("status.proxy_reloading"),
                Phase::Failed => tr!("status.proxy_failed"),
            }
        };
        let presentation = Presentation {
            status,
            locale: oxide_i18n::locale(),
            mode: desktop.state.settings.mode.clone(),
            connected,
            system_proxy: desktop.state.settings.system_proxy,
            tun: desktop.state.settings.tun,
            busy: desktop.busy,
            shutting_down: desktop.shutting_down,
        };
        // Avoid rebuilding D-Bus menu layouts on every snapshot/timer tick.
        if self.last.as_ref() == Some(&presentation) {
            return;
        }
        self.open.set_text(tr!("tray.open"));
        self.reload.set_text(tr!("common.reload"));
        self.system_proxy.set_text(tr!("sidebar.system_proxy"));
        self.tun.set_text(tr!("sidebar.tun"));
        self.routing.set_text(tr!("settings.routing"));
        self.quit.set_text(tr!("tray.quit"));
        self.shutdown.set_text(tr!("tray.shutdown"));
        self.status.set_text(&presentation.status);
        let _ = self
            .icon
            .set_tooltip(Some(format!("Clash Oxide — {}", presentation.status)));
        self.reload.set_enabled(!presentation.busy);
        self.system_proxy
            .set_enabled(!presentation.busy && presentation.connected);
        self.system_proxy.set_checked(presentation.system_proxy);
        self.tun
            .set_enabled(!presentation.busy && presentation.connected);
        self.tun.set_checked(presentation.tun);
        for (item, mode) in self.modes.iter().zip(["rule", "global", "direct"]) {
            item.set_text(oxide_i18n::mode(mode));
            item.set_enabled(!presentation.busy && presentation.connected);
            item.set_checked(presentation.mode == mode);
        }
        self.quit.set_enabled(!presentation.shutting_down);
        self.shutdown.set_enabled(!presentation.busy);
        self.last = Some(presentation);
    }

    pub fn actions(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                actions.push(Action::Open);
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let action = match event.id.0.as_str() {
                "open" => Action::Open,
                "reload" => Action::Command(Command::Reload),
                "system-proxy" => Action::Command(Command::SetSystemProxy(
                    !self.last.as_ref().is_some_and(|p| p.system_proxy),
                )),
                "tun" => {
                    Action::Command(Command::SetTun(!self.last.as_ref().is_some_and(|p| p.tun)))
                }
                mode @ ("rule" | "global" | "direct") => {
                    Action::Command(Command::SetMode(mode.into()))
                }
                "quit" => Action::Quit,
                "shutdown" => Action::Shutdown,
                _ => continue,
            };
            actions.push(action);
        }
        actions
    }
}

/// Creation can succeed before a panel exists. Close-to-tray requires both a
/// registered host and our actual item in the watcher's registry. All D-Bus I/O
/// runs outside GPUI; heartbeats also fail closed if the watcher stops answering.
pub fn monitor_host() -> Receiver<bool> {
    let (send, receive) = mpsc::sync_channel(4);
    std::thread::spawn(move || {
        loop {
            let connection = zbus::blocking::connection::Builder::session()
                .and_then(|builder| builder.method_timeout(Duration::from_secs(1)).build());
            if let Ok(connection) = connection {
                loop {
                    let available = host_available(&connection).unwrap_or(false);
                    if send.send(available).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                    if connection.inner().is_closed() {
                        break;
                    }
                }
            } else {
                if send.send(false).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
    receive
}

fn host_available(connection: &zbus::blocking::Connection) -> zbus::Result<bool> {
    let watcher: zbus::blocking::Proxy<'_> = zbus::blocking::proxy::Builder::new(connection)
        .destination("org.kde.StatusNotifierWatcher")?
        .path("/StatusNotifierWatcher")?
        .interface("org.kde.StatusNotifierWatcher")?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()?;
    if !watcher.get_property::<bool>("IsStatusNotifierHostRegistered")? {
        return Ok(false);
    }
    let items: Vec<String> = watcher.get_property("RegisteredStatusNotifierItems")?;
    let prefix = format!("org.kde.StatusNotifierItem-{}-", std::process::id());
    for item in items {
        let destination = item.split('/').next().unwrap_or_default();
        if destination.starts_with(&prefix) {
            return Ok(true);
        }
        // Some watchers normalize well-known names to unique bus names.
        if destination.starts_with(':') {
            let bus = zbus::blocking::fdo::DBusProxy::new(connection)?;
            if let Ok(name) = destination.try_into()
                && bus.get_connection_unix_process_id(name).ok() == Some(std::process::id())
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
