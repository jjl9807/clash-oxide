use super::*;
use oxide_model::Snapshot;

#[derive(Default)]
pub(super) struct RuleList {
    revision: u64,
    query: String,
    profile: Option<String>,
    indices: Rc<Vec<usize>>,
    scroll: UniformListScrollHandle,
}

impl RuleList {
    fn prepare(&mut self, session: &Session, state: &Snapshot) {
        let query = session.query();
        if self.revision == state.revision && self.query == query {
            return;
        }
        if self.query != query || self.profile != state.active_profile {
            self.scroll.set_offset(Point::default());
        }
        self.indices = Rc::new(
            state
                .engine
                .rules
                .iter()
                .enumerate()
                .filter(|(_, rule)| {
                    view::matches(query, &[&rule.kind, &rule.payload, &rule.target])
                })
                .map(|(index, _)| index)
                .collect(),
        );
        self.revision = state.revision;
        self.query = query.to_owned();
        self.profile = state.active_profile.clone();
    }
}

impl AppView {
    pub(super) fn rules(&mut self, cx: &Context<Self>) -> AnyElement {
        let s = &self.desktop.read(cx).state;
        self.rule_list.prepare(&self.session, s);
        let mut table = v_flex()
            .size_full()
            .gap_1()
            .child(Self::muted(
                tr!("rules.summary", count = s.engine.rule_count),
                cx,
            ))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .mr_4()
                    .p_2()
                    .gap_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().w(px(36.)).child("#"))
                    .child(div().w(px(140.)).child(tr!("rules.type")))
                    .child(div().flex_1().child(tr!("rules.payload")))
                    .child(div().w(px(150.)).child(tr!("rules.target"))),
            );
        let content = if self.rule_list.indices.is_empty() {
            Self::empty(
                tr!(if s.engine.rules.is_empty() {
                    "nav.rules"
                } else {
                    "common.empty"
                }),
                if s.engine.rules.is_empty() {
                    tr!("rules.empty")
                } else {
                    String::new()
                },
                cx,
            )
        } else {
            let indices = self.rule_list.indices.clone();
            let list = uniform_list(
                "rule-rows",
                indices.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                    let s = &this.desktop.read(cx).state;
                    range
                        .map(|index| {
                            let Some(rule) = s.engine.rules.get(indices[index]) else {
                                return div();
                            };
                            div().h(window.rem_size() * 2.5).pb_1().child(
                                h_flex()
                                    .h(window.rem_size() * 2.25)
                                    .p_2()
                                    .gap_3()
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().colors.list)
                                    .text_sm()
                                    .child(div().w(px(36.)).child((indices[index] + 1).to_string()))
                                    .child(div().w(px(140.)).truncate().child(rule.kind.clone()))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(rule.payload.clone()),
                                    )
                                    .child(div().w(px(150.)).truncate().child(rule.target.clone())),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .size_full()
            .pr_4()
            .track_scroll(&self.rule_list.scroll);
            Self::list_viewport(list, &self.rule_list.scroll)
        };
        table = table.child(div().flex_1().min_h_0().child(content));
        if s.engine.rule_count > s.engine.rules.len() {
            table = table.child(Self::muted(tr!("rules.limit"), cx).flex_shrink_0());
        }
        table.into_any_element()
    }
    pub(super) fn logs(&self, cx: &Context<Self>) -> AnyElement {
        let rows = self.session.filtered_logs();
        let mut list = v_flex().gap_1().child(Self::muted(tr!("logs.hint"), cx));
        if rows.is_empty() {
            return list
                .child(Self::empty(tr!("nav.logs"), tr!("logs.empty"), cx))
                .into_any_element();
        }
        for (i, line) in rows.iter().enumerate() {
            let text = (*line).clone();
            list = list.child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .items_start()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().colors.list)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child((*line).clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("copy-log-{i}")))
                            .ghost()
                            .small()
                            .label(tr!("common.copy"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                            }),
                    ),
            );
        }
        list.into_any_element()
    }
}
