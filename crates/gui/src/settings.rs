use super::*;

impl AppView {
    pub(super) fn settings(&self, cx: &Context<Self>) -> AnyElement {
        let s = &self.desktop.read(cx).state;
        let mut languages = h_flex().gap_2();
        for language in Language::ALL {
            languages = languages.child(
                Button::new(language.id())
                    .small()
                    .selected(oxide_i18n::language() == language)
                    .when(oxide_i18n::language() == language, |button| {
                        button.primary()
                    })
                    .label(language.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.error_notice = oxide_i18n::set_language(language)
                            .err()
                            .map(|error| Diagnostic::from_error(&error));
                        cx.notify();
                    })),
            );
        }
        let mut themes = h_flex().gap_2();
        for preference in theme::Preference::ALL {
            themes = themes.child(
                Button::new(preference.id())
                    .small()
                    .selected(theme::preference(cx) == preference)
                    .when(theme::preference(cx) == preference, |button| {
                        button.primary()
                    })
                    .label(preference.label())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.error_notice = theme::set_preference(preference, window, cx)
                            .err()
                            .map(|error| Diagnostic::from_error(&error));
                        cx.notify();
                    })),
            );
        }
        let mut content = v_flex()
            .gap_4()
            .child(div().font_medium().child(tr!("settings.general")))
            .child(
                Self::panel(cx)
                    .child(
                        h_flex()
                            .gap_4()
                            .justify_between()
                            .child(div().child(tr!("language.title")))
                            .child(languages),
                    )
                    .child(Self::muted(tr!("language.hint"), cx)),
            )
            .child(
                Self::panel(cx)
                    .child(
                        h_flex()
                            .gap_4()
                            .justify_between()
                            .child(div().child(tr!("theme.title")))
                            .child(themes),
                    )
                    .child(Self::muted(tr!("theme.hint"), cx)),
            )
            .child(div().font_medium().child(tr!("settings.network")))
            .child(
                Self::panel(cx)
                    .child(
                        h_flex()
                            .gap_4()
                            .justify_between()
                            .child(
                                v_flex()
                                    .child(tr!("settings.port", port = s.settings.mixed_port))
                                    .child(Self::muted("127.0.0.1 · HTTP / SOCKS5", cx)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().w(px(95.)).child(Input::new(&self.port)))
                                    .child(
                                        Button::new("apply-port")
                                            .small()
                                            .label(tr!("common.save"))
                                            .disabled(self.desktop.read(cx).busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                let port =
                                                    this.port.read(cx).value().parse().unwrap_or(0);
                                                this.send(Command::SetMixedPort(port), cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(Self::muted(tr!("settings.tun_hint"), cx)),
            )
            .child(
                Self::panel(cx)
                    .child(tr!("settings.test_url"))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.test_url)))
                            .child(
                                Button::new("save-test-url")
                                    .small()
                                    .label(tr!("common.save"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_preferences(cx);
                                    })),
                            ),
                    )
                    .child(Self::muted(tr!("settings.test_hint"), cx)),
            )
            .child(Self::muted(tr!("settings.about"), cx))
            .child(Self::muted(tr!("ui.shortcuts"), cx));
        for warning in &s.warnings {
            content = content.child(
                Self::panel(cx)
                    .bg(cx.theme().yellow_light)
                    .text_color(cx.theme().yellow)
                    .child(warning.clone()),
            );
        }
        content.into_any_element()
    }
}
