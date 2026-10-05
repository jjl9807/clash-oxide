use crate::{
    AppView,
    tray::{self, Action, Tray},
};
use gpui_kit::{component::TitleBar, *};
use oxide_client::{Update, lifecycle::DaemonOptions};
use oxide_i18n::tr;
use oxide_model::{Command, Diagnostic, Snapshot};
use std::{
    io::Write,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

/// Lives for the GUI process, independently of whether a window is open.
pub struct Desktop {
    pub state: Snapshot,
    pub completion: Option<(u64, Command, Option<Diagnostic>)>,
    pub busy: bool,
    pub error: Option<Diagnostic>,
    pub connecting: Option<String>,
    pub shutting_down: bool,
    seen: Option<Instant>,
    commands: SyncSender<Command>,
    updates: Receiver<Update>,
    options: DaemonOptions,
    activations: Receiver<crate::instance::Activation>,
    shutdown_result: Option<Receiver<Result<bool, Diagnostic>>>,
    tray: Option<Tray>,
    hosts: Receiver<bool>,
    host_seen: Option<Instant>,
    host_available: bool,
    window: Option<AnyWindowHandle>,
    icon: Arc<image::RgbaImage>,
    locale_generation: u64,
}

impl Desktop {
    pub fn new(
        options: DaemonOptions,
        activations: Receiver<crate::instance::Activation>,
        icon: Arc<image::RgbaImage>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (commands, updates) = oxide_client::connect(options.clone());
        let tray = match Tray::new() {
            Ok(tray) => Some(tray),
            Err(error) => {
                eprintln!("{}", tr!("tray.unavailable", detail = format!("{error:#}")));
                None
            }
        };
        let hosts = tray::monitor_host();
        cx.spawn(async move |desktop, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if desktop.update(cx, |desktop, cx| desktop.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            state: Snapshot::default(),
            completion: None,
            busy: true,
            error: None,
            connecting: Some("status.connecting".into()),
            shutting_down: false,
            seen: None,
            commands,
            updates,
            options,
            activations,
            shutdown_result: None,
            tray,
            hosts,
            host_seen: None,
            host_available: false,
            window: None,
            icon,
            locale_generation: oxide_i18n::generation(),
        }
    }

    pub fn connected(&self) -> bool {
        (self.busy && self.seen.is_some())
            || self
                .seen
                .is_some_and(|seen| seen.elapsed() < Duration::from_secs(5))
    }

    pub fn can_close_to_tray(&self) -> bool {
        self.tray.is_some()
            && self.host_available
            && self
                .host_seen
                .is_some_and(|seen| seen.elapsed() < Duration::from_secs(4))
    }

    pub fn send(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.commands.try_send(command) {
            Ok(()) => {
                self.busy = true;
                self.error = None;
            }
            Err(_) => {
                self.error = Some(Diagnostic::new(
                    "error.queue_busy",
                    "The command queue is busy. Try again shortly.",
                ))
            }
        }
        cx.notify();
    }

    pub fn show_window(&mut self, cx: &mut Context<Self>) {
        // GPUI may render synchronously while opening/activating a window. Release
        // the Desktop update lease first so AppView can read the shared state.
        let desktop = cx.entity();
        cx.defer(move |cx| Self::open_window(desktop, cx));
    }

    fn open_window(desktop: Entity<Self>, cx: &mut App) {
        let (existing, icon) = {
            let state = desktop.read(cx);
            (state.window, state.icon.clone())
        };
        if let Some(window) = existing
            && window
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            cx.activate(true);
            return;
        }
        let view_desktop = desktop.clone();
        let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
        let result = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.), px(540.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Clash Oxide".into()),
                    ..TitleBar::title_bar_options()
                }),
                window_decorations: Some(WindowDecorations::Client),
                app_id: Some("clash-oxide".into()),
                icon: Some(icon),
                ..TitleBar::window_options()
            },
            cx,
            move |window, cx| cx.new(|cx| AppView::new(view_desktop, window, cx)),
        );
        desktop.update(cx, |desktop, cx| match result {
            Ok((window, _)) => {
                desktop.window = Some(window);
                cx.activate(true);
            }
            Err(error) => {
                desktop.error = Some(Diagnostic::new(
                    "error.open_window",
                    format!("Could not open the desktop window: {error:#}"),
                ));
                eprintln!(
                    "{}",
                    oxide_i18n::diagnostic(desktop.error.as_ref().unwrap())
                );
                if !desktop.can_close_to_tray() {
                    cx.quit();
                }
            }
        });
    }

    pub fn window_closed(&mut self, cx: &mut Context<Self>) {
        if cx.windows().is_empty() {
            self.window = None;
            if !self.can_close_to_tray() && !self.shutting_down {
                cx.quit();
            }
        }
    }

    fn shutdown(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.shutting_down = true;
        self.error = None;
        let options = self.options.clone();
        let (send, receive) = mpsc::sync_channel(1);
        self.shutdown_result = Some(receive);
        std::thread::spawn(move || {
            let result = oxide_client::lifecycle::shutdown(&options)
                .map_err(|error| Diagnostic::from_error(&error));
            let _ = send.send(result);
        });
        cx.notify();
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.locale_generation != oxide_i18n::generation() {
            self.locale_generation = oxide_i18n::generation();
            gpui_kit::component::set_locale(&oxide_i18n::locale());
            cx.refresh_windows();
            cx.notify();
        }
        let was_connected = self.connected();
        let mut changed = false;
        while let Ok(update) = self.updates.try_recv() {
            if self.shutting_down {
                continue;
            }
            changed = true;
            match update {
                Update::State(state) => {
                    self.state = *state;
                    self.seen = Some(Instant::now());
                    self.error = None;
                    self.connecting = None;
                }
                Update::Error(error) => {
                    self.error = Some(error);
                    self.connecting = None;
                }
                Update::Busy(busy) => self.busy = busy,
                Update::Completed { command, error } => {
                    self.completion = Some((
                        self.completion.as_ref().map_or(1, |c| c.0 + 1),
                        command,
                        error,
                    ));
                }
                Update::Connecting(message) => self.connecting = Some(message),
            }
        }
        while let Ok(available) = self.hosts.try_recv() {
            changed |= self.host_available != available;
            self.host_available = available;
            self.host_seen = Some(Instant::now());
        }
        // A vanished panel must never leave an inaccessible, windowless GUI.
        if self.window.is_none() && !self.can_close_to_tray() && !self.shutting_down {
            self.show_window(cx);
        }
        while let Ok(mut activation) = self.activations.try_recv() {
            if let Some(language) = activation.language {
                oxide_i18n::set_session_language(language);
            }
            self.show_window(cx);
            let desktop = cx.entity();
            cx.defer(move |cx| {
                let reply = if desktop.read(cx).window.is_some() {
                    b"ok\n"
                } else {
                    b"no\n"
                };
                let _ = activation.stream.write_all(reply);
            });
        }
        if let Some(result) = self
            .shutdown_result
            .as_ref()
            .and_then(|receive| receive.try_recv().ok())
        {
            self.shutdown_result = None;
            match result {
                Ok(_) => {
                    cx.quit();
                    return;
                }
                Err(error) => {
                    self.shutting_down = false;
                    self.busy = false;
                    self.error = Some(error);
                    self.show_window(cx);
                    changed = true;
                }
            }
        }
        if let Some(mut tray) = self.tray.take() {
            for action in tray.actions() {
                match action {
                    Action::Open => self.show_window(cx),
                    Action::Command(command) => self.send(command, cx),
                    Action::Quit if !self.shutting_down => {
                        cx.quit();
                        return;
                    }
                    Action::Quit => {}
                    Action::Shutdown => self.shutdown(cx),
                }
            }
            tray.sync(self);
            self.tray = Some(tray);
        }
        if changed || was_connected != self.connected() {
            cx.notify();
        }
    }
}
