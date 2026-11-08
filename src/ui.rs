mod attachments;
mod composer;
mod diagram;
mod diff;
mod events;
mod file;
mod icons;
mod input;
mod layout;
mod markdown;
mod menus;
mod messages;
mod session;
mod sidebar;
mod status;
mod syntax;
mod transcript;
mod tree;

use std::{
    f32::consts::{FRAC_PI_2, TAU},
    time::Duration,
};

use async_channel::{Receiver, Sender};
use gpui::{prelude::*, *};
use space::{
    agent::{Command, Event, SidebarCommand},
    model::{Chat, Status},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use self::{
    icons::Icon,
    input::Input,
    tree::{FileTree, OpenFile},
};

actions!(
    space_ui,
    [
        SendMessage,
        Stop,
        NewChat,
        ToggleSidebar,
        BackToChat,
        ConfirmRename,
        CancelRename,
        NextModel,
        PreviousModel,
        ChooseModel,
        Quit,
        Close
    ]
);

pub fn run(
    commands: Sender<Command>,
    sidebar_commands: Sender<SidebarCommand>,
    events: Receiver<Event>,
    shutdown: CancellationToken,
) {
    syntax::initialize();
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("enter", SendMessage, Some("Composer && VimNormal")),
            KeyBinding::new("cmd-enter", SendMessage, Some("Composer && !VimCommand")),
            KeyBinding::new("enter", ConfirmRename, Some("ChatName")),
            KeyBinding::new("escape", CancelRename, Some("ChatName")),
            KeyBinding::new("escape", Stop, None),
            KeyBinding::new("escape", BackToChat, Some("FileView")),
            KeyBinding::new("down", NextModel, Some("ModelMenu")),
            KeyBinding::new("up", PreviousModel, Some("ModelMenu")),
            KeyBinding::new("enter", ChooseModel, Some("ModelMenu")),
            KeyBinding::new("cmd-n", NewChat, None),
            KeyBinding::new("cmd-b", ToggleSidebar, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-w", Close, None),
        ]);
        input::bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1040.), px(760.)), cx);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Fullscreen(bounds)),
                window_min_size: Some(size(px(layout::MIN_WINDOW_WIDTH), px(480.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Space".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    Workspace::new(commands, sidebar_commands, events, shutdown, window, cx)
                })
            },
        );
        if let Err(error) = result {
            eprintln!("Cannot open the window: {error:#}");
            cx.quit();
        }
        cx.activate(true);
    });
}

struct Workspace {
    commands: Sender<Command>,
    sidebar_commands: Sender<SidebarCommand>,
    shutdown: CancellationToken,
    input: Entity<Input>,
    attachments: attachments::AttachmentState,
    submission: Option<composer::Submission>,
    message_edit: Option<messages::MessageEdit>,
    pending_message_version: Option<usize>,
    chat: Option<Chat>,
    file: Option<Entity<file::FileView>>,
    expanded_diagram: Option<diagram::ExpandedDiagram>,
    transcript: transcript::TranscriptState,
    sidebar: sidebar::SidebarState,
    layout: layout::LayoutState,
    chat_menu: Option<(Uuid, Point<Pixels>)>,
    model_menu: Option<Point<Pixels>>,
    model_menu_focus: FocusHandle,
    model_menu_index: usize,
    rename: Option<menus::RenameDialog>,
    error: Option<String>,
    waiting: bool,
    pending_open: bool,
    restoring_view: Option<Uuid>,
    view_loaded: Option<Uuid>,
    workspace_view_loaded: bool,
}

impl Workspace {
    fn new(
        commands: Sender<Command>,
        sidebar_commands: Sender<SidebarCommand>,
        events: Receiver<Event>,
        shutdown: CancellationToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        file::initialize(window);
        cx.on_release(|this, cx| {
            if let Err(error) = this.save_view(cx) {
                eprintln!("Cannot save the workspace view: {error:#}");
            }
        })
        .detach();
        cx.on_app_quit(|this, cx| {
            if let Err(error) = this.save_view(cx) {
                eprintln!("Cannot save the workspace view: {error:#}");
            }
            async {}
        })
        .detach();
        let input = cx.new(Input::new);
        let tree = cx.new(|_| FileTree::new(sidebar_commands.clone()));
        cx.subscribe_in(&tree, window, |this, _, event: &OpenFile, window, cx| {
            this.open_file(event.0.clone(), window, cx);
        })
        .detach();
        let transcript = ListState::new(0, ListAlignment::Bottom, window.viewport_size().height);
        transcript.set_scroll_handler(cx.listener(|this, event: &ListScrollEvent, _, _| {
            this.transcript.follow_output = !event.is_scrolled;
        }));
        input.read(cx).focus_handle(cx).focus(window);
        cx.spawn_in(window, async move |view, cx| {
            while let Ok(event) = events.recv().await {
                if view
                    .update_in(cx, |view, window, cx| {
                        view.event(event, window, cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = view.update(cx, |view, cx| {
                view.error = Some(
                    "The agent worker stopped. Close the window and start Space again.".into(),
                );
                view.waiting = false;
                view.submission = None;
                view.restoring_view = None;
                if let Some(file) = &view.file {
                    file.update(cx, |file, cx| {
                        file.fail(
                            "The file worker stopped. Close the window and start Space again."
                                .into(),
                            cx,
                        );
                    });
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            commands,
            sidebar_commands,
            shutdown,
            input,
            attachments: attachments::AttachmentState::default(),
            submission: None,
            message_edit: None,
            pending_message_version: None,
            chat: None,
            file: None,
            expanded_diagram: None,
            transcript: transcript::TranscriptState::new(transcript),
            sidebar: sidebar::SidebarState::new(tree),
            layout: layout::LayoutState::default(),
            chat_menu: None,
            model_menu: None,
            model_menu_focus: cx.focus_handle(),
            model_menu_index: 0,
            rename: None,
            error: None,
            waiting: true,
            pending_open: false,
            restoring_view: None,
            view_loaded: None,
            workspace_view_loaded: false,
        }
    }

    fn running(&self) -> bool {
        self.chat
            .as_ref()
            .is_some_and(|chat| chat.status == Status::Running)
    }

    fn stop(&mut self) {
        if let Some(chat) = &self.chat
            && self.commands.try_send(Command::Stop(chat.id)).is_err()
        {
            self.error = Some("The agent worker is unavailable".into());
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if self.waiting {
            return;
        }
        let Some(chat) = &self.chat else {
            return;
        };
        if chat.status != Status::Interrupted || chat.archived || chat.contents.is_empty() {
            return;
        }
        if self.commands.try_send(Command::Retry(chat.id)).is_err() {
            self.error = Some("The agent worker is unavailable".into());
        } else {
            self.waiting = true;
            self.error = None;
            self.follow_latest();
        }
        cx.notify();
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.pending_open || self.restoring_view.is_some() {
            return;
        }
        self.chat_menu = None;
        self.model_menu = None;
        self.error = None;
        if self.commands.try_send(command).is_err() {
            self.error = Some("The agent worker is unavailable".into());
        } else {
            self.close_diagram(cx);
            self.pending_open = true;
            self.waiting = true;
        }
        cx.notify();
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if self.layout.dirty {
            let _ = self
                .sidebar_commands
                .try_send(SidebarCommand::SaveSettings(self.layout.settings()));
        }
        self.shutdown.cancel();
    }
}

impl Workspace {
    fn conversation_panel(&self, window: &Window, cx: &Context<Self>) -> Div {
        if let Some(diagram) = &self.expanded_diagram {
            return self.diagram_panel(diagram, cx);
        }
        div()
            .flex_1()
            .relative()
            .min_w(self.conversation_min_width())
            .h_full()
            .flex()
            .flex_col()
            .when_some(self.archived_banner(cx), |view, banner| view.child(banner))
            .when_some(self.error.clone(), |view, error| {
                view.child(div().px_6().py_2().text_color(rgb(0xf0aaaa)).child(error))
            })
            .when_some(self.layout.error.clone(), |view, error| {
                view.child(div().px_6().py_2().text_color(rgb(0xf0aaaa)).child(error))
            })
            .child(self.conversation_panes(window, cx))
            .when_some(
                self.chat
                    .as_ref()
                    .and_then(|chat| chat.cache_notice.clone()),
                |view, notice| {
                    view.child(
                        div()
                            .px_5()
                            .pb_2()
                            .text_xs()
                            .text_color(rgb(0xd5b98c))
                            .child(notice),
                    )
                },
            )
            .child(self.conversation_footer(window, cx))
            .when(
                self.layout.composer_split && !self.layout.composer_expanded,
                |view| {
                    view.on_drag_move::<layout::SplitResize>(cx.listener(Self::resize_split))
                        .child(self.split_resize_handle(window))
                },
            )
    }

    fn conversation_footer(&self, window: &Window, cx: &Context<Self>) -> Div {
        if !self.layout.composer_split || self.layout.composer_expanded {
            return self.usage_footer();
        }
        div()
            .flex()
            .flex_shrink_0()
            .border_t_1()
            .border_color(rgb(0x383842))
            .child(
                div()
                    .w(self.split_chat_width(window))
                    .flex_none()
                    .min_w(px(layout::MIN_CHAT_WIDTH))
                    .px_5()
                    .py_2()
                    .border_r_1()
                    .border_color(rgb(0x383842))
                    .flex()
                    .items_center()
                    .child(self.usage_statistics()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(layout::MIN_INPUT_WIDTH))
                    .px_4()
                    .py_2()
                    .flex()
                    .items_center()
                    .child(self.composer_controls(cx)),
            )
    }

    fn conversation_panes(&self, window: &Window, cx: &Context<Self>) -> Div {
        let split = self.layout.composer_split && !self.layout.composer_expanded;
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .when(!split, |view| view.flex_col())
            .when(!self.layout.composer_expanded, |view| {
                view.child(
                    self.transcript_panel(cx)
                        .min_w(px(layout::MIN_CHAT_WIDTH))
                        .when(split, |view| {
                            view.flex_none()
                                .flex_basis(self.split_chat_width(window))
                                .w(self.split_chat_width(window))
                                .h_full()
                                .border_r_1()
                                .border_color(rgb(0x383842))
                        }),
                )
            })
            .child(if split {
                div()
                    .flex_1()
                    .min_w(px(layout::MIN_INPUT_WIDTH))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.composer(window, cx))
                    .into_any_element()
            } else {
                self.composer(window, cx).into_any_element()
            })
    }

    fn archived_banner(&self, cx: &Context<Self>) -> Option<Div> {
        let chat = self.chat.as_ref().filter(|chat| chat.archived)?;
        let id = chat.id;
        Some(
            div()
                .px_5()
                .py_2()
                .flex()
                .items_center()
                .justify_between()
                .text_xs()
                .text_color(rgb(0x91919f))
                .child("Archived")
                .child(
                    button("unarchive-open-chat", "Unarchive").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.edit_chat(
                                Command::SetArchived {
                                    chat_id: id,
                                    archived: false,
                                },
                                cx,
                            );
                        },
                    )),
                ),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .bg(rgb(0x202025))
            .text_color(rgb(0xe5e5e8))
            .font_family(".AppleSystemUIFont")
            .text_sm()
            .flex()
            .capture_any_mouse_up(cx.listener(Self::finish_resize))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::finish_resize))
            .on_mouse_move(cx.listener(Self::resize_composer))
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::new_chat))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::close_file))
            .on_action(cx.listener(Self::confirm_rename))
            .on_action(cx.listener(Self::cancel_rename))
            .on_action(cx.listener(Self::choose_model))
            .on_action(cx.listener(Self::next_model))
            .on_action(cx.listener(Self::previous_model))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::expand_diagram))
            .on_action(|_: &Close, window, _| window.remove_window())
            .child(self.render_sidebar(window, cx))
            .child(if let Some(file) = &self.file {
                file.clone().into_any_element()
            } else {
                self.conversation_panel(window, cx).into_any_element()
            })
            .when_some(self.render_chat_menu(cx), |view, menu| view.child(menu))
            .when_some(self.render_model_menu(cx), |view, menu| view.child(menu))
            .when_some(self.rename_dialog(cx), |view, dialog| view.child(dialog))
    }
}

fn disclosure_arrow(expanded: bool) -> impl IntoElement {
    if expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    }
    .render(12., 0x9797a5)
}

fn spinner(id: impl Into<ElementId>) -> impl IntoElement {
    div().size(px(12.)).flex_shrink_0().with_animation(
        id,
        Animation::new(Duration::from_millis(900)).repeat(),
        |view, phase| {
            view.child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let radius = (bounds.size.width - px(2.)) / 2.;
                        let center = bounds.center();
                        let start = phase * TAU - FRAC_PI_2;
                        let end = start + TAU * 0.75;
                        let mut path = PathBuilder::stroke(px(1.5));
                        path.move_to(center + point(radius * start.cos(), radius * start.sin()));
                        path.arc_to(
                            point(radius, radius),
                            px(0.),
                            true,
                            true,
                            center + point(radius * end.cos(), radius * end.sin()),
                        );
                        if let Ok(path) = path.build() {
                            window.paint_path(path, rgb(0x9797a5));
                        }
                    },
                )
                .size_full(),
            )
        },
    )
}

fn button(id: impl Into<ElementId>, label: impl IntoElement) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .bg(rgb(0x383842))
        .hover(|style| style.bg(rgb(0x494957)))
        .child(label)
}

fn icon_button(id: impl Into<ElementId>, icon: impl IntoElement) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(0x383842)))
        .child(icon)
}

fn expansion_button(id: impl Into<ElementId>, expanded: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(22.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(0x383842)))
        .child(
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
            .size(px(14.)),
        )
}
