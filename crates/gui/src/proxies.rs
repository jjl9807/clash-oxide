use super::*;
use oxide_client::view::Sort;
use oxide_model::{ProxyGroup, Snapshot};
use std::collections::BTreeSet;

enum ProxyRow {
    Header { group: usize, open: bool },
    Nodes { group: usize, names: Vec<String> },
    Footer { automatic: bool },
    Gap,
}

/// Cache the row projection between state changes, including during wheel/drag
/// frames. A large expanded group is split into independently virtualized rows.
pub(super) struct ProxyList {
    revision: u64,
    columns: usize,
    rem: Pixels,
    query: String,
    sort: Sort,
    expanded: BTreeSet<String>,
    mode: String,
    profile: Option<String>,
    rows: Rc<Vec<ProxyRow>>,
    sizes: Rc<Vec<gpui_kit::Size<Pixels>>>,
    scroll: VirtualListScrollHandle,
}

impl Default for ProxyList {
    fn default() -> Self {
        Self {
            revision: 0,
            columns: 0,
            rem: px(0.),
            query: String::new(),
            sort: Sort::default(),
            expanded: BTreeSet::new(),
            mode: String::new(),
            profile: None,
            rows: Rc::default(),
            sizes: Rc::default(),
            scroll: VirtualListScrollHandle::new(),
        }
    }
}

impl ProxyList {
    fn prepare(&mut self, session: &Session, state: &Snapshot, columns: usize, rem: Pixels) {
        let query = session.query();
        if self.revision == state.revision
            && self.columns == columns
            && self.rem == rem
            && self.query == query
            && self.sort == session.sort
            && self.expanded == session.expanded
        {
            return;
        }
        if self.query != query
            || self.mode != state.settings.mode
            || self.profile != state.active_profile
        {
            self.scroll.set_offset(Point::default());
        }
        let mut rows = Vec::new();
        let mut sizes = Vec::new();
        let mut push = |row, height: f32| {
            rows.push(row);
            sizes.push(size(px(0.), rem * height));
        };
        for (group_index, group) in state.engine.groups.iter().enumerate() {
            if (state.settings.mode == "global" && group.name != "GLOBAL")
                || (state.settings.mode == "rule" && group.name == "GLOBAL")
            {
                continue;
            }
            let open = session.expanded.contains(&group.name) || !query.is_empty();
            // Collapsed groups don't need to filter, sort, or clone their nodes.
            let members = if open {
                session.members(group, state)
            } else {
                Vec::new()
            };
            if !query.is_empty() && members.is_empty() {
                continue;
            }
            push(
                ProxyRow::Header {
                    group: group_index,
                    open,
                },
                if open { 4.125 } else { 3.625 },
            );
            if open {
                for names in members.chunks(columns) {
                    push(
                        ProxyRow::Nodes {
                            group: group_index,
                            names: names.iter().map(|name| (*name).clone()).collect(),
                        },
                        4.875,
                    );
                }
                push(
                    ProxyRow::Footer {
                        automatic: !group.selectable,
                    },
                    if group.selectable { 0.75 } else { 2.25 },
                );
            }
            push(ProxyRow::Gap, 0.75);
        }
        self.revision = state.revision;
        self.columns = columns;
        self.rem = rem;
        self.query = query.to_owned();
        self.sort = session.sort;
        self.expanded = session.expanded.clone();
        self.mode = state.settings.mode.clone();
        self.profile = state.active_profile.clone();
        self.rows = Rc::new(rows);
        self.sizes = Rc::new(sizes);
    }
}

impl AppView {
    pub(super) fn proxies(&mut self, window: &mut Window, cx: &Context<Self>) -> AnyElement {
        let d = self.desktop.read(cx);
        let s = &d.state;
        if s.active_profile.is_none() {
            return v_flex()
                .gap_3()
                .child(Self::empty(
                    tr!("profiles.default"),
                    tr!(if s.phase == oxide_model::Phase::Running {
                        "profiles.default_hint"
                    } else {
                        "proxies.empty"
                    }),
                    cx,
                ))
                .child(
                    Button::new("add-profile")
                        .primary()
                        .label(tr!("profiles.add"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.navigate(Page::Profiles, window, cx)
                        })),
                )
                .into_any_element();
        }
        if s.settings.mode == "direct" {
            return Self::empty(tr!("mode.direct"), tr!("proxies.direct"), cx);
        }
        if s.engine.groups.is_empty() {
            return Self::empty(tr!("nav.proxies"), tr!("proxies.empty"), cx);
        }
        let rem = window.rem_size();
        // Sidebar, page padding, scrollbar gutter, and the node row's padding.
        let node_width = window.viewport_size().width - px(248.) - rem * 4.5 - px(2.);
        let columns = ((node_width + rem * 0.5) / (px(206.) + rem * 0.5))
            .floor()
            .max(1.) as usize;
        self.proxy_list.prepare(&self.session, s, columns, rem);
        if self.proxy_list.rows.is_empty() {
            return Self::empty(tr!("common.empty"), String::new(), cx);
        }
        let rows = self.proxy_list.rows.clone();
        let list = v_virtual_list(
            cx.entity(),
            "proxy-rows",
            self.proxy_list.sizes.clone(),
            move |this, range, window, cx| {
                range
                    .map(|index| this.proxy_row(&rows[index], window.rem_size(), cx))
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&self.proxy_list.scroll)
        .pr_4();
        Self::list_viewport(list, &self.proxy_list.scroll)
    }

    fn proxy_row(&self, row: &ProxyRow, rem: Pixels, cx: &Context<Self>) -> AnyElement {
        let d = self.desktop.read(cx);
        let s = &d.state;
        match row {
            ProxyRow::Header { group, open } => {
                let Some(group) = s.engine.groups.get(*group) else {
                    return div().into_any_element();
                };
                let key = group.name.clone();
                let test_key = key.clone();
                v_flex()
                    .w_full()
                    .h(rem * if *open { 4.125 } else { 3.625 })
                    .bg(theme::surface(cx))
                    .border_1()
                    .border_color(cx.theme().border)
                    .when(*open, |el| el.rounded_t(cx.theme().radius_lg).border_b_0())
                    .when(!*open, |el| el.rounded(cx.theme().radius_lg))
                    .child(
                        h_flex()
                            .h(rem * 3.5)
                            .flex_shrink_0()
                            .gap_2()
                            .p_3()
                            .child(
                                Button::new(SharedString::from(format!("group-{key}")))
                                    .ghost()
                                    .flex_1()
                                    .min_w_0()
                                    .justify_start()
                                    .icon(if *open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .child(
                                        div().w_full().truncate().child(format!(
                                            "{}   ·   {}",
                                            group.name, group.selected
                                        )),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.session.toggle_group(&key);
                                        this.save_preferences(cx);
                                    })),
                            )
                            .child(Self::muted(
                                format!(
                                    "{} · {}",
                                    group.kind,
                                    tr!("proxies.members", count = group.members.len())
                                ),
                                cx,
                            ))
                            .child(
                                Button::new(SharedString::from(format!("test-{}", group.name)))
                                    .small()
                                    .label(tr!(if s.engine.testing.as_ref() == Some(&group.name) {
                                        "proxies.testing"
                                    } else {
                                        "proxies.test"
                                    }))
                                    .disabled(d.busy || s.engine.testing.is_some())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.test(test_key.clone(), cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            ProxyRow::Nodes { group, names } => {
                let Some(group) = s.engine.groups.get(*group) else {
                    return div().into_any_element();
                };
                h_flex()
                    .w_full()
                    .h(rem * 4.875)
                    .items_start()
                    .gap_2()
                    .px_3()
                    .pb_2()
                    .bg(theme::surface(cx))
                    .border_x_1()
                    .border_color(cx.theme().border)
                    .children(
                        names
                            .iter()
                            .map(|name| self.proxy_node(group, name, rem, cx)),
                    )
                    .into_any_element()
            }
            ProxyRow::Footer { automatic } => div()
                .w_full()
                .h(rem * if *automatic { 2.25 } else { 0.75 })
                .bg(theme::surface(cx))
                .rounded_b(cx.theme().radius_lg)
                .border_x_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .when(*automatic, |el| {
                    el.child(Self::muted(tr!("proxies.auto"), cx).px_2())
                })
                .into_any_element(),
            ProxyRow::Gap => div().h(rem * 0.75).into_any_element(),
        }
    }

    fn proxy_node(&self, group: &ProxyGroup, name: &str, rem: Pixels, cx: &Context<Self>) -> Div {
        let d = self.desktop.read(cx);
        let s = &d.state;
        let selected = name == group.selected;
        let node = name.to_owned();
        let g = group.name.clone();
        let test_node = name.to_owned();
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
            .unwrap_or_else(|| tr!("proxies.untested"));
        v_flex()
            .flex_1()
            .min_w_0()
            .h(rem * 4.375)
            .p_2()
            .gap_1()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if selected {
                cx.theme().primary
            } else {
                cx.theme().border
            })
            .bg(if selected {
                theme::selected_node(cx)
            } else {
                cx.theme().colors.list
            })
            .child(
                Button::new(SharedString::from(format!("node-{g}-{node}")))
                    .ghost()
                    .small()
                    .w_full()
                    .justify_start()
                    .accessibility_label(node.clone())
                    .child(div().w_full().truncate().child(node.clone()))
                    .disabled(d.busy || !group.selectable)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send(
                            Command::SelectProxy {
                                group: g.clone(),
                                proxy: node.clone(),
                            },
                            cx,
                        )
                    })),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .justify_between()
                    .child(
                        Self::muted(info.map(|p| p.kind.clone()).unwrap_or_default(), cx)
                            .truncate(),
                    )
                    .child(
                        Button::new(SharedString::from(format!("latency-{}-{name}", group.name)))
                            .ghost()
                            .small()
                            .text_color(cx.theme().link)
                            .label(delay)
                            .disabled(d.busy || s.engine.testing.is_some())
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.test(test_node.clone(), cx)),
                            ),
                    ),
            )
    }
}
