use super::*;
use space::model::Model;

pub(super) struct RenameDialog {
    chat_id: Uuid,
    input: Entity<Input>,
    error: Option<String>,
}

impl Workspace {
    pub(super) fn edit_chat(&mut self, command: Command, cx: &mut Context<Self>) {
        self.chat_menu = None;
        self.error = None;
        if self.commands.try_send(command).is_err() {
            self.error = Some("The agent worker is unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn start_rename(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.sidebar.chats.iter().find(|chat| chat.id == id) else {
            return;
        };
        let title = chat.title.clone();
        let input = cx.new(|cx| Input::single_line(title, cx));
        input.read(cx).focus_handle(cx).focus(window);
        self.rename = Some(RenameDialog {
            chat_id: id,
            input,
            error: None,
        });
        self.chat_menu = None;
        cx.notify();
    }

    pub(super) fn confirm_rename(
        &mut self,
        _: &ConfirmRename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = &mut self.rename else {
            return;
        };
        if dialog.input.read(cx).is_composing() {
            return;
        }
        let title = dialog.input.read(cx).text().trim().to_owned();
        if title.is_empty() {
            dialog.error = Some("Enter a chat name".into());
            cx.notify();
            return;
        }
        let chat_id = dialog.chat_id;
        self.edit_chat(Command::Rename { chat_id, title }, cx);
        self.cancel_rename(&CancelRename, window, cx);
    }

    pub(super) fn cancel_rename(
        &mut self,
        _: &CancelRename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rename = None;
        if let Some(file) = &self.file {
            file.read(cx).focus_handle(cx).focus(window);
        } else if let Some(diagram) = &self.expanded_diagram {
            diagram.focus_handle(cx).focus(window);
        } else {
            self.input.read(cx).focus_handle(cx).focus(window);
        }
        cx.notify();
    }

    pub(super) fn close_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_menu = None;
        self.input.read(cx).focus_handle(cx).focus(window);
        cx.notify();
    }

    pub(super) fn choose_model(
        &mut self,
        _: &ChooseModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model_menu.is_none() {
            return;
        }
        if let Some(chat) = &self.chat {
            let command = Command::SetModel {
                chat_id: chat.id,
                model: Model::ALL[self.model_menu_index],
            };
            self.edit_chat(command, cx);
        }
        self.close_model_menu(window, cx);
    }

    pub(super) fn model_selector(&self, cx: &Context<Self>) -> impl IntoElement {
        let selected = self
            .chat
            .as_ref()
            .and_then(|chat| Model::from_id(&chat.model));
        let name = selected.map_or("Choose model", Model::name);
        let next = self
            .chat
            .as_ref()
            .is_some_and(|chat| self.running() && chat.model != chat.partial_model.id());
        button(
            "model-selector",
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(div().min_w_0().truncate().child(if next {
                    format!("Next: {name}")
                } else {
                    name.into()
                }))
                .child(disclosure_arrow(true)),
        )
        .min_w_0()
        .max_w_full()
        .flex_shrink_0()
        .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
            cx.stop_propagation();
            if this.model_menu.is_some() {
                this.close_model_menu(window, cx);
            } else if !this.pending_open && this.chat.is_some() {
                this.chat_menu = None;
                this.model_menu_index = Model::ALL
                    .iter()
                    .position(|model| {
                        this.chat
                            .as_ref()
                            .is_some_and(|chat| chat.model == model.id())
                    })
                    .unwrap_or(0);
                this.model_menu = Some(event.position() - point(px(0.), px(18.)));
                this.model_menu_focus.focus(window);
                cx.notify();
            }
        }))
    }

    pub(super) fn render_model_menu(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let position = self.model_menu?;
        Some(
            deferred(
                anchored()
                    .anchor(Corner::BottomRight)
                    .position(position)
                    .child(
                        div()
                            .id("model-menu")
                            .key_context("ModelMenu")
                            .track_focus(&self.model_menu_focus)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(184.))
                            .p_1()
                            .bg(rgb(0x303039))
                            .border_1()
                            .border_color(rgb(0x494957))
                            .rounded_md()
                            .shadow_lg()
                            .occlude()
                            .on_mouse_down_out(
                                cx.listener(|this, _, window, cx| {
                                    this.close_model_menu(window, cx)
                                }),
                            )
                            .children(Model::ALL.into_iter().enumerate().map(|(index, model)| {
                                let selected = self
                                    .chat
                                    .as_ref()
                                    .is_some_and(|chat| chat.model == model.id());
                                div()
                                    .id(index)
                                    .h(px(28.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .rounded_sm()
                                    .text_xs()
                                    .cursor_pointer()
                                    .when(index == self.model_menu_index, |view| {
                                        view.bg(rgb(0x494957))
                                    })
                                    .hover(|style| style.bg(rgb(0x494957)))
                                    .child(model.name())
                                    .when(selected, |view| {
                                        view.child(Icon::Check.render(14., 0xe5e5e8))
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.model_menu_index = index;
                                        this.choose_model(&ChooseModel, window, cx);
                                    }))
                            })),
                    ),
            )
            .into_any_element(),
        )
    }

    pub(super) fn render_chat_menu(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (id, position) = self.chat_menu?;
        let chat = self.sidebar.chats.iter().find(|chat| chat.id == id)?;
        let archived = chat.archived;
        Some(
            deferred(
                anchored().position(position).child(
                    div()
                        .id("chat-menu")
                        .w(px(144.))
                        .p_1()
                        .bg(rgb(0x303039))
                        .border_1()
                        .border_color(rgb(0x494957))
                        .rounded_md()
                        .shadow_lg()
                        .occlude()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.chat_menu = None;
                            cx.notify();
                        }))
                        .children(
                            [
                                "Rename",
                                if archived { "Unarchive" } else { "Archive" },
                                "Delete",
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(action, label)| {
                                div()
                                    .id(action)
                                    .h(px(28.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .rounded_sm()
                                    .text_xs()
                                    .text_color(rgb(if action == 2 { 0xd5a0a0 } else { 0xe5e5e8 }))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0x494957)))
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.chat_menu = None;
                                        match action {
                                            0 => this.start_rename(id, window, cx),
                                            1 => this.edit_chat(
                                                Command::SetArchived {
                                                    chat_id: id,
                                                    archived: !archived,
                                                },
                                                cx,
                                            ),
                                            _ => this.confirm_delete_chat(id, window, cx),
                                        }
                                    }))
                            }),
                        ),
                ),
            )
            .into_any_element(),
        )
    }

    pub(super) fn rename_dialog(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let dialog = self.rename.as_ref()?;
        Some(
            deferred(
                div()
                    .id("rename-overlay")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x00000080))
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.cancel_rename(&CancelRename, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .id("rename-dialog")
                            .w(px(360.))
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .bg(rgb(0x292930))
                            .border_1()
                            .border_color(rgb(0x494957))
                            .rounded_md()
                            .shadow_lg()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child("Rename chat")
                            .child(
                                div()
                                    .h(px(38.))
                                    .px_2()
                                    .py(px(7.))
                                    .bg(rgb(0x202025))
                                    .border_1()
                                    .border_color(rgb(0x626270))
                                    .rounded_sm()
                                    .child(dialog.input.clone()),
                            )
                            .when_some(dialog.error.clone(), |view, error| {
                                view.child(div().text_xs().text_color(rgb(0xf0aaaa)).child(error))
                            })
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(button("cancel-rename", "Cancel").on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.cancel_rename(&CancelRename, window, cx)
                                        },
                                    )))
                                    .child(button("confirm-rename", "Save").on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.confirm_rename(&ConfirmRename, window, cx)
                                        },
                                    ))),
                            ),
                    ),
            )
            .into_any_element(),
        )
    }

    pub(super) fn confirm_delete_chat(
        &mut self,
        id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.waiting {
            return;
        }
        let Some(chat) = self.sidebar.chats.iter().find(|chat| chat.id == id) else {
            return;
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Delete \"{}\"?", chat.title),
            Some("This deletes the saved conversation. You cannot undo this action."),
            &[PromptButton::cancel("Cancel"), PromptButton::new("Delete")],
            cx,
        );
        cx.spawn(async move |view, cx| {
            if answer.await.ok() == Some(1) {
                let _ = view.update(cx, |view, cx| {
                    if view.waiting {
                        return;
                    }
                    if view.chat.as_ref().is_some_and(|chat| chat.id == id) {
                        view.command(Command::Delete(id), cx);
                    } else {
                        view.error = None;
                        if view.commands.try_send(Command::Delete(id)).is_err() {
                            view.error = Some("The agent worker is unavailable".into());
                        }
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }
}

impl Workspace {
    pub(super) fn next_model(&mut self, _: &NextModel, _: &mut Window, cx: &mut Context<Self>) {
        self.model_menu_index = (self.model_menu_index + 1) % Model::ALL.len();
        cx.notify();
    }

    pub(super) fn previous_model(
        &mut self,
        _: &PreviousModel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model_menu_index = (self.model_menu_index + Model::ALL.len() - 1) % Model::ALL.len();
        cx.notify();
    }

    pub(super) fn dismiss(&mut self, _: &Stop, window: &mut Window, cx: &mut Context<Self>) {
        if self.model_menu.is_some() {
            self.close_model_menu(window, cx);
        } else if self.rename.is_some() {
            self.cancel_rename(&CancelRename, window, cx);
        } else if self.chat_menu.take().is_some() {
            cx.notify();
        } else if self.expanded_diagram.is_some() {
            self.close_diagram(cx);
        } else if self.message_edit.is_some() {
            self.cancel_message_edit(window, cx);
        } else if self.layout.composer_expanded {
            self.toggle_composer(window, cx);
        } else {
            self.stop();
            cx.notify();
        }
    }
}
