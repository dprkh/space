use super::*;
use serde::{Deserialize, Serialize};

pub(super) struct MessageEdit {
    pub(super) content_index: usize,
    draft: Entity<Input>,
    attachments: attachments::AttachmentState,
}

#[derive(Deserialize, Serialize)]
pub(super) struct SavedMessageEdit {
    content_index: usize,
    draft: input::InputState,
    pdfs: Vec<space::pdf::Pdf>,
}

impl MessageEdit {
    pub(super) fn saved_state(&self, cx: &App) -> SavedMessageEdit {
        SavedMessageEdit {
            content_index: self.content_index,
            draft: self.draft.read(cx).saved_state(),
            pdfs: self.attachments.pdfs.clone(),
        }
    }
}

impl SavedMessageEdit {
    pub(super) fn restore(
        self,
        chat: &Chat,
        cx: &mut Context<Workspace>,
    ) -> anyhow::Result<MessageEdit> {
        anyhow::ensure!(
            chat.contents
                .get(self.content_index)
                .is_some_and(|content| content.is_user_message()),
            "The edited message no longer exists"
        );
        let draft = cx.new(Input::new);
        draft.update(cx, |input, cx| input.restore_state(self.draft, cx))?;
        Ok(MessageEdit {
            content_index: self.content_index,
            draft,
            attachments: attachments::AttachmentState {
                pdfs: self.pdfs,
                loading: None,
            },
        })
    }

    pub(super) fn restore_draft(
        self,
        cx: &mut Context<Workspace>,
    ) -> anyhow::Result<(Entity<Input>, attachments::AttachmentState)> {
        let draft = cx.new(Input::new);
        draft.update(cx, |input, cx| input.restore_state(self.draft, cx))?;
        Ok((
            draft,
            attachments::AttachmentState {
                pdfs: self.pdfs,
                loading: None,
            },
        ))
    }
}

impl Workspace {
    pub(super) fn copy_message(&self, content_index: usize, cx: &mut Context<Self>) {
        let Some(chat) = &self.chat else {
            return;
        };
        let text = if content_index == chat.contents.len() {
            chat.partial.clone()
        } else if let Some(content) = chat.contents.get(content_index) {
            content.visible_text()
        } else {
            return;
        };
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(super) fn confirm_delete_message(
        &mut self,
        content_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_change_branch() || self.input.read(cx).is_composing() {
            return;
        }
        let Some(chat) = self.chat.as_ref().filter(|chat| !chat.archived) else {
            return;
        };
        let chat_id = chat.id;
        let branch = chat.active_branch();
        let updated_at = chat.updated_at;
        let answer = window.prompt(
            PromptLevel::Warning,
            "Delete this message and all later messages?",
            Some("Versions that start at this message or later are also deleted. You cannot undo this action."),
            &[PromptButton::cancel("Cancel"), PromptButton::new("Delete")],
            cx,
        );
        cx.spawn(async move |view, cx| {
            if answer.await.ok() != Some(1) {
                return;
            }
            let _ = view.update(cx, |view, cx| {
                if !view.can_change_branch()
                    || view.chat.as_ref().is_none_or(|chat| {
                        chat.id != chat_id
                            || chat.archived
                            || chat.active_branch() != branch
                            || chat.updated_at != updated_at
                    })
                {
                    return;
                }
                view.command(
                    Command::DeleteMessage {
                        chat_id,
                        content_index,
                    },
                    cx,
                );
                if view.pending_open {
                    view.transcript.follow_output = true;
                }
            });
        })
        .detach();
    }

    pub(super) fn message_actions(
        &self,
        chat: &Chat,
        content_index: usize,
        cx: &Context<Self>,
    ) -> Div {
        let can_copy = if content_index == chat.contents.len() {
            !chat.partial.is_empty()
        } else {
            chat.contents.get(content_index).is_some_and(|content| {
                content.parts.iter().any(|part| {
                    part["thought"] != true
                        && part["text"].as_str().is_some_and(|text| !text.is_empty())
                })
            })
        };
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .child(
                icon_button("copy-message", Icon::Copy.render(14., 0x9797a5))
                    .when(!can_copy, |view| view.opacity(0.3).cursor_default())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.copy_message(content_index, cx)),
                    ),
            )
            .when(!chat.archived, |view| {
                view.child(
                    icon_button("delete-message", Icon::Trash.render(14., 0x9797a5))
                        .when(!self.can_change_branch(), |view| {
                            view.opacity(0.3).cursor_default()
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_delete_message(content_index, window, cx);
                        })),
                )
            })
    }

    pub(super) fn can_change_branch(&self) -> bool {
        !self.running()
            && !self.waiting
            && !self.pending_open
            && self.submission.is_none()
            && self.attachments.loading.is_none()
            && self.message_edit.is_none()
            && self.file.is_none()
            && self.expanded_diagram.is_none()
            && self.rename.is_none()
            && self.chat_menu.is_none()
            && self.model_menu.is_none()
    }

    pub(super) fn start_message_edit(
        &mut self,
        content_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_change_branch() || self.input.read(cx).is_composing() {
            return;
        }
        let Some(chat) = self.chat.as_ref().filter(|chat| !chat.archived) else {
            return;
        };
        let Some(content) = chat
            .contents
            .get(content_index)
            .filter(|content| content.is_user_message())
        else {
            return;
        };
        let text = content.visible_text();
        let input = cx.new(|cx| Input::with_text(text, cx));
        self.message_edit = Some(MessageEdit {
            content_index,
            draft: std::mem::replace(&mut self.input, input),
            attachments: std::mem::take(&mut self.attachments),
        });
        self.input.read(cx).focus_handle(cx).focus(window);
        self.error = None;
        cx.notify();
    }

    pub(super) fn cancel_message_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submission.is_none() {
            self.finish_message_edit(window, cx);
        }
    }

    pub(super) fn finish_message_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.message_edit.take() else {
            return;
        };
        self.input = edit.draft;
        self.attachments = edit.attachments;
        if self.file.is_none()
            && self.expanded_diagram.is_none()
            && self.rename.is_none()
            && self.model_menu.is_none()
        {
            self.input.read(cx).focus_handle(cx).focus(window);
        }
        cx.notify();
    }

    pub(super) fn select_message_version(
        &mut self,
        content_index: usize,
        version: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.can_change_branch() || self.input.read(cx).is_composing() {
            return;
        }
        let Some(chat) = &self.chat else {
            return;
        };
        let Some((selected, count)) = chat.message_versions(content_index) else {
            return;
        };
        if version >= count || version == selected {
            return;
        }
        self.command(
            Command::SelectMessageVersion {
                chat_id: chat.id,
                content_index,
                version,
            },
            cx,
        );
        if self.pending_open {
            self.pending_message_version = Some(content_index);
        }
    }

    pub(super) fn message_edit_banner(&self, cx: &Context<Self>) -> Option<Div> {
        let edit = self.message_edit.as_ref()?;
        let content = self.chat.as_ref()?.contents.get(edit.content_index)?;
        let pdfs = content
            .parts
            .iter()
            .filter(|part| part["inlineData"]["mimeType"] == "application/pdf")
            .count();
        let label = match pdfs {
            0 => "Edit message".to_owned(),
            1 => "Edit message (1 PDF attached)".to_owned(),
            _ => format!("Edit message ({pdfs} PDFs attached)"),
        };
        Some(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .mb_2()
                .text_xs()
                .text_color(rgb(0x9797a5))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .child(
                    button("cancel-message-edit", "Cancel")
                        .flex_shrink_0()
                        .when(self.submission.is_some(), |view| {
                            view.opacity(0.4).cursor_default()
                        })
                        .on_click(
                            cx.listener(|this, _, window, cx| this.cancel_message_edit(window, cx)),
                        ),
                ),
        )
    }
}
