use super::layout::SidebarResize;
use super::*;
use space::store::ChatSummary;
use std::collections::HashMap;

pub(super) struct SidebarState {
    pub(super) chats: Vec<ChatSummary>,
    pub(super) runs: HashMap<Uuid, Uuid>,
    pub(super) archived: bool,
    pub(super) error: Option<String>,
    pub(super) files: bool,
    pub(super) tree: Entity<FileTree>,
    pub(super) visible: bool,
}

impl SidebarState {
    pub(super) fn new(tree: Entity<FileTree>) -> Self {
        Self {
            chats: Vec::new(),
            runs: HashMap::new(),
            archived: false,
            error: None,
            files: false,
            tree,
            visible: true,
        }
    }

    pub(super) fn replace_chats(&mut self, chats: Vec<ChatSummary>) {
        self.runs
            .retain(|id, _| chats.iter().any(|chat| chat.id == *id));
        self.chats = chats;
    }

    pub(super) fn record_activity(&mut self, chat_id: Uuid, run_id: Uuid, timestamp: u64) -> bool {
        if self.runs.get(&chat_id) != Some(&run_id) {
            return false;
        }
        if let Some(summary) = self.chats.iter_mut().find(|chat| chat.id == chat_id) {
            summary.updated_at = timestamp;
        }
        self.chats
            .sort_by_key(|chat| std::cmp::Reverse(chat.updated_at));
        true
    }

    pub(super) fn update_chat(&mut self, chat: &Chat, run_id: Option<Uuid>) {
        if let Some(run_id) = run_id {
            self.runs.insert(chat.id, run_id);
        } else {
            self.runs.remove(&chat.id);
        }
        let summary = ChatSummary::from(chat);
        if let Some(existing) = self.chats.iter_mut().find(|item| item.id == chat.id) {
            *existing = summary;
        } else {
            self.chats.push(summary);
        }
        self.chats
            .sort_by_key(|item| std::cmp::Reverse(item.updated_at));
    }
}

impl Workspace {
    pub(super) fn new_chat(&mut self, _: &NewChat, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.is_some() || self.pending_open {
            return;
        }
        self.sidebar.archived = false;
        self.close_file(&BackToChat, window, cx);
        self.command(Command::New, cx);
    }

    pub(super) fn toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.visible = !self.sidebar.visible;
        self.sidebar.tree.update(cx, |tree, cx| {
            tree.set_active(self.sidebar.visible && self.sidebar.files, cx)
        });
        cx.notify();
    }

    pub(super) fn render_sidebar(&self, window: &Window, cx: &Context<Self>) -> Stateful<Div> {
        let max_width = (window.viewport_size().width - px(360.)).clamp(px(180.), px(480.));
        div()
            .id("sidebar")
            .relative()
            .w(self.layout.sidebar_width.clamp(px(180.), max_width))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(0x19191d))
            .border_r_1()
            .border_color(rgb(0x303036))
            .flex()
            .flex_col()
            .on_drag_move::<SidebarResize>(cx.listener(
                move |this, event: &DragMoveEvent<SidebarResize>, _, cx| {
                    this.resize_sidebar(event, max_width, cx);
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(if self.sidebar.files {
                        self.sidebar.tree.clone().into_any_element()
                    } else {
                        self.chat_list(cx).into_any_element()
                    })
                    .when_some(self.sidebar.error.clone(), |view, error| {
                        view.child(div().text_xs().text_color(rgb(0xf0aaaa)).child(error))
                    }),
            )
            .child(self.sidebar_switcher(cx))
            .child(
                div()
                    .id("sidebar-resize")
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(6.))
                    .border_r_1()
                    .border_color(transparent_black())
                    .cursor(CursorStyle::ResizeLeftRight)
                    .hover(|style| style.border_color(rgb(0x626270)))
                    .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| Empty)),
            )
    }

    fn sidebar_switcher(&self, cx: &Context<Self>) -> Stateful<Div> {
        div()
            .id("sidebar-switcher")
            .h(px(28.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(rgb(0x303036))
            .flex()
            .children(
                [
                    ("sidebar-chats", "Chats", false),
                    ("sidebar-files", "Files", true),
                ]
                .into_iter()
                .map(|(id, label, files)| {
                    div()
                        .id(id)
                        .flex_1()
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .cursor_pointer()
                        .text_color(rgb(if self.sidebar.files == files {
                            0xe5e5e8
                        } else {
                            0x8b8b96
                        }))
                        .bg(rgb(if self.sidebar.files == files {
                            0x303039
                        } else {
                            0x19191d
                        }))
                        .hover(|style| style.bg(rgb(0x292930)))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sidebar.files = files;
                            this.sidebar
                                .tree
                                .update(cx, |tree, cx| tree.set_active(files, cx));
                            cx.notify();
                        }))
                }),
            )
    }

    pub(super) fn chat_list(&self, cx: &Context<Self>) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_y_1()
                    .items_center()
                    .justify_between()
                    .child(
                        div().flex().gap_1().children(
                            [
                                ("active-chats", "Chats", false),
                                ("archived-chats", "Archived", true),
                            ]
                            .into_iter()
                            .map(|(id, label, archived)| {
                                button(id, label)
                                    .px_2()
                                    .bg(rgb(if self.sidebar.archived == archived {
                                        0x383842
                                    } else {
                                        0x19191d
                                    }))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.sidebar.archived = archived;
                                        this.chat_menu = None;
                                        cx.notify();
                                    }))
                            }),
                        ),
                    )
                    .child(
                        button("new-chat", new_chat_icon())
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child("New")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_chat(&NewChat, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("chat-list")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .when(
                        !self
                            .sidebar
                            .chats
                            .iter()
                            .any(|chat| chat.archived == self.sidebar.archived),
                        |view| {
                            view.child(
                                div()
                                    .px_2()
                                    .py_2()
                                    .text_xs()
                                    .text_color(rgb(0x8b8b96))
                                    .child(if self.sidebar.archived {
                                        "No archived chats"
                                    } else {
                                        "No chats"
                                    }),
                            )
                        },
                    )
                    .children(
                        self.sidebar
                            .chats
                            .iter()
                            .filter(|chat| chat.archived == self.sidebar.archived)
                            .map(|chat| self.chat_row(chat, cx)),
                    ),
            )
    }

    fn chat_row(&self, chat: &ChatSummary, cx: &Context<Self>) -> Stateful<Div> {
        let id = chat.id;
        let selected = self.chat.as_ref().is_some_and(|active| active.id == id);
        div()
            .id(SharedString::from(id.to_string()))
            .h(px(32.))
            .flex_shrink_0()
            .mb_1()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_sm()
            .cursor_pointer()
            .bg(rgb(if selected { 0x303039 } else { 0x19191d }))
            .hover(|style| style.bg(rgb(0x292930)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .child(chat.title.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .when(chat.status == Status::Running, |view| {
                        view.child(spinner("chat-running"))
                    })
                    .child(
                        div()
                            .id("chat-actions")
                            .size(px(22.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .hover(|style| style.bg(rgb(0x494957)))
                            .child(chat_actions_icon())
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.chat_menu = Some((id, event.position()));
                                cx.notify();
                            })),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.pending_open {
                    return;
                }
                this.close_file(&BackToChat, window, cx);
                this.command(Command::Select(id), cx);
            }))
    }
}

fn new_chat_icon() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |bounds, _, window, _| {
            let center = bounds.center();
            let mut path = PathBuilder::stroke(px(1.5));
            path.move_to(center - point(px(4.), px(0.)));
            path.line_to(center + point(px(4.), px(0.)));
            path.move_to(center - point(px(0.), px(4.)));
            path.line_to(center + point(px(0.), px(4.)));
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(0xe5e5e8));
            }
        },
    )
    .size(px(12.))
    .flex_shrink_0()
}

fn chat_actions_icon() -> impl IntoElement {
    div()
        .flex()
        .gap(px(3.))
        .children((0..3).map(|_| div().size(px(2.)).rounded_full().bg(rgb(0xe5e5e8))))
}
