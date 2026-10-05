use super::*;

impl AppView {
    fn connection_detail(
        &self,
        connection: Connection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity();
        let closed = self.session.show_closed;
        window.open_dialog(cx, move |dialog, _, cx| {
            let mut fields = v_flex().gap_3();
            for (label, value) in [
                ("connections.hostname", connection.host.clone()),
                ("connections.source", connection.source.clone()),
                ("connections.host", connection.destination.clone()),
                ("connections.network", connection.network.clone()),
                ("connections.chain", connection.chain.clone()),
                ("connections.rule", connection.rule.clone()),
                ("connections.started", connection.started.clone()),
                ("connections.upload", bytes(connection.upload)),
                ("connections.download", bytes(connection.download)),
            ] {
                fields = fields.child(
                    v_flex()
                        .gap_1()
                        .child(Self::muted(tr!(label), cx))
                        .child(div().text_sm().child(value)),
                );
            }
            let text = format!("{connection:#?}");
            let id = connection.id.clone();
            let view = view.clone();
            dialog
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .title(tr!("common.details"))
                .w(px(580.))
                .child(fields)
                .footer(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("copy-connection")
                                .label(tr!("common.copy"))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                                }),
                        )
                        .child(
                            Button::new("close-connection")
                                .label(tr!("common.close"))
                                .disabled(closed)
                                .on_click(move |_, w, cx| {
                                    view.update(cx, |this, cx| {
                                        this.send(Command::CloseConnection { id: id.clone() }, cx)
                                    });
                                    w.close_dialog(cx);
                                }),
                        ),
                )
        });
    }
    pub(super) fn connections(&self, cx: &Context<Self>) -> AnyElement {
        let rows = self.session.filtered_connections();
        let s = &self.desktop.read(cx).state;
        let mut table = v_flex()
            .gap_1()
            .child(Self::muted(
                if self.session.show_closed {
                    tr!("connections.history_hint")
                } else {
                    tr!(
                        "connections.limit",
                        shown = rows.len(),
                        total = s.engine.connection_count
                    )
                },
                cx,
            ))
            .child(
                h_flex()
                    .p_2()
                    .gap_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().flex_1().child(tr!("connections.host")))
                    .child(div().w(px(75.)).child(tr!("connections.network")))
                    .child(div().w(px(130.)).child(tr!("connections.chain")))
                    .child(div().w(px(85.)).child(tr!("connections.upload")))
                    .child(div().w(px(85.)).child(tr!("connections.download"))),
            );
        for c in rows {
            let connection = c.clone();
            table = table.child(
                h_flex()
                    .id(SharedString::from(format!("connection-{}", c.id)))
                    .p_3()
                    .gap_3()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().colors.list)
                    .cursor_pointer()
                    .hover(|el| el.bg(cx.theme().accent))
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(if c.host.is_empty() {
                                c.destination.clone()
                            } else {
                                c.host.clone()
                            }),
                    )
                    .child(div().w(px(75.)).child(c.network.clone()))
                    .child(div().w(px(130.)).truncate().child(c.chain.clone()))
                    .child(div().w(px(85.)).child(bytes(c.upload)))
                    .child(div().w(px(85.)).child(bytes(c.download)))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.connection_detail(connection.clone(), w, cx)
                    })),
            );
        }
        if self.session.filtered_connections().is_empty() {
            table = table.child(Self::empty(tr!("connections.none"), String::new(), cx));
        }
        table.into_any_element()
    }
}
