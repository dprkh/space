use super::*;
use space::store::MIN_COMPOSER_HEIGHT;

pub(super) struct Submission {
    id: Uuid,
    chat_id: Uuid,
    revision: u64,
}

impl Workspace {
    pub(super) fn send(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        if self.file.is_some()
            || self.running()
            || self.waiting
            || self.submission.is_some()
            || self.attachments.loading.is_some()
            || self.rename.is_some()
            || self.chat_menu.is_some()
            || self.model_menu.is_some()
            || self.input.read(cx).is_composing()
        {
            return;
        }
        let text = self.input.read(cx).text().trim().to_owned();
        if text.is_empty() && self.attachments.pdfs.is_empty() {
            return;
        }
        let Some(chat) = &self.chat else {
            return;
        };
        if chat.archived {
            return;
        }
        let chat_id = chat.id;
        let submission_id = Uuid::new_v4();
        if self
            .commands
            .try_send(Command::Send {
                chat_id,
                submission_id,
                text,
                pdfs: self.attachments.pdfs.clone(),
            })
            .is_err()
        {
            self.error = Some("The agent worker is unavailable".into());
        } else {
            self.waiting = true;
            self.error = None;
            self.submission = Some(Submission {
                id: submission_id,
                chat_id,
                revision: self.input.read(cx).revision(),
            });
            if self.layout.composer_expanded {
                self.toggle_composer(window, cx);
            }
            self.follow_latest();
        }
        cx.notify();
    }

    pub(super) fn complete_send(
        &mut self,
        chat_id: Uuid,
        submission_id: Uuid,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let Some(submission) = &self.submission else {
            return;
        };
        if submission.chat_id != chat_id || submission.id != submission_id {
            return;
        }
        let revision = submission.revision;
        self.submission = None;
        match result {
            Ok(()) => {
                self.attachments = Default::default();
                if self.input.read(cx).revision() == revision {
                    self.input.update(cx, |input, cx| input.clear(cx));
                }
            }
            Err(error) => {
                self.waiting = self.pending_open;
                self.error = Some(error);
            }
        }
    }

    pub(super) fn composer(&self, window: &Window, cx: &Context<Self>) -> Stateful<Div> {
        let expanded = self.layout.composer_expanded;
        let height = self
            .layout
            .composer_height
            .clamp(px(MIN_COMPOSER_HEIGHT), Self::composer_max_height(window));
        div()
            .id("composer")
            .relative()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x454550))
            .bg(rgb(0x27272e))
            .mx_5()
            .mb_3()
            .when(expanded, |view| view.flex_1().mt_3())
            .when(!expanded, |view| view.h(height).flex_shrink_0())
            .when(!expanded, |view| {
                view.child(self.composer_resize_handle(cx))
            })
            .child(self.composer_body(cx))
    }

    fn composer_body(&self, cx: &Context<Self>) -> Div {
        div()
            .w_full()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .p_2()
            .flex()
            .flex_col()
            .when(!self.attachments.pdfs.is_empty(), |view| {
                view.child(self.pdf_attachments(cx))
            })
            .child(self.composer_input(cx))
            .child(self.composer_controls(cx))
    }

    fn composer_input(&self, cx: &Context<Self>) -> Div {
        let expanded = self.layout.composer_expanded;
        div()
            .w_full()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .child(self.input.clone()),
            )
            .child(
                div()
                    .id("expand-composer")
                    .size(px(22.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x383842)))
                    .child(composer_icon(expanded))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_composer(window, cx))),
            )
    }

    fn composer_controls(&self, cx: &Context<Self>) -> Div {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_shrink_0()
            .gap_2()
            .justify_between()
            .items_end()
            .mt_1()
            .child(
                button(
                    "attach-pdf",
                    if self.attachments.loading.is_some() {
                        "Loading PDF…"
                    } else {
                        "Attach PDF"
                    },
                )
                .flex_shrink_0()
                .when(!self.can_attach_pdf(), |view| {
                    view.opacity(0.4).cursor_default()
                })
                .on_click(cx.listener(|this, _, _, cx| this.attach_pdf(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(rgb(0x91919f))
                    .child(self.composer_hint()),
            )
            .child(self.model_selector(cx))
            .child(self.send_button(cx))
    }

    fn composer_hint(&self) -> &'static str {
        if self.chat.as_ref().is_some_and(|chat| chat.archived) {
            "Unarchive this chat to send a message"
        } else if self.running() {
            ""
        } else if self.waiting {
            "Loading…"
        } else {
            "Enter to send · Shift+Enter for a new line"
        }
    }

    fn send_button(&self, cx: &Context<Self>) -> Stateful<Div> {
        if self.running() {
            button(
                "stop",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(spinner("response-spinner"))
                    .child("Stop"),
            )
            .flex_shrink_0()
            .on_click(cx.listener(|this, _, _, cx| {
                this.stop();
                cx.notify();
            }))
        } else {
            button("send", "Send")
                .flex_shrink_0()
                .when(
                    self.chat.as_ref().is_some_and(|chat| chat.archived),
                    |button| button.opacity(0.4).cursor_default(),
                )
                .on_click(cx.listener(|this, _, window, cx| this.send(&SendMessage, window, cx)))
        }
    }
}

fn composer_icon(expanded: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let (tip, tail, corner) = if expanded {
                (1.5, 5., 5.)
            } else {
                (5., 0., 1.5)
            };
            let mut path = PathBuilder::stroke(px(1.5));
            for sign in [-1., 1.] {
                path.move_to(center + point(px(tail * sign), px(-tail * sign)));
                path.line_to(center + point(px(tip * sign), px(-tip * sign)));
                path.move_to(center + point(px(corner * sign), px(-tip * sign)));
                path.line_to(center + point(px(tip * sign), px(-tip * sign)));
                path.line_to(center + point(px(tip * sign), px(-corner * sign)));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(0x9797a5));
            }
        },
    )
    .size(px(14.))
}
