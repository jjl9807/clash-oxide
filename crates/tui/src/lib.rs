use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
};
use oxide_client::{
    Update,
    view::{self, LOG_LEVELS, Page, Preferences, Session},
};
use oxide_i18n::{Language, tr};
use oxide_model::*;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Sparkline, Wrap},
};
use std::{
    collections::VecDeque,
    sync::mpsc::{Receiver, SyncSender},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

const BG: Color = Color::Rgb(20, 22, 29);
const PANEL: Color = Color::Rgb(30, 33, 43);
const ACCENT: Color = Color::Rgb(151, 137, 241);
const TEXT: Color = Color::Rgb(221, 223, 233);
const MUTED: Color = Color::Rgb(142, 148, 164);

#[derive(Clone)]
enum Action {
    Page(Page),
    Command(Command),
    Group(String),
    Test(String),
    Search,
    Sort,
    Expand,
    Collapse,
    Import,
    RefreshAll,
    Profile(String),
    Detail(Connection),
    Log(String),
    Pause,
    History(bool),
    Order,
    Clear,
    Level,
    Language(Language),
    Port,
    TestUrl,
    Confirm(Command, String),
    Rename(String, String),
    Cancel,
    Help,
    Quit,
}
#[derive(Clone)]
struct Row {
    text: String,
    action: Option<Action>,
}
impl Row {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            action: None,
        }
    }
    fn button(text: impl Into<String>, action: Action) -> Self {
        Self {
            text: text.into(),
            action: Some(action),
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Sidebar,
    Toolbar,
    Body,
}
#[derive(Clone)]
enum EditKind {
    Search,
    Import,
    Rename(String),
    Port,
    TestUrl,
}
struct Editor {
    kind: EditKind,
    fields: Vec<(String, String)>,
    field: usize,
    cursor: usize,
}
impl Editor {
    fn new(kind: EditKind, fields: Vec<(String, String)>) -> Self {
        let cursor = fields[0].1.chars().count();
        Self {
            kind,
            fields,
            field: 0,
            cursor,
        }
    }
}
struct Modal {
    title: String,
    text: String,
    actions: Vec<Row>,
    selected: usize,
}
struct App {
    state: Snapshot,
    session: Session,
    commands: SyncSender<Command>,
    events: Receiver<Update>,
    zone: Zone,
    side_focus: usize,
    tool_focus: usize,
    selected: usize,
    scroll: usize,
    side: Vec<Row>,
    tools: Vec<Row>,
    rows: Vec<Row>,
    hits: Vec<(Rect, Zone, usize)>,
    editor: Option<Editor>,
    modal: Option<Modal>,
    busy: bool,
    seen: Option<Instant>,
    connecting: Option<String>,
    error: Option<Diagnostic>,
    notice: bool,
    quit: bool,
    test_url: String,
    refresh_queue: VecDeque<String>,
    import_draft: [String; 2],
    pending_import: bool,
    wide: bool,
}
impl App {
    fn save_preferences(&mut self) {
        if let Err(error) = (Preferences {
            test_url: self.test_url.clone(),
            sort: self.session.sort,
            expanded: self.session.expanded.clone(),
        })
        .save()
        {
            self.error = Some(Diagnostic::from_error(&error));
        }
    }
    fn connected(&self) -> bool {
        (self.busy && self.seen.is_some())
            || self
                .seen
                .is_some_and(|seen| seen.elapsed() < Duration::from_secs(5))
    }
    fn send(&mut self, command: Command) {
        if self.busy {
            return;
        }
        match self.commands.try_send(command) {
            Ok(()) => {
                self.busy = true;
                self.error = None;
                self.notice = false;
            }
            Err(_) => {
                self.error = Some(Diagnostic::new(
                    "error.queue_busy",
                    "The command queue is busy. Try again shortly.",
                ))
            }
        }
    }
    fn navigate(&mut self, page: Page) {
        self.session.page = page;
        self.selected = 0;
        self.scroll = 0;
        self.zone = Zone::Body;
        self.editor = None;
        self.modal = None;
    }
    fn edit(&mut self, kind: EditKind, fields: Vec<(String, String)>) {
        self.modal = None;
        self.editor = Some(Editor::new(kind, fields));
    }
    fn confirm(&mut self, command: Command, text: String) {
        self.modal = Some(Modal {
            title: tr!("common.confirm"),
            text,
            actions: vec![
                Row::button(tr!("common.cancel"), Action::Cancel),
                Row::button(tr!("common.confirm"), Action::Command(command)),
            ],
            selected: 0,
        });
    }
    fn activate(&mut self, action: Action) {
        match action {
            Action::Page(page) => self.navigate(page),
            Action::Command(command) => {
                if self.busy {
                    return;
                }
                self.modal = None;
                self.send(command);
            }
            Action::Group(name) => {
                self.session.toggle_group(&name);
                self.save_preferences();
            }
            Action::Test(name) => self.send(Command::TestProxy {
                name,
                url: self.test_url.clone(),
            }),
            Action::Search => self.edit(
                EditKind::Search,
                vec![(tr!("common.search"), self.session.query().into())],
            ),
            Action::Sort => {
                self.session.sort = self.session.sort.next();
                self.save_preferences();
            }
            Action::Expand => {
                self.session.expanded = self
                    .state
                    .engine
                    .groups
                    .iter()
                    .map(|g| g.name.clone())
                    .collect();
                self.save_preferences();
            }
            Action::Collapse => {
                self.session.expanded.clear();
                self.save_preferences();
            }
            Action::Import => self.edit(
                EditKind::Import,
                vec![
                    (
                        tr!("profiles.name_placeholder"),
                        self.import_draft[0].clone(),
                    ),
                    (
                        tr!("profiles.source_placeholder"),
                        self.import_draft[1].clone(),
                    ),
                ],
            ),
            Action::RefreshAll => {
                if !self.busy {
                    self.refresh_queue = self.state.profiles.iter().map(|p| p.id.clone()).collect();
                    if let Some(id) = self.refresh_queue.pop_front() {
                        self.send(Command::Refresh { id });
                    }
                }
            }
            Action::Profile(id) => {
                if let Some(p) = self.state.profiles.iter().find(|p| p.id == id) {
                    let active = self.state.active_profile.as_ref() == Some(&id);
                    let mut actions = Vec::new();
                    if !active {
                        actions.push(Row::button(
                            tr!("profiles.use"),
                            Action::Command(Command::SwitchProfile { id: id.clone() }),
                        ));
                    }
                    actions.push(Row::button(
                        tr!("common.refresh"),
                        Action::Command(Command::Refresh { id: id.clone() }),
                    ));
                    actions.push(Row::button(
                        tr!("common.rename"),
                        Action::Rename(id.clone(), p.name.clone()),
                    ));
                    if !active {
                        actions.push(Row::button(
                            tr!("common.remove"),
                            Action::Confirm(
                                Command::RemoveProfile { id: id.clone() },
                                tr!("profiles.delete_hint", name = p.name),
                            ),
                        ));
                    }
                    actions.push(Row::button(tr!("common.cancel"), Action::Cancel));
                    self.modal = Some(Modal {
                        title: p.name.clone(),
                        text: tr!(if active {
                            "profiles.selected"
                        } else if p.subscription {
                            "profiles.subscription"
                        } else {
                            "profiles.local"
                        }),
                        actions,
                        selected: 0,
                    });
                }
            }
            Action::Detail(c) => {
                let text = format!(
                    "{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n↑ {}   ↓ {}",
                    tr!("connections.host"),
                    if c.host.is_empty() {
                        &c.destination
                    } else {
                        &c.host
                    },
                    tr!("connections.source"),
                    c.source,
                    tr!("connections.network"),
                    c.network,
                    tr!("connections.chain"),
                    c.chain,
                    tr!("connections.rule"),
                    c.rule,
                    tr!("connections.started"),
                    c.started,
                    bytes(c.upload),
                    bytes(c.download)
                );
                let mut actions = vec![Row::button(tr!("common.cancel"), Action::Cancel)];
                if !self.session.show_closed {
                    actions.push(Row::button(
                        tr!("common.close"),
                        Action::Command(Command::CloseConnection { id: c.id.clone() }),
                    ));
                }
                self.modal = Some(Modal {
                    title: tr!("common.details"),
                    text,
                    actions,
                    selected: 0,
                });
            }
            Action::Log(text) => {
                self.modal = Some(Modal {
                    title: tr!("nav.logs"),
                    text,
                    actions: vec![Row::button(tr!("common.close"), Action::Cancel)],
                    selected: 0,
                })
            }
            Action::Pause => {
                if self.session.page == Page::Logs {
                    self.session.logs_paused = !self.session.logs_paused;
                } else {
                    self.session.connections_paused = !self.session.connections_paused;
                }
            }
            Action::History(closed) => {
                self.session.show_closed = closed;
                self.selected = 0;
                self.scroll = 0;
            }
            Action::Order => self.session.sort_download = !self.session.sort_download,
            Action::Clear => {
                if self.session.page == Page::Logs {
                    self.session.clear_logs();
                } else {
                    self.session.clear_closed();
                }
                self.selected = 0;
                self.scroll = 0;
            }
            Action::Level => {
                self.session.log_level = (self.session.log_level + 1) % LOG_LEVELS.len()
            }
            Action::Language(language) => {
                if let Err(e) = oxide_i18n::set_language(language) {
                    self.error = Some(Diagnostic::from_error(&e));
                }
            }
            Action::Port => self.edit(
                EditKind::Port,
                vec![(
                    tr!("settings.apply_port"),
                    self.state.settings.mixed_port.to_string(),
                )],
            ),
            Action::TestUrl => self.edit(
                EditKind::TestUrl,
                vec![(tr!("settings.test_url"), self.test_url.clone())],
            ),
            Action::Confirm(command, text) => self.confirm(command, text),
            Action::Rename(id, name) => {
                self.edit(EditKind::Rename(id), vec![(tr!("common.rename"), name)])
            }
            Action::Cancel => {
                self.modal = None;
                self.editor = None;
            }
            Action::Help => {
                self.modal = Some(Modal {
                    title: tr!("cli.help"),
                    text: tr!("tui.help"),
                    actions: vec![Row::button(tr!("common.close"), Action::Cancel)],
                    selected: 0,
                })
            }
            Action::Quit => self.quit = true,
        }
    }
    fn controls(&self) -> (Vec<Row>, Vec<Row>) {
        let s = &self.state;
        let mut side = vec![Row::text(format!(
            "clash-rs · {}",
            if self.connected() {
                oxide_i18n::phase(&s.phase)
            } else {
                tr!("status.disconnected")
            }
        ))];
        for mode in ["rule", "global", "direct"] {
            side.push(Row::button(
                format!(
                    "{} {}",
                    if s.settings.mode == mode {
                        "●"
                    } else {
                        "○"
                    },
                    oxide_i18n::mode(mode)
                ),
                Action::Command(Command::SetMode(mode.into())),
            ));
        }
        side.push(Row::button(
            format!(
                "[{}] {}",
                if s.settings.system_proxy { "x" } else { " " },
                tr!("sidebar.system_proxy")
            ),
            Action::Command(Command::SetSystemProxy(!s.settings.system_proxy)),
        ));
        side.push(Row::button(
            format!(
                "[{}] {}",
                if s.settings.tun { "x" } else { " " },
                tr!("sidebar.tun")
            ),
            Action::Command(Command::SetTun(!s.settings.tun)),
        ));
        for (i, page) in Page::ALL.iter().enumerate() {
            side.push(Row::button(
                format!("F{} {}", i + 1, tr!(page.key())),
                Action::Page(*page),
            ));
        }
        side.push(Row::button(
            tr!("common.reload"),
            Action::Command(Command::Reload),
        ));
        let mut tools = vec![Row::button(tr!("common.search"), Action::Search)];
        match self.session.page {
            Page::Proxies => tools.extend([
                Row::button(tr!(self.session.sort.key()), Action::Sort),
                Row::button(tr!("proxies.expand"), Action::Expand),
                Row::button(tr!("proxies.collapse"), Action::Collapse),
            ]),
            Page::Profiles => tools.extend([
                Row::button(tr!("profiles.add"), Action::Import),
                Row::button(tr!("profiles.refresh_all"), Action::RefreshAll),
            ]),
            Page::Connections => {
                tools.extend([
                    Row::button(
                        tr!(if self.session.show_closed {
                            "connections.active"
                        } else {
                            "connections.closed"
                        }),
                        Action::History(!self.session.show_closed),
                    ),
                    Row::button(
                        tr!(if self.session.connections_paused {
                            "common.resume"
                        } else {
                            "common.pause"
                        }),
                        Action::Pause,
                    ),
                    Row::button(
                        tr!(if self.session.sort_download {
                            "connections.order_download"
                        } else {
                            "connections.order_time"
                        }),
                        Action::Order,
                    ),
                ]);
                tools.push(if self.session.show_closed {
                    Row::button(tr!("common.clear"), Action::Clear)
                } else {
                    Row::button(
                        tr!("connections.close_all"),
                        Action::Confirm(
                            Command::CloseAllConnections,
                            tr!("connections.close_confirm"),
                        ),
                    )
                });
            }
            Page::Logs => tools.extend([
                Row::button(tr!(LOG_LEVELS[self.session.log_level]), Action::Level),
                Row::button(
                    tr!(if self.session.logs_paused {
                        "common.resume"
                    } else {
                        "common.pause"
                    }),
                    Action::Pause,
                ),
                Row::button(tr!("common.clear"), Action::Clear),
            ]),
            Page::Settings => {
                tools = vec![
                    Row::button(tr!("cli.help"), Action::Help),
                    Row::button(tr!("tui.quit"), Action::Quit),
                ]
            }
            _ => {}
        }
        (side, tools)
    }
    fn rows(&self, width: u16) -> Vec<Row> {
        let s = &self.state;
        let mut rows = Vec::new();
        match self.session.page {
            Page::Proxies => {
                if s.settings.mode == "direct" {
                    return vec![Row::text(tr!("proxies.direct"))];
                }
                if s.active_profile.is_none() {
                    return vec![
                        Row::text(tr!(if s.phase == Phase::Running {
                            "profiles.default_hint"
                        } else {
                            "proxies.empty"
                        })),
                        Row::button(tr!("profiles.add"), Action::Import),
                    ];
                }
                if s.engine.groups.is_empty() {
                    return vec![
                        Row::text(tr!("proxies.empty")),
                        Row::button(tr!("profiles.add"), Action::Import),
                    ];
                }
                for group in &s.engine.groups {
                    if s.settings.mode == "global" && group.name != "GLOBAL"
                        || s.settings.mode == "rule" && group.name == "GLOBAL"
                    {
                        continue;
                    }
                    let members = self.session.members(group, s);
                    if !self.session.query().is_empty() && members.is_empty() {
                        continue;
                    }
                    let open = self.session.expanded.contains(&group.name)
                        || !self.session.query().is_empty();
                    rows.push(Row::button(
                        format!(
                            "{} {}  · {}  → {}",
                            if open { "▾" } else { "▸" },
                            group.name,
                            group.kind,
                            group.selected
                        ),
                        Action::Group(group.name.clone()),
                    ));
                    if open {
                        rows.push(Row::button(
                            format!(
                                "  [{}]",
                                tr!(if s.engine.testing.as_ref() == Some(&group.name) {
                                    "proxies.testing"
                                } else {
                                    "proxies.test"
                                })
                            ),
                            Action::Test(group.name.clone()),
                        ));
                        for name in members {
                            let info = s.engine.proxies.get(name);
                            let delay = info
                                .and_then(|p| p.delay)
                                .map(|d| {
                                    if d == 0 {
                                        tr!("proxies.failed")
                                    } else {
                                        format!("{d} ms")
                                    }
                                })
                                .unwrap_or("—".into());
                            let text = format!(
                                "  {} {} {}",
                                if name == &group.selected {
                                    "●"
                                } else {
                                    "○"
                                },
                                fit(name, width.saturating_sub(20) as usize),
                                delay
                            );
                            rows.push(if group.selectable {
                                Row::button(
                                    text,
                                    Action::Command(Command::SelectProxy {
                                        group: group.name.clone(),
                                        proxy: name.clone(),
                                    }),
                                )
                            } else {
                                Row::text(text)
                            });
                        }
                        rows.push(Row::text(""));
                    }
                }
            }
            Page::Profiles => {
                if s.profiles.is_empty() {
                    rows.push(Row::text(tr!("profiles.empty")));
                    rows.push(Row::button(tr!("profiles.add"), Action::Import));
                }
                for p in s
                    .profiles
                    .iter()
                    .filter(|p| view::matches(self.session.query(), &[&p.name]))
                {
                    rows.push(Row::button(
                        format!(
                            "{} {}  · {}",
                            if Some(&p.id) == s.active_profile.as_ref() {
                                "●"
                            } else {
                                "○"
                            },
                            p.name,
                            tr!(if p.subscription {
                                "profiles.subscription"
                            } else {
                                "profiles.local"
                            })
                        ),
                        Action::Profile(p.id.clone()),
                    ));
                    rows.push(Row::text(""));
                }
            }
            Page::Connections => {
                let host = width.saturating_sub(38).max(12) as usize;
                rows.push(Row::text(format!(
                    "{} {} {} {}",
                    fit(&tr!("connections.host"), host),
                    fit(&tr!("connections.network"), 6),
                    fit(&tr!("connections.upload"), 12),
                    tr!("connections.download")
                )));
                for c in self.session.filtered_connections() {
                    rows.push(Row::button(
                        format!(
                            "{} {} {} {}",
                            fit(
                                if c.host.is_empty() {
                                    &c.destination
                                } else {
                                    &c.host
                                },
                                host
                            ),
                            fit(&c.network, 6),
                            fit(&bytes(c.upload), 12),
                            bytes(c.download)
                        ),
                        Action::Detail(c.clone()),
                    ));
                }
                if rows.len() == 1 {
                    rows.push(Row::text(tr!("connections.none")));
                }
            }
            Page::Rules => {
                let payload = width.saturating_sub(38).max(10) as usize;
                rows.push(Row::text(tr!("rules.summary", count = s.engine.rule_count)));
                for (i, r) in s.engine.rules.iter().enumerate().filter(|(_, r)| {
                    view::matches(self.session.query(), &[&r.kind, &r.payload, &r.target])
                }) {
                    rows.push(Row::text(format!(
                        "{:>4} {} {} {}",
                        i + 1,
                        fit(&r.kind, 15),
                        fit(&r.payload, payload),
                        r.target
                    )));
                }
                if s.engine.rules.is_empty() {
                    rows.push(Row::text(tr!("rules.empty")));
                }
            }
            Page::Logs => {
                rows.extend(
                    self.session
                        .filtered_logs()
                        .into_iter()
                        .map(|l| Row::button(l.clone(), Action::Log(l.clone()))),
                );
                if rows.is_empty() {
                    rows.push(Row::text(tr!("logs.empty")));
                }
            }
            Page::Settings => {
                rows.push(Row::text(tr!("settings.general")));
                rows.push(Row::text(tr!("language.title")));
                for language in Language::ALL {
                    rows.push(Row::button(
                        format!(
                            "  {} {}",
                            if oxide_i18n::language() == language {
                                "●"
                            } else {
                                "○"
                            },
                            language.label()
                        ),
                        Action::Language(language),
                    ));
                }
                rows.extend([
                    Row::text(""),
                    Row::text(tr!("settings.network")),
                    Row::button(
                        tr!("settings.port", port = s.settings.mixed_port),
                        Action::Port,
                    ),
                    Row::text(tr!("settings.tun_hint")),
                    Row::text(""),
                    Row::button(
                        format!("{}: {}", tr!("settings.test_url"), self.test_url),
                        Action::TestUrl,
                    ),
                    Row::text(tr!("settings.test_hint")),
                    Row::text(""),
                    Row::text(tr!("settings.about")),
                ]);
                rows.extend(s.warnings.iter().cloned().map(Row::text));
            }
        }
        if rows.is_empty() {
            rows.push(Row::text(tr!("common.empty")));
        }
        rows
    }
    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.hits.clear();
        frame.render_widget(
            Block::default().style(Style::default().bg(BG).fg(TEXT)),
            area,
        );
        if area.width < 48 || area.height < 16 {
            frame.render_widget(
                Paragraph::new(tr!("tui.enlarge")).wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        let wide = area.width >= 96 && area.height >= 26;
        self.wide = wide;
        if !wide && self.zone == Zone::Sidebar {
            self.zone = Zone::Body;
        }
        let status = if let Some(key) = &self.connecting {
            tr!(key)
        } else if self.busy {
            tr!("common.working")
        } else {
            tr!(if self.connected() {
                "status.connected"
            } else {
                "status.disconnected"
            })
        };
        frame.render_widget(
            Paragraph::new(format!(" CLASH OXIDE  ·  {status}"))
                .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Rect::new(0, 0, area.width, 1),
        );
        let (side, tools) = self.controls();
        self.side = side;
        self.tools = tools;
        let left = if wide { 28 } else { 0 };
        let top = if wide { 2 } else { 4 };
        if wide {
            frame.render_widget(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(PANEL)),
                Rect::new(0, 2, left, area.height - 4),
            );
            for (i, row) in self.side.iter().enumerate() {
                let y = 3
                    + i as u16
                    + if i >= 6 {
                        2
                    } else if i >= 4 {
                        1
                    } else {
                        0
                    };
                let rect = Rect::new(2, y, left - 4, 1);
                frame.render_widget(
                    Paragraph::new(row.text.clone()).style(choice_style(
                        self.zone == Zone::Sidebar && self.side_focus == i,
                    )),
                    rect,
                );
                self.hits.push((rect, Zone::Sidebar, i));
            }
            let name = self
                .state
                .profiles
                .iter()
                .find(|p| Some(&p.id) == self.state.active_profile.as_ref())
                .map(|p| p.name.as_str())
                .unwrap_or("—");
            frame.render_widget(
                Paragraph::new(format!(
                    "{}\n{}\n↑ {}/s\n↓ {}/s",
                    tr!("sidebar.profile"),
                    fit(name, 24),
                    bytes(self.state.engine.traffic.upload_rate),
                    bytes(self.state.engine.traffic.download_rate)
                ))
                .style(Style::default().fg(MUTED)),
                Rect::new(2, 20, 24, 4),
            );
            if area.height > 29 {
                let traffic: Vec<_> = self.session.traffic.iter().map(|(u, d)| u + d).collect();
                frame.render_widget(
                    Sparkline::default()
                        .data(&traffic)
                        .style(Style::default().fg(ACCENT)),
                    Rect::new(2, 25, 24, 2),
                );
            }
        } else {
            let cell_width = area.width / Page::ALL.len() as u16;
            for (i, page) in Page::ALL.iter().enumerate() {
                let rect = Rect::new(i as u16 * cell_width, 2, cell_width, 1);
                frame.render_widget(
                    Paragraph::new(fit(
                        &format!("F{} {}", i + 1, tr!(page.key())),
                        cell_width.saturating_sub(1) as usize,
                    ))
                    .style(if *page == self.session.page {
                        choice_style(true)
                    } else {
                        Style::default().fg(MUTED)
                    }),
                    rect,
                );
                self.hits.push((rect, Zone::Sidebar, 6 + i));
            }
            // Global controls stay available through F8 and m/t/s and the toolbar focus zone.
        }
        let x = left + 2;
        let width = area.width.saturating_sub(x + 2);
        frame.render_widget(
            Paragraph::new(tr!(self.session.page.key()))
                .style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
            Rect::new(x, top, width, 1),
        );
        let mut tx = x;
        let mut ty = top + 2;
        self.tool_focus = self.tool_focus.min(self.tools.len().saturating_sub(1));
        for (i, row) in self.tools.iter().enumerate() {
            let text = format!(" {} ", row.text);
            let w = (text.width() as u16).min(width);
            if tx + w > x + width {
                tx = x;
                ty += 2;
            }
            let rect = Rect::new(tx, ty, w, 1);
            frame.render_widget(
                Paragraph::new(text).style(choice_style(
                    self.zone == Zone::Toolbar && self.tool_focus == i,
                )),
                rect,
            );
            self.hits.push((rect, Zone::Toolbar, i));
            tx += w + 1;
        }
        let query_y = ty + 2;
        let query = if self.session.query().is_empty() {
            tr!("tui.shortcuts")
        } else {
            tr!("tui.search_hint", query = self.session.query())
        };
        frame.render_widget(
            Paragraph::new(query).style(Style::default().fg(MUTED)),
            Rect::new(x, query_y, width, 1),
        );
        let body = Rect::new(
            x,
            query_y + 2,
            width,
            area.height.saturating_sub(query_y + 5),
        );
        self.rows = self.rows(width);
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + body.height.max(1) as usize {
            self.scroll = self.selected + 1 - body.height.max(1) as usize;
        }
        for (index, row) in self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(body.height as usize)
        {
            let rect = Rect::new(body.x, body.y + (index - self.scroll) as u16, body.width, 1);
            let selected = self.zone == Zone::Body && self.selected == index;
            frame.render_widget(
                Paragraph::new(row.text.clone()).style(if selected {
                    choice_style(true)
                } else if row.action.is_some() {
                    Style::default().fg(TEXT)
                } else {
                    Style::default().fg(MUTED)
                }),
                rect,
            );
            self.hits.push((rect, Zone::Body, index));
        }
        let message = if let Some(error) = &self.error {
            oxide_i18n::diagnostic(error).replace('\n', " · ")
        } else if let Some(error) = oxide_i18n::snapshot_error(&self.state) {
            error.replace('\n', " · ")
        } else if self.notice {
            tr!("common.done")
        } else {
            tr!("tui.shortcuts")
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(if self.error.is_some() {
                Color::LightRed
            } else {
                MUTED
            })),
            Rect::new(1, area.height - 2, area.width - 2, 1),
        );
        if let Some(editor) = &self.editor {
            if matches!(editor.kind, EditKind::Search) {
                let prefix = tr!("tui.search_hint", query = "");
                frame.render_widget(
                    Paragraph::new(format!("{prefix}{}", editor.fields[0].1))
                        .style(choice_style(true)),
                    Rect::new(x, query_y, width, 1),
                );
                let offset = editor.fields[0]
                    .1
                    .chars()
                    .take(editor.cursor)
                    .collect::<String>()
                    .width()
                    + prefix.width();
                frame.set_cursor_position((x + (offset as u16).min(width - 1), query_y));
            } else {
                let rect = centered(area, 76, 17);
                frame.render_widget(Clear, rect);
                frame.render_widget(
                    Block::default()
                        .title(match editor.kind {
                            EditKind::Import => tr!("profiles.add"),
                            EditKind::Rename(_) => tr!("common.rename"),
                            EditKind::Port => tr!("settings.apply_port"),
                            _ => tr!("settings.test_url"),
                        })
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .style(Style::default().bg(PANEL).fg(TEXT)),
                    rect,
                );
                for (i, (label, value)) in editor.fields.iter().enumerate() {
                    let y = rect.y + 2 + i as u16 * 3;
                    frame.render_widget(
                        Paragraph::new(label.clone()).style(Style::default().fg(MUTED)),
                        Rect::new(rect.x + 2, y, rect.width - 4, 1),
                    );
                    let prefix: String = value.chars().take(editor.cursor).collect();
                    let cells = prefix.width();
                    let max = rect.width.saturating_sub(5) as usize;
                    let start = if i == editor.field {
                        cells.saturating_sub(max)
                    } else {
                        0
                    };
                    let mut skip = 0;
                    let shown: String = value
                        .chars()
                        .skip_while(|c| {
                            if skip < start {
                                skip += c.to_string().width();
                                true
                            } else {
                                false
                            }
                        })
                        .collect();
                    frame.render_widget(
                        Paragraph::new(shown).style(choice_style(i == editor.field)),
                        Rect::new(rect.x + 2, y + 1, rect.width - 4, 1),
                    );
                    if i == editor.field {
                        frame.set_cursor_position((
                            rect.x + 2 + cells.saturating_sub(start).min(max) as u16,
                            y + 1,
                        ));
                    }
                }
                if let Some(error) = &self.error {
                    frame.render_widget(
                        Paragraph::new(oxide_i18n::diagnostic(error))
                            .wrap(Wrap { trim: false })
                            .style(Style::default().fg(Color::LightRed)),
                        Rect::new(rect.x + 2, rect.y + 9, rect.width - 4, 3),
                    );
                }
                frame.render_widget(
                    Paragraph::new(if self.busy {
                        tr!("common.working")
                    } else {
                        tr!("tui.input_hint")
                    })
                    .style(Style::default().fg(MUTED)),
                    Rect::new(rect.x + 2, rect.y + rect.height - 3, rect.width - 4, 2),
                );
            }
        }
        if let Some(modal) = &self.modal {
            let text_lines = modal.text.lines().count() as u16;
            let rect = centered(
                area,
                80,
                (text_lines + modal.actions.len() as u16 * 2 + 7).min(area.height - 2),
            );
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Block::default()
                    .title(modal.title.clone())
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(Style::default().bg(PANEL).fg(TEXT)),
                rect,
            );
            let action_height = (modal.actions.len() as u16 * 2).min(rect.height.saturating_sub(4));
            frame.render_widget(
                Paragraph::new(modal.text.clone()).wrap(Wrap { trim: false }),
                Rect::new(
                    rect.x + 2,
                    rect.y + 2,
                    rect.width - 4,
                    rect.height.saturating_sub(action_height + 4),
                ),
            );
            self.hits.clear();
            for (i, row) in modal.actions.iter().enumerate() {
                let target = Rect::new(
                    rect.x + 2,
                    rect.y + rect.height - action_height - 1 + i as u16 * 2,
                    rect.width - 4,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(row.text.clone()).style(choice_style(modal.selected == i)),
                    target,
                );
                self.hits.push((target, Zone::Body, i));
            }
        }
    }
    fn submit(&mut self, editor: Editor) {
        match editor.kind.clone() {
            EditKind::Search => {
                self.zone = Zone::Body;
                self.selected = 0;
            }
            EditKind::Import => {
                if self.busy {
                    self.editor = Some(editor);
                    return;
                }
                self.import_draft = [editor.fields[0].1.clone(), editor.fields[1].1.clone()];
                if self.import_draft[1].trim().is_empty() {
                    self.editor = Some(editor);
                    return;
                }
                self.pending_import = true;
                self.send(Command::Import {
                    name: self.import_draft[0].clone(),
                    source: self.import_draft[1].clone(),
                });
                self.editor = Some(editor);
            }
            EditKind::Rename(id) => self.send(Command::RenameProfile {
                id,
                name: editor.fields[0].1.clone(),
            }),
            EditKind::Port => self.send(Command::SetMixedPort(
                editor.fields[0].1.parse().unwrap_or(0),
            )),
            EditKind::TestUrl => {
                self.test_url = editor.fields[0].1.clone();
                self.save_preferences();
            }
        }
    }
    fn editor_key(&mut self, key: KeyEvent) {
        let mut editor = self.editor.take().unwrap();
        if key.code == KeyCode::Esc {
            if matches!(editor.kind, EditKind::Search) {
                self.session.queries[self.session.page.index()].clear();
            }
            if matches!(editor.kind, EditKind::Import) {
                self.import_draft = [editor.fields[0].1.clone(), editor.fields[1].1.clone()];
            }
            return;
        }
        if key.code == KeyCode::Enter {
            self.submit(editor);
            return;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            editor.field = (editor.field + 1) % editor.fields.len();
            editor.cursor = editor.fields[editor.field].1.chars().count();
            self.editor = Some(editor);
            return;
        }
        let value = &mut editor.fields[editor.field].1;
        let mut chars: Vec<_> = value.chars().collect();
        editor.cursor = editor.cursor.min(chars.len());
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                chars.clear();
                editor.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if chars.len() < 4096 {
                    chars.insert(editor.cursor, c);
                    editor.cursor += 1;
                }
            }
            KeyCode::Backspace if editor.cursor > 0 => {
                editor.cursor -= 1;
                chars.remove(editor.cursor);
            }
            KeyCode::Delete if editor.cursor < chars.len() => {
                chars.remove(editor.cursor);
            }
            KeyCode::Left => editor.cursor = editor.cursor.saturating_sub(1),
            KeyCode::Right => editor.cursor = (editor.cursor + 1).min(chars.len()),
            KeyCode::Home => editor.cursor = 0,
            KeyCode::End => editor.cursor = chars.len(),
            _ => {}
        }
        *value = chars.into_iter().collect();
        if matches!(editor.kind, EditKind::Search) {
            self.session.queries[self.session.page.index()] = value.clone();
            self.selected = 0;
            self.scroll = 0;
        }
        self.editor = Some(editor);
    }
    fn activate_focused(&mut self) {
        let action = match self.zone {
            Zone::Sidebar => self.side.get(self.side_focus),
            Zone::Toolbar => self.tools.get(self.tool_focus),
            Zone::Body => self.rows.get(self.selected),
        }
        .and_then(|r| r.action.clone());
        if let Some(action) = action {
            self.activate(action);
        }
    }
    fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.editor.is_some() {
            self.editor_key(key);
            return;
        }
        if let Some(modal) = self.modal.as_mut() {
            match key.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Tab | KeyCode::Down | KeyCode::Right => {
                    modal.selected = (modal.selected + 1) % modal.actions.len()
                }
                KeyCode::BackTab | KeyCode::Up | KeyCode::Left => {
                    modal.selected =
                        (modal.selected + modal.actions.len() - 1) % modal.actions.len()
                }
                KeyCode::Enter => {
                    if let Some(action) = modal.actions[modal.selected].action.clone() {
                        self.activate(action);
                    }
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::F(n) if (1..=6).contains(&n) => self.navigate(Page::ALL[n as usize - 1]),
            KeyCode::Char('m') => {
                let mode = match self.state.settings.mode.as_str() {
                    "rule" => "global",
                    "global" => "direct",
                    _ => "rule",
                };
                self.send(Command::SetMode(mode.into()));
            }
            KeyCode::Char('t') => self.send(Command::SetTun(!self.state.settings.tun)),
            KeyCode::Char('s') => {
                self.send(Command::SetSystemProxy(!self.state.settings.system_proxy))
            }
            KeyCode::F(8) => self.send(Command::Reload),
            KeyCode::Tab => {
                self.zone = match self.zone {
                    Zone::Sidebar => Zone::Toolbar,
                    Zone::Toolbar => Zone::Body,
                    Zone::Body => {
                        if self.wide {
                            Zone::Sidebar
                        } else {
                            Zone::Toolbar
                        }
                    }
                }
            }
            KeyCode::BackTab => {
                self.zone = match self.zone {
                    Zone::Sidebar => Zone::Body,
                    Zone::Toolbar => {
                        if self.wide {
                            Zone::Sidebar
                        } else {
                            Zone::Body
                        }
                    }
                    Zone::Body => Zone::Toolbar,
                }
            }
            KeyCode::Enter => self.activate_focused(),
            KeyCode::Char('/') => self.activate(Action::Search),
            KeyCode::Char('?') => self.activate(Action::Help),
            KeyCode::Char('n') => self.activate(Action::Import),
            KeyCode::Char(' ') if matches!(self.session.page, Page::Connections | Page::Logs) => {
                self.activate(Action::Pause)
            }
            KeyCode::Char('r') => {
                if let Some(Action::Profile(id)) =
                    self.rows.get(self.selected).and_then(|r| r.action.clone())
                {
                    self.send(Command::Refresh { id });
                } else {
                    self.send(Command::Reload);
                }
            }
            KeyCode::Char('d') => {
                match self.rows.get(self.selected).and_then(|r| r.action.clone()) {
                    Some(Action::Profile(id))
                        if Some(&id) != self.state.active_profile.as_ref() =>
                    {
                        let name = self
                            .state
                            .profiles
                            .iter()
                            .find(|p| p.id == id)
                            .map(|p| p.name.clone())
                            .unwrap_or_default();
                        self.confirm(
                            Command::RemoveProfile { id },
                            tr!("profiles.delete_hint", name = name),
                        );
                    }
                    Some(Action::Detail(c)) if !self.session.show_closed => {
                        self.confirm(Command::CloseConnection { id: c.id }, tr!("common.close"))
                    }
                    _ => {}
                }
            }
            KeyCode::Esc => {
                self.error = None;
                self.notice = false;
                self.session.queries[self.session.page.index()].clear();
            }
            key @ (KeyCode::Down
            | KeyCode::Up
            | KeyCode::PageDown
            | KeyCode::PageUp
            | KeyCode::Home
            | KeyCode::End) => {
                let (index, len) = match self.zone {
                    Zone::Sidebar => (&mut self.side_focus, self.side.len()),
                    Zone::Toolbar => (&mut self.tool_focus, self.tools.len()),
                    Zone::Body => (&mut self.selected, self.rows.len()),
                };
                *index = match key {
                    KeyCode::Down => (*index + 1).min(len.saturating_sub(1)),
                    KeyCode::Up => index.saturating_sub(1),
                    KeyCode::PageDown => (*index + 10).min(len.saturating_sub(1)),
                    KeyCode::PageUp => index.saturating_sub(10),
                    KeyCode::Home => 0,
                    _ => len.saturating_sub(1),
                };
            }
            _ => {}
        }
    }
    fn event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.key(key),
            Event::Paste(text) => {
                if let Some(editor) = &mut self.editor {
                    let field = &mut editor.fields[editor.field].1;
                    let mut chars: Vec<_> = field.chars().collect();
                    let inserted: Vec<_> = text
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(4096usize.saturating_sub(chars.len()))
                        .collect();
                    let len = inserted.len();
                    chars.splice(editor.cursor..editor.cursor, inserted);
                    editor.cursor += len;
                    *field = chars.into_iter().collect();
                    if matches!(editor.kind, EditKind::Search) {
                        self.session.queries[self.session.page.index()] = field.clone();
                    }
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) if self.editor.is_none() => {
                    if let Some((_, zone, index)) = self
                        .hits
                        .iter()
                        .find(|(rect, _, _)| rect.contains((mouse.column, mouse.row).into()))
                        .cloned()
                    {
                        if let Some(modal) = &mut self.modal {
                            modal.selected = index;
                            if let Some(action) = modal.actions[index].action.clone() {
                                self.activate(action);
                            }
                        } else {
                            self.zone = zone;
                            match zone {
                                Zone::Sidebar => self.side_focus = index,
                                Zone::Toolbar => self.tool_focus = index,
                                Zone::Body => self.selected = index,
                            }
                            self.activate_focused();
                        }
                    }
                }
                MouseEventKind::ScrollDown if self.modal.is_none() && self.editor.is_none() => {
                    self.selected = (self.selected + 3).min(self.rows.len().saturating_sub(1))
                }
                MouseEventKind::ScrollUp if self.modal.is_none() && self.editor.is_none() => {
                    self.selected = self.selected.saturating_sub(3)
                }
                _ => {}
            },
            _ => {}
        }
    }
    fn update(&mut self, event: Update) {
        match event {
            Update::State(state) => {
                self.session.update(&state);
                self.state = *state;
                self.seen = Some(Instant::now());
                self.connecting = None;
            }
            Update::Error(error) => {
                self.error = Some(error);
                self.connecting = None;
            }
            Update::Busy(busy) => self.busy = busy,
            Update::Connecting(key) => self.connecting = Some(key),
            Update::Completed { command, error } => {
                self.notice = error.is_none();
                self.error = error;
                if self.error.is_some() {
                    self.refresh_queue.clear();
                }
                if matches!(command, Command::Import { .. }) && self.pending_import {
                    self.pending_import = false;
                    if self.error.is_none() {
                        self.import_draft = Default::default();
                        self.editor = None;
                        self.navigate(Page::Profiles);
                    }
                }
            }
        }
    }
}
fn choice_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(BG)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(TEXT).bg(PANEL)
    }
}
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new((area.width - w) / 2, (area.height - h) / 2, w, h)
}
fn fit(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for c in text.chars() {
        let n = c.to_string().width();
        if used + n > width {
            break;
        }
        result.push(c);
        used += n;
    }
    result.push_str(&" ".repeat(width.saturating_sub(used)));
    result
}
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        ratatui::restore();
    }
}
pub fn run(options: oxide_client::lifecycle::DaemonOptions) -> Result<()> {
    use std::io::IsTerminal;
    anyhow::ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        Diagnostic::new(
            "error.terminal",
            "TUI requires an interactive terminal. Use `clash-oxide ctl` for scripts."
        )
    );
    let (commands, events) = oxide_client::connect(options);
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard;
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let preferences = Preferences::load();
    let preference_error = preferences.as_ref().err().map(Diagnostic::from_error);
    let preferences = preferences.unwrap_or_default();
    let mut app = App {
        state: Snapshot::default(),
        session: Session::with_preferences(&preferences),
        commands,
        events,
        zone: Zone::Body,
        side_focus: 0,
        tool_focus: 0,
        selected: 0,
        scroll: 0,
        side: vec![],
        tools: vec![],
        rows: vec![],
        hits: vec![],
        editor: None,
        modal: None,
        busy: true,
        seen: None,
        connecting: Some("status.connecting".into()),
        error: preference_error,
        notice: false,
        quit: false,
        test_url: preferences.test_url,
        refresh_queue: VecDeque::new(),
        import_draft: Default::default(),
        pending_import: false,
        wide: true,
    };
    while !app.quit {
        while let Ok(event) = app.events.try_recv() {
            app.update(event);
        }
        if !app.busy
            && let Some(id) = app.refresh_queue.pop_front()
        {
            app.send(Command::Refresh { id });
        }
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(100))? {
            app.event(event::read()?);
        }
    }
    Ok(())
}
