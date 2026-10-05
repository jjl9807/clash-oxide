use super::*;

impl AppView {
    pub(super) fn profiles(&self, cx: &Context<Self>) -> AnyElement {
        let d = self.desktop.read(cx);
        let s = &d.state;
        let import = Self::panel(cx)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .w(px(170.))
                            .child(Input::new(&self.name).disabled(d.busy)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.source).disabled(d.busy)),
                    )
                    .child(
                        Button::new("import")
                            .primary()
                            .label(tr!("profiles.import"))
                            .disabled(d.busy || self.source.read(cx).value().trim().is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(Self::muted(tr!("profiles.import_hint"), cx))
                    .child(
                        Button::new("browse")
                            .small()
                            .ghost()
                            .icon(IconName::Folder)
                            .label(tr!("profiles.browse"))
                            .on_click(cx.listener(|this, _, w, cx| this.browse(w, cx))),
                    ),
            );
        let content = v_flex().gap_4().child(import);
        if s.profiles.is_empty() {
            return content
                .child(Self::empty(
                    tr!("profiles.empty"),
                    tr!("profiles.empty_hint"),
                    cx,
                ))
                .into_any_element();
        }
        let mut cards = h_flex().flex_wrap().items_stretch().gap_3();
        for p in &s.profiles {
            if !view::matches(self.session.query(), &[&p.name]) {
                continue;
            }
            let active = s.active_profile.as_ref() == Some(&p.id);
            let id = p.id.clone();
            let rename_id = id.clone();
            let remove_id = id.clone();
            let rename_name = p.name.clone();
            let remove_name = p.name.clone();
            let ago = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .saturating_sub(p.updated_at);
            let age = if ago < 60 {
                format!("{ago}s")
            } else if ago < 3600 {
                format!("{}m", ago / 60)
            } else if ago < 86400 {
                format!("{}h", ago / 3600)
            } else {
                format!("{}d", ago / 86400)
            };
            cards = cards.child(
                Self::panel(cx)
                    .w(px(305.))
                    .flex_grow(1.)
                    .min_w_0()
                    .when(active, |el| {
                        el.border_color(cx.theme().primary).bg(cx.theme().selection)
                    })
                    .child(
                        Button::new(SharedString::from(format!("profile-{id}")))
                            .ghost()
                            .w_full()
                            .justify_start()
                            .accessibility_label(p.name.clone())
                            .child(div().w_full().truncate().child(p.name.clone()))
                            .disabled(d.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !active {
                                    this.send(Command::SwitchProfile { id: id.clone() }, cx);
                                }
                            })),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Self::muted(
                                tr!(if p.subscription {
                                    "profiles.subscription"
                                } else {
                                    "profiles.local"
                                }),
                                cx,
                            ))
                            .child(Self::muted(
                                if p.updated_at == 0 {
                                    tr!("profiles.never_updated")
                                } else {
                                    tr!("profiles.updated", age = age)
                                },
                                cx,
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                self.command_button(
                                    format!("refresh-{}", p.id),
                                    tr!("common.refresh"),
                                    Command::Refresh { id: p.id.clone() },
                                    cx,
                                )
                                .ghost(),
                            )
                            .child(
                                Button::new(SharedString::from(format!("rename-{}", p.id)))
                                    .small()
                                    .ghost()
                                    .label(tr!("common.rename"))
                                    .disabled(d.busy)
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.rename(rename_id.clone(), rename_name.clone(), w, cx)
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("remove-{}", p.id)))
                                    .small()
                                    .ghost()
                                    .label(tr!("common.remove"))
                                    .disabled(d.busy || active)
                                    .tooltip(if active {
                                        tr!("profiles.active_hint")
                                    } else {
                                        tr!("common.remove")
                                    })
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.confirm(
                                            tr!("profiles.delete_title"),
                                            tr!("profiles.delete_hint", name = remove_name),
                                            Command::RemoveProfile {
                                                id: remove_id.clone(),
                                            },
                                            w,
                                            cx,
                                        )
                                    })),
                            ),
                    ),
            );
        }
        content.child(cards).into_any_element()
    }
}
