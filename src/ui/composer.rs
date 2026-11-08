use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Submission {
    id: Uuid,
    chat_id: Uuid,
    pub(super) revision: u64,
    content_index: usize,
    text: String,
    branch: Option<Uuid>,
    edited: bool,
}

impl Submission {
    pub(super) fn accepted(&self, chat: &Chat) -> bool {
        chat.id == self.chat_id
            && (!self.edited || chat.active_branch() != self.branch)
            && chat
                .contents
                .get(self.content_index)
                .is_some_and(|content| {
                    content.is_user_message() && content.visible_text() == self.text
                })
    }
}

impl Workspace {
    pub(super) fn send(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        if self.file.is_some()
            || self.expanded_diagram.is_some()
            || self.running()
            || self.waiting
            || self.submission.is_some()
            || self.attachments.loading.is_some()
            || self.rename.is_some()
            || self.chat_menu.is_some()
            || self.model_menu.is_some()
            || self.input.read(cx).is_composing()
            || self.input.read(cx).is_command_mode()
        {
            return;
        }
        let text = self.input.read(cx).text().to_owned();
        if text.is_empty() && self.attachments.pdfs.is_empty() && self.message_edit.is_none() {
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
        let submission = Submission {
            id: submission_id,
            chat_id,
            revision: self.input.read(cx).revision(),
            content_index: self
                .message_edit
                .as_ref()
                .map_or(chat.contents.len(), |edit| edit.content_index),
            text: text.clone(),
            branch: chat.active_branch(),
            edited: self.message_edit.is_some(),
        };
        let command = if let Some(edit) = &self.message_edit {
            Command::EditMessage {
                chat_id,
                submission_id,
                content_index: edit.content_index,
                text,
            }
        } else {
            Command::Send {
                chat_id,
                submission_id,
                text,
                pdfs: self.attachments.pdfs.clone(),
            }
        };
        if self.commands.try_send(command).is_err() {
            self.error = Some("The agent worker is unavailable".into());
        } else {
            self.waiting = true;
            self.error = None;
            self.submission = Some(submission);
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
        window: &mut Window,
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
                if self.message_edit.is_some() {
                    if self.input.read(cx).revision() == revision {
                        self.finish_message_edit(window, cx);
                    }
                } else {
                    self.attachments = Default::default();
                    if self.input.read(cx).revision() == revision {
                        self.input.update(cx, |input, cx| input.clear(cx));
                    }
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
        let split = self.layout.composer_split && !expanded;
        let height = self.layout.composer_height.clamp(
            self.composer_min_height(window),
            Self::composer_max_height(window),
        );
        div()
            .id("composer")
            .relative()
            .min_h_0()
            .min_w(px(layout::MIN_INPUT_WIDTH))
            .flex()
            .flex_col()
            .when(!split, |view| {
                view.rounded_lg()
                    .border_1()
                    .border_color(rgb(0x454550))
                    .bg(rgb(0x27272e))
                    .mx_5()
                    .mb_3()
            })
            .when(expanded, |view| view.flex_1().mt_3())
            .when(split, |view| view.flex_1())
            .when(!expanded && !split, |view| view.h(height).flex_shrink_0())
            .when(!expanded && !split, |view| {
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
            .when(
                self.layout.composer_split && !self.layout.composer_expanded,
                |view| view.p_4(),
            )
            .flex()
            .flex_col()
            .when_some(self.message_edit_banner(cx), |view, banner| {
                view.child(banner)
            })
            .when(!self.attachments.pdfs.is_empty(), |view| {
                view.child(self.pdf_attachments(cx))
            })
            .child(div().flex_1().min_h_0().min_w_0().overflow_hidden().child(
                if self.restoring_view.is_some() {
                    div()
                        .text_xs()
                        .text_color(rgb(0x9797a5))
                        .child("Loading input…")
                        .into_any_element()
                } else {
                    self.input.clone().into_any_element()
                },
            ))
            .when(
                !self.layout.composer_split || self.layout.composer_expanded,
                |view| view.child(self.composer_controls(cx).mt_2()),
            )
    }

    fn composer_layout_controls(&self, cx: &Context<Self>) -> Div {
        let expanded = self.layout.composer_expanded;
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .child(
                expansion_button("expand-composer", expanded)
                    .size(px(28.))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_composer(window, cx))),
            )
            .child(
                icon_button(
                    "split-composer",
                    if self.layout.composer_split {
                        Icon::SplitHorizontal
                    } else {
                        Icon::SplitVertical
                    }
                    .render(14., 0x9797a5),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_composer_split(window, cx);
                })),
            )
    }

    pub(super) fn composer_controls(&self, cx: &Context<Self>) -> Div {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .flex_shrink_0()
            .gap_2()
            .justify_between()
            .items_center()
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .child(self.attachment_button(cx))
                    .child(self.composer_layout_controls(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_auto()
                    .min_w_0()
                    .max_w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(self.model_selector(cx))
                    .child(self.send_button(cx)),
            )
    }

    fn attachment_button(&self, cx: &Context<Self>) -> Stateful<Div> {
        icon_button(
            "attach-pdf",
            if self.attachments.loading.is_some() {
                spinner("attachment-spinner").into_any_element()
            } else {
                Icon::Paperclip.render(16., 0x9797a5).into_any_element()
            },
        )
        .when(!self.can_attach_pdf(), |view| {
            view.opacity(0.4).cursor_default()
        })
        .on_click(cx.listener(|this, _, _, cx| this.attach_pdf(cx)))
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
