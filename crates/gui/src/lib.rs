mod connections;
mod desktop;
mod diagnostics;
mod icon;
mod instance;
mod profiles;
mod proxies;
mod settings;
mod theme;
mod tray;

use desktop::Desktop;
use gpui_kit::component::{
    button::*,
    input::{Input, InputEvent, InputState},
    scroll::{Scrollbar, ScrollbarHandle, ScrollbarMode},
    switch::Switch,
    *,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use oxide_client::view::{self, LOG_LEVELS, Page, Preferences, Session};
use oxide_i18n::{Language, tr};
use oxide_model::{Command, Connection, Diagnostic, bytes};
use std::{collections::VecDeque, rc::Rc, sync::Arc};

struct AppView {
    focus_handle: FocusHandle,
    desktop: Entity<Desktop>,
    session: Session,
    search: Entity<InputState>,
    source: Entity<InputState>,
    name: Entity<InputState>,
    port: Entity<InputState>,
    test_url: Entity<InputState>,
    locale_generation: u64,
    completion: u64,
    clear_import: bool,
    error_notice: Option<Diagnostic>,
    refresh_queue: VecDeque<String>,
    initialized_port: bool,
    proxy_list: proxies::ProxyList,
    rule_list: diagnostics::RuleList,
}
impl AppView {
    fn new(desktop: Entity<Desktop>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("common.search")));
        let source = cx
            .new(|cx| InputState::new(window, cx).placeholder(tr!("profiles.source_placeholder")));
        let name =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("profiles.name_placeholder")));
        let port = cx.new(|cx| InputState::new(window, cx));
        let preferences = Preferences::load();
        let preference_error = preferences
            .as_ref()
            .err()
            .map(Diagnostic::from_error)
            .or(theme::take_load_error(cx));
        let preferences = preferences.unwrap_or_default();
        theme::sync(window, cx);
        cx.observe_window_appearance(window, |_, window, cx| theme::sync(window, cx))
            .detach();
        let test_url =
            cx.new(|cx| InputState::new(window, cx).default_value(preferences.test_url.clone()));
        cx.subscribe_in(&search, window, |this, input, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.session.queries[this.session.page.index()] =
                    input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        cx.subscribe_in(&source, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.import(cx);
            }
            cx.notify();
        })
        .detach();
        cx.observe(&desktop, |this, desktop, cx| {
            let data = desktop.read(cx);
            this.session.update(&data.state);
            if let Some((id, command, error)) = &data.completion
                && *id != this.completion
            {
                this.completion = *id;
                this.error_notice = error.clone();
                if error.is_none() && matches!(command, Command::Import { .. }) {
                    this.clear_import = true;
                }
                if error.is_some() {
                    this.refresh_queue.clear();
                }
            }
            if !data.busy
                && let Some(id) = this.refresh_queue.pop_front()
            {
                this.send(Command::Refresh { id }, cx);
            }
            cx.notify();
        })
        .detach();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            focus_handle,
            desktop,
            session: Session::with_preferences(&preferences),
            search,
            source,
            name,
            port,
            test_url,
            locale_generation: oxide_i18n::generation(),
            completion: 0,
            clear_import: false,
            error_notice: preference_error,
            refresh_queue: VecDeque::new(),
            initialized_port: false,
            proxy_list: proxies::ProxyList::default(),
            rule_list: diagnostics::RuleList::default(),
        }
    }
    fn save_preferences(&mut self, cx: &mut Context<Self>) {
        let result = Preferences {
            test_url: self.test_url.read(cx).value().to_string(),
            sort: self.session.sort,
            expanded: self.session.expanded.clone(),
        }
        .save();
        self.error_notice = result.err().map(|error| Diagnostic::from_error(&error));
        cx.notify();
    }
    fn send(&mut self, command: Command, cx: &mut Context<Self>) {
        self.error_notice = None;
        self.desktop
            .update(cx, |desktop, cx| desktop.send(command, cx));
    }
    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.session.page = page;
        window.focus(&self.focus_handle, cx);
        let query = self.session.queries[page.index()].clone();
        self.search
            .update(cx, |input, cx| input.set_value(query, window, cx));
        cx.notify();
    }
    fn import(&mut self, cx: &mut Context<Self>) {
        if self.desktop.read(cx).busy {
            return;
        }
        let source = self.source.read(cx).value().to_string();
        if source.trim().is_empty() {
            return;
        }
        self.send(
            Command::Import {
                name: self.name.read(cx).value().to_string(),
                source,
            },
            cx,
        );
    }
    fn command_button(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        command: Command,
        cx: &Context<Self>,
    ) -> Button {
        Button::new(id.into())
            .label(label)
            .small()
            .disabled(self.desktop.read(cx).busy)
            .on_click(cx.listener(move |this, _, _, cx| this.send(command.clone(), cx)))
    }
    fn panel(cx: &App) -> Div {
        v_flex()
            .gap_2()
            .p_3()
            .rounded(cx.theme().radius_lg)
            .bg(theme::surface(cx))
            .border_1()
            .border_color(cx.theme().border)
    }
    fn list_viewport<H: ScrollbarHandle + Clone>(list: impl IntoElement, handle: &H) -> AnyElement {
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(list)
            .child(Scrollbar::vertical(handle).mode(ScrollbarMode::Always))
            .into_any_element()
    }
    fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
    }
    fn empty(title: String, hint: String, cx: &App) -> AnyElement {
        v_flex()
            .flex_1()
            .min_h(px(180.))
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                Icon::new(IconName::Inbox)
                    .size_8()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(div().text_lg().child(title))
            .child(Self::muted(hint, cx))
            .into_any_element()
    }
    fn nav_card(
        &self,
        page: Page,
        icon: IconName,
        detail: String,
        cx: &Context<Self>,
    ) -> AnyElement {
        let active = self.session.page == page;
        Self::panel(cx)
            .id(SharedString::from(format!("nav-{}", page.index())))
            .flex_1()
            .min_w_0()
            .cursor_pointer()
            .when(active, |el| {
                el.bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .border_color(cx.theme().primary)
            })
            .hover(|el| {
                el.bg(if active {
                    cx.theme().primary_hover
                } else {
                    cx.theme().accent
                })
            })
            .child(
                h_flex()
                    .justify_between()
                    .child(Icon::new(icon).size_4())
                    .child(
                        Self::muted(detail, cx)
                            .when(active, |el| el.text_color(cx.theme().primary_foreground)),
                    ),
            )
            .child(div().text_sm().font_medium().child(tr!(page.key())))
            .on_click(cx.listener(move |this, _, window, cx| this.navigate(page, window, cx)))
            .into_any_element()
    }
    fn sidebar(&self, cx: &Context<Self>) -> AnyElement {
        let d = self.desktop.read(cx);
        let s = &d.state;
        let mut modes = h_flex()
            .gap_1()
            .p_1()
            .rounded(cx.theme().radius_lg)
            .bg(cx.theme().muted);
        for mode in ["rule", "global", "direct"] {
            modes = modes.child(
                self.command_button(
                    format!("mode-{mode}"),
                    oxide_i18n::mode(mode),
                    Command::SetMode(mode.into()),
                    cx,
                )
                .selected(s.settings.mode == mode)
                .when(s.settings.mode == mode, |button| button.primary())
                .flex_1(),
            );
        }
        let toggles = h_flex()
            .gap_2()
            .child(
                Self::panel(cx)
                    .flex_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Icon::new(IconName::Globe))
                            .child(
                                Switch::new("system-proxy")
                                    .small()
                                    .checked(s.settings.system_proxy)
                                    .disabled(d.busy || !d.connected())
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.send(Command::SetSystemProxy(*value), cx)
                                    })),
                            ),
                    )
                    .child(Self::muted(tr!("sidebar.system_proxy"), cx)),
            )
            .child(
                Self::panel(cx)
                    .flex_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Icon::new(IconName::Network))
                            .child(
                                Switch::new("tun")
                                    .small()
                                    .checked(s.settings.tun)
                                    .disabled(d.busy)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.send(Command::SetTun(*value), cx)
                                    })),
                            ),
                    )
                    .child(Self::muted(tr!("sidebar.tun"), cx)),
            );
        let profile = s
            .profiles
            .iter()
            .find(|p| Some(&p.id) == s.active_profile.as_ref());
        let mut graph = h_flex().items_end().h(px(30.)).gap_px();
        let peak = self
            .session
            .traffic
            .iter()
            .map(|(u, d)| u + d)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        for (up, down) in &self.session.traffic {
            graph = graph.child(
                div()
                    .flex_1()
                    .h(px(((*up + *down) as f32 / peak * 28.).max(2.)))
                    .bg(cx.theme().primary.opacity(0.45)),
            );
        }
        v_flex()
            .w(px(248.))
            .flex_shrink_0()
            .h_full()
            .p_3()
            .gap_2()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(modes)
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap_2()
                            .child(toggles)
                            .child(
                                Self::panel(cx)
                                    .id("profile-card")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.navigate(Page::Profiles, w, cx)
                                    }))
                                    .child(Self::muted(tr!("sidebar.profile"), cx))
                                    .child(
                                        div().text_sm().font_medium().truncate().child(
                                            profile
                                                .map(|p| p.name.clone())
                                                .unwrap_or(tr!("profiles.default")),
                                        ),
                                    )
                                    .child(Self::muted(
                                        profile
                                            .map(|p| {
                                                tr!(if p.subscription {
                                                    "profiles.subscription"
                                                } else {
                                                    "profiles.local"
                                                })
                                            })
                                            .unwrap_or(tr!("profiles.add")),
                                        cx,
                                    )),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(self.nav_card(
                                        Page::Proxies,
                                        IconName::Network,
                                        s.engine.groups.len().to_string(),
                                        cx,
                                    ))
                                    .child(self.nav_card(
                                        Page::Rules,
                                        IconName::FileText,
                                        s.engine.rule_count.to_string(),
                                        cx,
                                    )),
                            )
                            .child(
                                Self::panel(cx)
                                    .id("traffic-card")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.navigate(Page::Connections, w, cx)
                                    }))
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .child(Icon::new(IconName::Network))
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .child(s.engine.connection_count.to_string()),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .child(Self::muted(
                                                format!(
                                                    "↑ {}/s",
                                                    bytes(s.engine.traffic.upload_rate)
                                                ),
                                                cx,
                                            ))
                                            .child(Self::muted(
                                                format!(
                                                    "↓ {}/s",
                                                    bytes(s.engine.traffic.download_rate)
                                                ),
                                                cx,
                                            )),
                                    )
                                    .child(graph)
                                    .child(div().text_sm().child(tr!("nav.connections"))),
                            )
                            .child(
                                Self::panel(cx)
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .child(div().text_sm().child("clash-rs"))
                                            .child(
                                                self.command_button(
                                                    "reload",
                                                    tr!("common.reload"),
                                                    Command::Reload,
                                                    cx,
                                                )
                                                .disabled(d.busy),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .child(Self::muted(
                                                if d.connected() {
                                                    oxide_i18n::phase(&s.phase)
                                                } else {
                                                    tr!("status.disconnected")
                                                },
                                                cx,
                                            ))
                                            .child(Self::muted(
                                                format!(":{}", s.settings.mixed_port),
                                                cx,
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(self.nav_card(
                                        Page::Logs,
                                        IconName::FileText,
                                        s.logs.len().to_string(),
                                        cx,
                                    ))
                                    .child(self.nav_card(
                                        Page::Settings,
                                        IconName::Settings,
                                        String::new(),
                                        cx,
                                    )),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        div()
                            .size_2()
                            .flex_shrink_0()
                            .rounded_full()
                            // Align with the glyphs' optical center, below the line-box center.
                            .relative()
                            .top(px(1.))
                            .bg(if d.connected() {
                                cx.theme().green
                            } else {
                                cx.theme().red
                            }),
                    )
                    .child(Self::muted(
                        tr!(if d.connected() {
                            "status.connected"
                        } else {
                            "status.disconnected"
                        }),
                        cx,
                    )),
            )
            .into_any_element()
    }
    fn confirmation_footer(destructive: bool) -> dialog::DialogFooter {
        dialog::DialogFooter::new()
            .child(
                dialog::DialogClose::new().child(Button::new("cancel").label(tr!("common.cancel"))),
            )
            .child(
                dialog::DialogAction::new().child(
                    Button::new("confirm")
                        .primary()
                        .when(destructive, |button| button.danger())
                        .label(tr!("common.confirm")),
                ),
            )
    }
    fn confirm(
        &self,
        title: String,
        description: String,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let view = view.clone();
            let command = command.clone();
            dialog
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .title(title.clone())
                .child(description.clone())
                .footer(Self::confirmation_footer(true))
                .on_ok(move |_, _, cx| {
                    view.update(cx, |this, cx| this.send(command.clone(), cx));
                    true
                })
        });
    }
    fn rename(&self, id: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let view = cx.entity();
        let focus_input = input.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let input = input.clone();
            let view = view.clone();
            let id = id.clone();
            dialog
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .title(tr!("common.rename"))
                .child(Input::new(&input))
                .footer(Self::confirmation_footer(false))
                .on_ok(move |_, _, cx| {
                    let name = input.read(cx).value().to_string();
                    if name.trim().is_empty() {
                        return false;
                    }
                    view.update(cx, |this, cx| {
                        this.send(
                            Command::RenameProfile {
                                id: id.clone(),
                                name,
                            },
                            cx,
                        )
                    });
                    true
                })
        });
        focus_input.update(cx, |input, cx| input.focus(window, cx));
    }
    fn browse(&self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr!("profiles.browse").into()),
        });
        let source = self.source.clone();
        let name = self.name.clone();
        cx.spawn_in(window, async move |_, window| {
            let path = paths.await.ok()?.ok()??.first()?.clone();
            window
                .update(|window, cx| {
                    source.update(cx, |input, cx| {
                        input.set_value(path.to_string_lossy(), window, cx)
                    });
                    name.update(cx, |input, cx| {
                        input.set_value(
                            path.file_stem().unwrap_or_default().to_string_lossy(),
                            window,
                            cx,
                        )
                    });
                })
                .ok()
        })
        .detach();
    }
    fn test(&mut self, name: String, cx: &mut Context<Self>) {
        self.send(
            Command::TestProxy {
                name,
                url: self.test_url.read(cx).value().to_string(),
            },
            cx,
        );
    }
    fn toolbar(&self, cx: &Context<Self>) -> AnyElement {
        let mut bar = h_flex().w_full().gap_2().flex_wrap().items_center();
        if self.session.page != Page::Settings {
            bar = bar.child(
                div().flex_1().min_w(px(160.)).child(
                    Input::new(&self.search)
                        .prefix(IconName::Search)
                        .cleanable(true),
                ),
            );
        }
        match self.session.page {
            Page::Proxies => {
                bar = bar
                    .child(
                        Button::new("sort")
                            .small()
                            .label(tr!(self.session.sort.key()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.sort = this.session.sort.next();
                                this.save_preferences(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("expand")
                            .small()
                            .label(tr!("proxies.expand"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.expanded = this
                                    .desktop
                                    .read(cx)
                                    .state
                                    .engine
                                    .groups
                                    .iter()
                                    .map(|g| g.name.clone())
                                    .collect();
                                this.save_preferences(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("collapse")
                            .small()
                            .label(tr!("proxies.collapse"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.expanded.clear();
                                this.save_preferences(cx);
                                cx.notify();
                            })),
                    );
            }
            Page::Profiles => {
                bar = bar.child(
                    Button::new("refresh-all")
                        .small()
                        .label(tr!("profiles.refresh_all"))
                        .disabled(
                            self.desktop.read(cx).busy
                                || self.desktop.read(cx).state.profiles.is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refresh_queue = this
                                .desktop
                                .read(cx)
                                .state
                                .profiles
                                .iter()
                                .map(|p| p.id.clone())
                                .collect();
                            if let Some(id) = this.refresh_queue.pop_front() {
                                this.send(Command::Refresh { id }, cx);
                            }
                        })),
                )
            }
            Page::Connections => {
                for closed in [false, true] {
                    bar = bar.child(
                        Button::new(if closed { "closed" } else { "active" })
                            .small()
                            .selected(self.session.show_closed == closed)
                            .when(self.session.show_closed == closed, |button| {
                                button.primary()
                            })
                            .label(tr!(if closed {
                                "connections.closed"
                            } else {
                                "connections.active"
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.session.show_closed = closed;
                                cx.notify();
                            })),
                    );
                }
                bar = bar
                    .child(
                        Button::new("pause")
                            .small()
                            .selected(self.session.connections_paused)
                            .when(self.session.connections_paused, |button| button.primary())
                            .label(tr!(if self.session.connections_paused {
                                "common.resume"
                            } else {
                                "common.pause"
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.connections_paused = !this.session.connections_paused;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("order")
                            .small()
                            .label(tr!(if self.session.sort_download {
                                "connections.order_download"
                            } else {
                                "connections.order_time"
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.sort_download = !this.session.sort_download;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("close-all")
                            .small()
                            .label(tr!(if self.session.show_closed {
                                "common.clear"
                            } else {
                                "connections.close_all"
                            }))
                            .disabled(self.desktop.read(cx).busy)
                            .on_click(cx.listener(|this, _, w, cx| {
                                if this.session.show_closed {
                                    this.session.clear_closed();
                                    cx.notify();
                                } else {
                                    this.confirm(
                                        tr!("connections.close_all"),
                                        tr!("connections.close_confirm"),
                                        Command::CloseAllConnections,
                                        w,
                                        cx,
                                    );
                                }
                            })),
                    );
            }
            Page::Logs => {
                bar = bar
                    .child(
                        Button::new("level")
                            .small()
                            .label(tr!(LOG_LEVELS[self.session.log_level]))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.log_level =
                                    (this.session.log_level + 1) % LOG_LEVELS.len();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("pause")
                            .small()
                            .selected(self.session.logs_paused)
                            .when(self.session.logs_paused, |button| button.primary())
                            .label(tr!(if self.session.logs_paused {
                                "common.resume"
                            } else {
                                "common.pause"
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.logs_paused = !this.session.logs_paused;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("clear")
                            .small()
                            .label(tr!("common.clear"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.clear_logs();
                                cx.notify();
                            })),
                    );
            }
            _ => {}
        }
        bar.into_any_element()
    }
}
impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A focused control can disappear when changing pages or become disabled
        // during a command. Keep page shortcuts reachable after that transition.
        if !window.has_active_dialog(cx) && !self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.focus_handle, cx);
        }
        if !self.initialized_port && self.desktop.read(cx).state.revision > 0 {
            self.initialized_port = true;
            let port = self.desktop.read(cx).state.settings.mixed_port.to_string();
            self.port
                .update(cx, |input, cx| input.set_value(port, window, cx));
        }
        if self.clear_import {
            self.clear_import = false;
            for input in [&self.source, &self.name] {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
        if self.locale_generation != oxide_i18n::generation() {
            self.locale_generation = oxide_i18n::generation();
            for (input, key) in [
                (&self.search, "common.search"),
                (&self.source, "profiles.source_placeholder"),
                (&self.name, "profiles.name_placeholder"),
            ] {
                input.update(cx, |input, cx| input.set_placeholder(tr!(key), window, cx));
            }
        }
        let content = match self.session.page {
            Page::Proxies => self.proxies(window, cx),
            Page::Profiles => self.profiles(cx),
            Page::Connections => self.connections(cx),
            Page::Rules => self.rules(cx),
            Page::Logs => self.logs(cx),
            Page::Settings => self.settings(cx),
        };
        let d = self.desktop.read(cx);
        let status = if d.busy {
            tr!("common.working")
        } else if let Some(message) = &d.connecting {
            tr!(message)
        } else if !d.connected() {
            tr!("status.disconnected")
        } else {
            oxide_i18n::phase(&d.state.phase)
        };
        let banner = self
            .error_notice
            .as_ref()
            .map(oxide_i18n::diagnostic)
            .or_else(|| d.error.as_ref().map(oxide_i18n::diagnostic))
            .or_else(|| oxide_i18n::snapshot_error(&d.state));
        let mut main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_flex()
                    .h(px(56.))
                    .flex_shrink_0()
                    .px_4()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().title_bar_border)
                    .bg(cx.theme().title_bar)
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .child(tr!(self.session.page.key())),
                    )
                    .child(Self::muted(status, cx)),
            )
            .child(div().px_4().py_3().child(self.toolbar(cx)));
        main = main.child(
            div()
                .id(SharedString::from(format!(
                    "page-scroll-{}",
                    self.session.page.index()
                )))
                .flex_1()
                .min_h_0()
                .when(
                    !matches!(self.session.page, Page::Proxies | Page::Rules),
                    |el| el.overflow_y_scroll(),
                )
                .when(
                    matches!(self.session.page, Page::Proxies | Page::Rules),
                    |el| el.overflow_hidden(),
                )
                .px_4()
                .pb_4()
                .child(content),
        );
        if let Some(banner) = banner {
            main = main.child(
                h_flex()
                    .mx_4()
                    .flex_shrink_0()
                    .my_2()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().red.opacity(0.25))
                    .bg(cx.theme().red_light)
                    .text_color(cx.theme().red)
                    .gap_2()
                    .child(div().flex_1().min_w_0().text_sm().child(banner))
                    .child(
                        Button::new("dismiss")
                            .ghost()
                            .small()
                            .flex_shrink_0()
                            .label(tr!("common.dismiss"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.error_notice = None;
                                this.desktop.update(cx, |d, cx| {
                                    d.error = None;
                                    cx.notify();
                                });
                                cx.notify();
                            })),
                    ),
            );
        }
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.0.first() {
                    this.navigate(Page::Profiles, window, cx);
                    this.source.update(cx, |input, cx| {
                        input.set_value(path.to_string_lossy(), window, cx)
                    });
                    this.name.update(cx, |input, cx| {
                        input.set_value(
                            path.file_stem().unwrap_or_default().to_string_lossy(),
                            window,
                            cx,
                        )
                    });
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.error_notice = None;
                    this.desktop.update(cx, |desktop, cx| {
                        desktop.error = None;
                        cx.notify();
                    });
                    cx.notify();
                }
                if event.keystroke.modifiers.control {
                    if let Ok(number) = event.keystroke.key.parse::<usize>()
                        && (1..=6).contains(&number)
                    {
                        this.navigate(Page::ALL[number - 1], window, cx);
                        cx.stop_propagation();
                    }
                    if event.keystroke.key == "f" {
                        this.search.update(cx, |input, cx| input.focus(window, cx));
                        cx.stop_propagation();
                    }
                }
            }))
            .when(
                matches!(window.window_decorations(), Decorations::Client { .. }),
                |el| el.child(TitleBar::new().child(div().text_sm().child("Clash Oxide"))),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.sidebar(cx))
                    .child(main),
            )
    }
}

pub fn run(options: oxide_client::lifecycle::DaemonOptions) -> anyhow::Result<()> {
    let Some(activations) = instance::acquire(&options.socket)? else {
        return Ok(());
    };
    let icon = Arc::new(icon::render(128)?);
    gpui_kit::application()
        .with_quit_mode(QuitMode::Explicit)
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit::component::set_locale(&oxide_i18n::locale());
            theme::init(cx);
            let desktop = cx.new(|cx| Desktop::new(options, activations, icon, cx));
            desktop.update(cx, |desktop, cx| desktop.show_window(cx));
            // Retain the desktop session even after the last window is closed.
            cx.on_window_closed(move |cx, _| {
                desktop.update(cx, |desktop, cx| desktop.window_closed(cx));
            })
            .detach();
        });
    Ok(())
}
