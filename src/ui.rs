use std::{
    collections::{HashMap, HashSet},
    f32::consts::{FRAC_PI_2, TAU},
    time::Duration,
};

use async_channel::{Receiver, Sender};
use gpui::{prelude::*, *};
use space::{
    agent::{Command, Event, SidebarCommand},
    model::{CONTEXT_WINDOW_TOKENS, Chat, Role, Status},
    store::{ChatSummary, MAX_COMPOSER_HEIGHT, MIN_COMPOSER_HEIGHT, UiSettings},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    diff::FileDiff,
    input::{self, Input},
    markdown::{CachedText, Markdown},
    tree::FileTree,
};

actions!(
    space_ui,
    [SendMessage, Stop, NewChat, ToggleSidebar, Quit, Close]
);

pub fn run(
    commands: Sender<Command>,
    sidebar_commands: Sender<SidebarCommand>,
    events: Receiver<Event>,
    shutdown: CancellationToken,
) {
    Application::new().run(move |cx: &mut App| {
        input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("enter", SendMessage, Some("Composer")),
            KeyBinding::new("escape", Stop, None),
            KeyBinding::new("cmd-n", NewChat, None),
            KeyBinding::new("cmd-b", ToggleSidebar, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-w", Close, None),
        ]);
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
                window_min_size: Some(size(px(640.), px(480.))),
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

struct SidebarResize;

#[derive(Clone, Copy)]
struct ComposerResize {
    pointer_y: Pixels,
    height: Pixels,
}

const TRANSCRIPT_PADDING: f32 = 20.;

#[derive(Default)]
struct MessageText {
    answer: Markdown,
    thoughts: Markdown,
}

#[derive(Clone)]
enum TranscriptRow {
    User(usize, CachedText),
    ModelHeader(usize),
    Thinking(usize),
    Thought(usize, usize),
    Model(usize, usize),
    Tool(usize, usize, SharedString),
    Diff(usize, usize, usize),
    Notice,
}

impl TranscriptRow {
    fn content_index(&self) -> usize {
        match self {
            Self::User(index, _)
            | Self::ModelHeader(index)
            | Self::Thinking(index)
            | Self::Thought(index, _)
            | Self::Model(index, _)
            | Self::Tool(index, ..)
            | Self::Diff(index, ..) => *index,
            Self::Notice => usize::MAX,
        }
    }

    fn key(&self) -> (u8, usize, usize) {
        match self {
            Self::User(index, _) => (0, *index, 0),
            Self::Model(index, block) => (1, *index, *block),
            Self::Tool(index, part, ..) => (2, *index, *part),
            Self::Notice => (3, 0, 0),
            Self::ModelHeader(index) => (4, *index, 0),
            Self::Thinking(index) => (5, *index, 0),
            Self::Thought(index, block) => (6, *index, *block),
            Self::Diff(_, change, block) => (7, *change, *block),
        }
    }
}

struct Workspace {
    commands: Sender<Command>,
    sidebar_commands: Sender<SidebarCommand>,
    shutdown: CancellationToken,
    cancel: Option<CancellationToken>,
    input: Entity<Input>,
    chat: Option<Chat>,
    markdown: Vec<MessageText>,
    partial_markdown: MessageText,
    collapsed_thoughts: HashSet<usize>,
    diffs: Vec<FileDiff>,
    change_indices: HashMap<(usize, usize), usize>,
    collapsed_changes: HashSet<usize>,
    chats: Vec<ChatSummary>,
    sidebar: bool,
    sidebar_width: Pixels,
    composer_height: Pixels,
    composer_expanded: bool,
    composer_resize: Option<ComposerResize>,
    settings_dirty: bool,
    settings_error: Option<String>,
    sidebar_error: Option<String>,
    files: bool,
    tree: Entity<FileTree>,
    transcript: ListState,
    rows: Vec<TranscriptRow>,
    follow_output: bool,
    error: Option<String>,
    waiting: bool,
    pending_open: bool,
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
        let input = cx.new(Input::new);
        let tree = cx.new(|_| FileTree::new(sidebar_commands.clone()));
        let transcript = ListState::new(0, ListAlignment::Bottom, window.viewport_size().height);
        transcript.set_scroll_handler(cx.listener(|this, event: &ListScrollEvent, _, _| {
            this.follow_output = !event.is_scrolled;
        }));
        input.read(cx).focus_handle(cx).focus(window);
        cx.spawn(async move |view, cx| {
            while let Ok(event) = events.recv().await {
                if view
                    .update(cx, |view, cx| {
                        view.event(event, cx);
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
                cx.notify();
            });
        })
        .detach();
        Self {
            commands,
            sidebar_commands,
            shutdown,
            cancel: None,
            input,
            chat: None,
            markdown: Vec::new(),
            partial_markdown: MessageText::default(),
            collapsed_thoughts: HashSet::new(),
            diffs: Vec::new(),
            change_indices: HashMap::new(),
            collapsed_changes: HashSet::new(),
            chats: Vec::new(),
            sidebar: true,
            sidebar_width: px(UiSettings::default().sidebar_width),
            composer_height: px(UiSettings::default().composer_height),
            composer_expanded: false,
            composer_resize: None,
            settings_dirty: false,
            settings_error: None,
            sidebar_error: None,
            files: false,
            tree,
            transcript,
            rows: Vec::new(),
            follow_output: true,
            error: None,
            waiting: true,
            pending_open: false,
        }
    }

    fn event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Chats(chats) => self.chats = chats,
            Event::UiSettings(settings) => {
                if !self.settings_dirty {
                    self.sidebar_width = px(settings.sidebar_width);
                    self.composer_height = px(settings.composer_height);
                }
            }
            Event::SettingsError(error) => self.settings_error = Some(error),
            Event::SidebarError(error) => self.sidebar_error = Some(error),
            Event::Directory { path, listing } => {
                self.tree
                    .update(cx, |tree, cx| tree.directory(path, listing, cx));
            }
            Event::State { chat, opened } => {
                if opened {
                    self.pending_open = false;
                }
                let previous_chat = self.chat.take();
                let changed = previous_chat.as_ref().is_none_or(|old| old.id != chat.id);
                let mut changed_content =
                    previous_chat
                        .as_ref()
                        .filter(|_| !changed)
                        .map_or(0, |old| {
                            old.contents
                                .iter()
                                .zip(&chat.contents)
                                .position(|(old, new)| {
                                    old.role != new.role || old.parts != new.parts
                                })
                                .unwrap_or(old.contents.len().min(chat.contents.len()))
                        });
                if changed {
                    self.input.update(cx, |input, cx| input.clear(cx));
                    self.collapsed_thoughts.clear();
                    self.collapsed_changes.clear();
                }
                if changed || opened || chat.file_changes.len() < self.diffs.len() {
                    self.diffs.clear();
                    self.change_indices.clear();
                    self.collapsed_changes
                        .retain(|&index| index < chat.file_changes.len());
                    changed_content = 0;
                }
                for (index, change) in chat.file_changes.iter().enumerate().skip(self.diffs.len()) {
                    self.diffs.push(FileDiff::new(&change.change.patch));
                    self.change_indices
                        .insert((change.content_index, change.part_index), index);
                    changed_content = changed_content.min(change.content_index);
                }
                let summary = ChatSummary::from(chat.as_ref());
                if let Some(existing) = self.chats.iter_mut().find(|item| item.id == chat.id) {
                    *existing = summary;
                } else {
                    self.chats.push(summary);
                }
                self.chats
                    .sort_by_key(|item| std::cmp::Reverse(item.updated_at));
                if !self.pending_open {
                    self.waiting = false;
                }
                let mut previous = std::mem::take(&mut self.markdown).into_iter();
                let mut partial = std::mem::take(&mut self.partial_markdown);
                self.markdown = chat
                    .contents
                    .iter()
                    .enumerate()
                    .map(|(index, content)| {
                        let old = previous.next().unwrap_or_default();
                        if content.role == Role::Model {
                            if !changed && index < changed_content {
                                return old;
                            }
                            let mut markdown = if changed {
                                MessageText::default()
                            } else if previous_chat
                                .as_ref()
                                .is_some_and(|old| index == old.contents.len())
                            {
                                std::mem::take(&mut partial)
                            } else {
                                old
                            };
                            markdown.answer.update(&content.visible_text(), !changed);
                            markdown.thoughts.update(&content.thought_text(), !changed);
                            markdown
                        } else {
                            MessageText::default()
                        }
                    })
                    .collect();
                if changed {
                    partial = MessageText::default();
                }
                if changed
                    || previous_chat
                        .as_ref()
                        .is_none_or(|old| old.partial != chat.partial)
                {
                    partial.answer.update(&chat.partial, !changed);
                }
                if changed
                    || previous_chat
                        .as_ref()
                        .is_none_or(|old| old.partial_thoughts != chat.partial_thoughts)
                {
                    partial.thoughts.update(&chat.partial_thoughts, !changed);
                }
                self.partial_markdown = partial;
                self.collapsed_thoughts.retain(|&index| {
                    chat.contents
                        .get(index)
                        .is_some_and(|content| content.role == Role::Model)
                        || (index == chat.contents.len() && !chat.partial_thoughts.is_empty())
                });
                self.chat = Some(*chat);
                if changed {
                    self.follow_output = true;
                }
                self.refresh_rows(changed_content, changed);
            }
            Event::Delta(text) => {
                if let Some(chat) = &mut self.chat {
                    chat.partial.push_str(&text);
                    self.partial_markdown.answer.update(&chat.partial, true);
                    let content_index = chat.contents.len();
                    self.refresh_rows(content_index, false);
                }
            }
            Event::ThoughtDelta(text) => {
                if let Some(chat) = &mut self.chat {
                    chat.partial_thoughts.push_str(&text);
                    self.partial_markdown
                        .thoughts
                        .update(&chat.partial_thoughts, true);
                    let content_index = chat.contents.len();
                    self.refresh_rows(content_index, false);
                }
            }
            Event::ContextTokens(tokens) => {
                if let Some(chat) = &mut self.chat {
                    chat.context_tokens = Some(tokens);
                }
            }
            Event::Error(error) => {
                self.error = Some(error);
                self.pending_open = false;
                self.waiting = false;
            }
        }
    }

    fn running(&self) -> bool {
        self.chat
            .as_ref()
            .is_some_and(|chat| chat.status == Status::Running)
    }
    fn stop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        self.stop();
        self.pending_open = true;
        self.error = None;
        if self.commands.try_send(command).is_err() {
            self.error = Some("The agent worker is unavailable".into());
        } else {
            self.waiting = true;
        }
        cx.notify();
    }

    fn send(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        if self.running() || self.waiting || self.input.read(cx).is_composing() {
            return;
        }
        let text = self.input.read(cx).text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Some(chat) = &self.chat else {
            return;
        };
        let chat_id = chat.id;
        let cancel = self.shutdown.child_token();
        if self
            .commands
            .try_send(Command::Send {
                chat_id,
                text,
                cancel: cancel.clone(),
            })
            .is_err()
        {
            self.error = Some("The agent worker is unavailable".into());
        } else {
            self.cancel = Some(cancel);
            self.waiting = true;
            self.error = None;
            self.input.update(cx, |input, cx| input.clear(cx));
            if self.composer_expanded {
                self.toggle_composer(window, cx);
            }
            self.follow_latest();
        }
        cx.notify();
    }

    fn new_chat(&mut self, _: &NewChat, _: &mut Window, cx: &mut Context<Self>) {
        self.command(Command::New, cx);
    }

    fn follow_latest(&mut self) {
        self.follow_output = true;
        self.transcript.scroll_to(ListOffset {
            item_ix: self.rows.len(),
            offset_in_item: px(0.),
        });
    }

    fn confirm_delete_chat(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if self.waiting {
            return;
        }
        let Some(chat) = self.chats.iter().find(|chat| chat.id == id) else {
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

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar = !self.sidebar;
        self.tree.update(cx, |tree, cx| {
            tree.set_active(self.sidebar && self.files, cx)
        });
        cx.notify();
    }

    fn settings(&self) -> UiSettings {
        UiSettings {
            sidebar_width: self.sidebar_width.into(),
            composer_height: self.composer_height.into(),
        }
    }

    fn composer_max_height(window: &Window) -> Pixels {
        (window.viewport_size().height - px(200.))
            .clamp(px(MIN_COMPOSER_HEIGHT), px(MAX_COMPOSER_HEIGHT))
    }

    fn toggle_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer_expanded = !self.composer_expanded;
        self.input.read(cx).focus_handle(cx).focus(window);
        cx.notify();
    }

    fn resize_composer(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resize) = self.composer_resize else {
            return;
        };
        let height = (resize.height + resize.pointer_y - event.position.y)
            .clamp(px(MIN_COMPOSER_HEIGHT), Self::composer_max_height(window));
        if height != self.composer_height {
            self.composer_height = height;
            self.settings_dirty = true;
            cx.notify();
        }
    }

    fn finish_resize(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.button != MouseButton::Left {
            return;
        }
        self.composer_resize = None;
        if self.settings_dirty {
            match self
                .sidebar_commands
                .try_send(SidebarCommand::SaveSettings(self.settings()))
            {
                Ok(()) => {
                    self.settings_dirty = false;
                    self.settings_error = None;
                }
                Err(_) => {
                    self.settings_error =
                        Some("Cannot save the layout settings: the worker is unavailable".into())
                }
            }
            cx.notify();
        }
    }

    fn sidebar(&self, window: &Window, cx: &Context<Self>) -> Stateful<Div> {
        let max_width = (window.viewport_size().width - px(360.)).clamp(px(180.), px(480.));
        div()
            .id("sidebar")
            .relative()
            .w(self.sidebar_width.clamp(px(180.), max_width))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(0x19191d))
            .border_r_1()
            .border_color(rgb(0x303036))
            .flex()
            .flex_col()
            .on_drag_move::<SidebarResize>(cx.listener(
                move |this, event: &DragMoveEvent<SidebarResize>, _, cx| {
                    let width =
                        (event.event.position.x - event.bounds.left()).clamp(px(180.), max_width);
                    if this.sidebar_width != width {
                        this.sidebar_width = width;
                        this.settings_dirty = true;
                        cx.notify();
                    }
                    cx.stop_propagation();
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
                    .child(if self.files {
                        self.tree.clone().into_any_element()
                    } else {
                        self.chat_list(cx).into_any_element()
                    })
                    .when_some(self.sidebar_error.clone(), |view, error| {
                        view.child(div().text_xs().text_color(rgb(0xf0aaaa)).child(error))
                    }),
            )
            .child(
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
                                .text_color(rgb(if self.files == files {
                                    0xe5e5e8
                                } else {
                                    0x8b8b96
                                }))
                                .bg(rgb(if self.files == files {
                                    0x303039
                                } else {
                                    0x19191d
                                }))
                                .hover(|style| style.bg(rgb(0x292930)))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.files = files;
                                    this.tree.update(cx, |tree, cx| tree.set_active(files, cx));
                                    cx.notify();
                                }))
                        }),
                    ),
            )
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

    fn chat_list(&self, cx: &Context<Self>) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().justify_end().child(
                button("new-chat", "+ New").on_click(
                    cx.listener(|this, _, window, cx| this.new_chat(&NewChat, window, cx)),
                ),
            ))
            .child(
                div()
                    .id("chat-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(self.chats.iter().map(|chat| {
                        let id = chat.id;
                        let selected = self.chat.as_ref().is_some_and(|active| active.id == id);
                        div()
                            .id(SharedString::from(id.to_string()))
                            .p_3()
                            .mb_1()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(rgb(if selected { 0x303039 } else { 0x19191d }))
                            .hover(|style| style.bg(rgb(0x292930)))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .truncate()
                                                    .child(chat.title.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(0x8b8b96))
                                                    .child(date_label(chat.updated_at)),
                                            ),
                                    )
                                    .child(
                                        button("delete", "Delete")
                                            .flex_shrink_0()
                                            .px_2()
                                            .text_color(rgb(0xd5a0a0))
                                            .when(self.waiting, |button| {
                                                button.opacity(0.4).cursor_default()
                                            })
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                cx.stop_propagation();
                                                this.confirm_delete_chat(id, window, cx);
                                            })),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.command(Command::Select(id), cx)
                            }))
                    })),
            )
    }

    fn model_rows(&self, content_index: usize, text: &MessageText, rows: &mut Vec<TranscriptRow>) {
        if text.answer.block_count() == 0 && text.thoughts.block_count() == 0 {
            return;
        }
        rows.push(TranscriptRow::ModelHeader(content_index));
        if text.thoughts.block_count() > 0 {
            rows.push(TranscriptRow::Thinking(content_index));
            if !self.collapsed_thoughts.contains(&content_index) {
                rows.extend(
                    (0..text.thoughts.block_count())
                        .map(|block| TranscriptRow::Thought(content_index, block)),
                );
            }
        }
        rows.extend(
            (0..text.answer.block_count()).map(|block| TranscriptRow::Model(content_index, block)),
        );
    }

    fn message_text(&self, content_index: usize) -> &MessageText {
        self.markdown
            .get(content_index)
            .unwrap_or(&self.partial_markdown)
    }

    fn toggle_thoughts(&mut self, content_index: usize, cx: &mut Context<Self>) {
        if !self.collapsed_thoughts.remove(&content_index) {
            self.collapsed_thoughts.insert(content_index);
        }
        self.follow_output = false;
        self.refresh_rows(content_index, false);
        cx.notify();
    }

    fn toggle_diff(&mut self, change_index: usize, content_index: usize, cx: &mut Context<Self>) {
        if !self.collapsed_changes.remove(&change_index) {
            self.collapsed_changes.insert(change_index);
        }
        self.follow_output = false;
        self.refresh_rows(content_index, false);
        cx.notify();
    }

    fn refresh_rows(&mut self, changed_content: usize, reset: bool) {
        if !reset
            && !self.follow_output
            && self.transcript.logical_scroll_top().item_ix == self.rows.len()
        {
            let height = self.transcript.viewport_bounds().size.height;
            if height > px(TRANSCRIPT_PADDING * 2.) {
                // Turn the bottom pin into an explicit viewport anchor while reading.
                self.transcript
                    .scroll_by(px(TRANSCRIPT_PADDING * 2.) - height);
            }
        }
        let Some(chat) = &self.chat else {
            return;
        };
        let stable = if reset {
            0
        } else {
            self.rows
                .iter()
                .position(|row| row.content_index() >= changed_content)
                .unwrap_or(self.rows.len())
        };
        let mut rows = self.rows[..stable].to_vec();
        for (content_index, content) in chat.contents.iter().enumerate().skip(changed_content) {
            if content.role == Role::Model {
                self.model_rows(content_index, &self.markdown[content_index], &mut rows);
            } else {
                let text = content.visible_text();
                if !text.is_empty() {
                    rows.push(TranscriptRow::User(content_index, CachedText::new(text)));
                }
            }
            for (part_index, part) in content.parts.iter().enumerate() {
                if let Some(label) = tool_label(part) {
                    rows.push(TranscriptRow::Tool(content_index, part_index, label.into()));
                    if let Some(&change_index) =
                        self.change_indices.get(&(content_index, part_index))
                        && !self.collapsed_changes.contains(&change_index)
                    {
                        rows.extend(
                            (0..self.diffs[change_index].block_count()).map(|block| {
                                TranscriptRow::Diff(content_index, change_index, block)
                            }),
                        );
                    }
                }
            }
        }
        self.model_rows(chat.contents.len(), &self.partial_markdown, &mut rows);
        if chat.notice.is_some() {
            rows.push(TranscriptRow::Notice);
        }
        if reset {
            self.transcript.reset(rows.len());
        } else {
            let prefix = self
                .rows
                .iter()
                .zip(&rows)
                .take_while(|(old, new)| old.key() == new.key())
                .count();
            let suffix = self.rows[prefix..]
                .iter()
                .rev()
                .zip(rows[prefix..].iter().rev())
                .take_while(|(old, new)| old.key() == new.key())
                .count();
            let replaced_end = rows.len() - suffix;
            if prefix < self.rows.len() - suffix || prefix < replaced_end {
                self.transcript
                    .splice(prefix..self.rows.len() - suffix, replaced_end - prefix);
            }
            let viewport = self.transcript.viewport_bounds();
            for (index, row) in rows.iter().enumerate().skip(stable) {
                if (prefix..replaced_end).contains(&index) || matches!(row, TranscriptRow::Diff(..))
                {
                    continue;
                }
                let visible = self
                    .transcript
                    .bounds_for_item(index)
                    .is_some_and(|bounds| {
                        bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom()
                    });
                // Visible rows are measured on each frame. Keep their scroll anchor.
                if !visible {
                    self.transcript.splice(index..index + 1, 1);
                }
            }
        }
        self.rows = rows;
        if self.follow_output {
            self.follow_latest();
        }
    }

    fn transcript_row(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        let Some(chat) = &self.chat else {
            return Empty.into_any_element();
        };
        let previous_is_tool = self.rows[..index]
            .last()
            .is_some_and(|row| matches!(row, TranscriptRow::Tool(..) | TranscriptRow::Diff(..)));
        let row = match self.rows[index].clone() {
            TranscriptRow::User(content_index, text) => div()
                .id(SharedString::from(format!(
                    "message-{}-{content_index}",
                    chat.id
                )))
                .pb_6()
                .child(
                    div()
                        .mb_2()
                        .text_xs()
                        .text_color(rgb(0x9797a5))
                        .child("YOU"),
                )
                .child(div().text_size(px(14.)).line_height(px(23.)).child(text))
                .into_any_element(),
            TranscriptRow::ModelHeader(content_index) => div()
                .id(SharedString::from(format!(
                    "model-{}-{content_index}",
                    chat.id
                )))
                .when(previous_is_tool, |view| view.pt_3())
                .pb_2()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(0xc5c5d2))
                .child("ARGON")
                .into_any_element(),
            TranscriptRow::Thinking(content_index) => {
                let expanded = !self.collapsed_thoughts.contains(&content_index);
                let active = content_index == chat.contents.len()
                    && chat.status == Status::Running
                    && chat.partial.is_empty();
                div()
                    .id(SharedString::from(format!(
                        "thinking-{}-{content_index}",
                        chat.id
                    )))
                    .pb_3()
                    .child(
                        div()
                            .id("toggle")
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .line_height(px(18.))
                            .text_color(rgb(0x9797a5))
                            .cursor_pointer()
                            .child(disclosure_arrow(expanded))
                            .child(if active { "Thinking…" } else { "Thinking" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_thoughts(content_index, cx)
                            })),
                    )
                    .into_any_element()
            }
            TranscriptRow::Thought(content_index, block) => {
                let markdown = &self.message_text(content_index).thoughts;
                div()
                    .id(SharedString::from(format!(
                        "thought-{}-{content_index}-{block}",
                        chat.id
                    )))
                    .pl_3()
                    .pb(if block + 1 == markdown.block_count() {
                        px(16.)
                    } else {
                        px(8.)
                    })
                    .border_l_1()
                    .border_color(rgb(0x3b3b45))
                    .text_color(rgb(0x9797a5))
                    .child(
                        markdown
                            .render_block(block, ("thought", block))
                            .text_size(px(13.))
                            .line_height(px(21.)),
                    )
                    .into_any_element()
            }
            TranscriptRow::Model(content_index, block) => {
                let markdown = &self.message_text(content_index).answer;
                div()
                    .id(SharedString::from(format!(
                        "message-{}-{content_index}-{block}",
                        chat.id
                    )))
                    .pb(if block + 1 == markdown.block_count() {
                        px(24.)
                    } else {
                        px(12.)
                    })
                    .child(markdown.render_block(block, ("markdown", block)))
                    .into_any_element()
            }
            TranscriptRow::Tool(content_index, part_index, label) => {
                let change_index = self
                    .change_indices
                    .get(&(content_index, part_index))
                    .copied();
                let marker = match change_index {
                    Some(index) if self.collapsed_changes.contains(&index) => "▸",
                    Some(_) => "▾",
                    None => "↳",
                };
                div()
                    .id(SharedString::from(format!(
                        "tool-{content_index}-{part_index}"
                    )))
                    .pt(if previous_is_tool { px(0.) } else { px(8.) })
                    .pb_1()
                    .child(
                        div()
                            .id("caption")
                            .flex()
                            .items_center()
                            .gap_2()
                            .pl_2()
                            .font_family("Menlo")
                            .text_size(px(11.))
                            .line_height(px(18.))
                            .text_color(rgb(0x858592))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(rgb(0x626270))
                                    .child(marker),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            .when_some(change_index, |view, change_index| {
                                let diff = &self.diffs[change_index];
                                view.cursor_pointer()
                                    .when(diff.added > 0, |view| {
                                        view.child(
                                            div()
                                                .text_color(rgb(0x87ac90))
                                                .child(format!("+{}", diff.added)),
                                        )
                                    })
                                    .when(diff.removed > 0, |view| {
                                        view.child(
                                            div()
                                                .text_color(rgb(0xb58589))
                                                .child(format!("−{}", diff.removed)),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_diff(change_index, content_index, cx);
                                    }))
                            }),
                    )
                    .into_any_element()
            }
            TranscriptRow::Diff(_, change_index, block) => div()
                .id(SharedString::from(format!("diff-{change_index}-{block}")))
                .min_w_0()
                .pb(if block + 1 == self.diffs[change_index].block_count() {
                    px(8.)
                } else {
                    px(0.)
                })
                .child(self.diffs[change_index].render_block(block))
                .into_any_element(),
            TranscriptRow::Notice => div()
                .pt_4()
                .text_sm()
                .text_color(rgb(0xd5b98c))
                .child(chat.notice.clone().unwrap_or_default())
                .into_any_element(),
        };
        div().px_6().min_w_0().child(row).into_any_element()
    }

    fn composer(&self, window: &Window, cx: &Context<Self>) -> Stateful<Div> {
        let expanded = self.composer_expanded;
        let height = self
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
                view.child(
                    div()
                        .id("composer-resize")
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(8.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_t_lg()
                        .border_t_1()
                        .border_color(transparent_black())
                        .cursor(CursorStyle::ResizeUpDown)
                        .hover(|style| style.border_color(rgb(0x626270)))
                        .child(div().w(px(32.)).h(px(2.)).rounded_full().bg(rgb(0x626270)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                this.composer_resize = Some(ComposerResize {
                                    pointer_y: event.position.y,
                                    height: this.composer_height.clamp(
                                        px(MIN_COMPOSER_HEIGHT),
                                        Self::composer_max_height(window),
                                    ),
                                });
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
            })
            .child(
                div()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .p_2()
                    .flex()
                    .flex_col()
                    .child(
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
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_composer(window, cx)
                                    })),
                            ),
                    )
                    .child(
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
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .line_height(px(16.))
                                    .text_color(rgb(0x91919f))
                                    .child(if self.running() {
                                        ""
                                    } else if self.waiting {
                                        "Loading…"
                                    } else {
                                        "Enter to send · Shift+Enter for a new line"
                                    }),
                            )
                            .child(if self.running() {
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
                                .into_any_element()
                            } else {
                                button("send", "Send")
                                    .flex_shrink_0()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.send(&SendMessage, window, cx)
                                    }))
                                    .into_any_element()
                            }),
                    ),
            )
    }

    fn transcript(&self, cx: &Context<Self>) -> Stateful<Div> {
        div().id("transcript").flex_1().min_h_0().min_w_0().child(
            list(
                self.transcript.clone(),
                cx.processor(|this, index, _, cx| this.transcript_row(index, cx)),
            )
            .size_full()
            .py(px(TRANSCRIPT_PADDING)),
        )
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if self.settings_dirty {
            let _ = self
                .sidebar_commands
                .try_send(SidebarCommand::SaveSettings(self.settings()));
        }
        self.stop();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let usage = self
            .chat
            .as_ref()
            .map(|chat| chat.usage)
            .unwrap_or_default();
        let cost = self.chat.as_ref().map_or(0., Chat::estimated_cost_usd);
        let context_limit = format_tokens(CONTEXT_WINDOW_TOKENS);
        let context = self.chat.as_ref().and_then(|chat| chat.context_tokens);
        let context = context.map_or_else(
            || format!("Context: — / {context_limit}"),
            |tokens| {
                let percent = tokens as f64 / CONTEXT_WINDOW_TOKENS as f64 * 100.;
                format!(
                    "Context: {} / {context_limit} ({percent:.1}%)",
                    format_tokens(tokens)
                )
            },
        );
        div()
            .size_full()
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
            .on_action(cx.listener(|this, _: &Stop, window, cx| {
                if this.composer_expanded {
                    this.toggle_composer(window, cx);
                } else {
                    this.stop();
                    cx.notify();
                }
            }))
            .on_action(|_: &Close, window, _| window.remove_window())
            .when(self.sidebar, |view| view.child(self.sidebar(window, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .when(!self.composer_expanded, |view| {
                        view.child(
                            div()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .flex()
                                .flex_col()
                                .child(self.transcript(cx))
                                .when(!self.follow_output, |view| {
                                    view.child(div().absolute().right_6().bottom_3().child(
                                        button("latest", "Latest").on_click(cx.listener(
                                            |this, _, _, cx| {
                                                this.follow_latest();
                                                cx.notify();
                                            },
                                        )),
                                    ))
                                }),
                        )
                    })
                    .when_some(self.error.clone(), |view, error| {
                        view.child(div().px_6().py_2().text_color(rgb(0xf0aaaa)).child(error))
                    })
                    .when_some(self.settings_error.clone(), |view, error| {
                        view.child(div().px_6().py_2().text_color(rgb(0xf0aaaa)).child(error))
                    })
                    .child(self.composer(window, cx))
                    .child(
                        div()
                            .px_5()
                            .pb_3()
                            .flex()
                            .flex_wrap()
                            .gap_x_3()
                            .gap_y_1()
                            .text_xs()
                            .text_color(rgb(0x858593))
                            .children(
                                [
                                    context,
                                    format!("{} input", format_tokens(usage.input)),
                                    format!("{} cached", format_tokens(usage.cached)),
                                    format!("{} output", format_tokens(usage.output)),
                                    format!("{} thinking", format_tokens(usage.thinking)),
                                    format!("Est. ${cost:.4} USD"),
                                ]
                                .into_iter()
                                .map(|label| div().child(label)),
                            ),
                    ),
            )
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    // Promote counts that round to 1,000K to the M suffix.
    let (scale, suffix) = if tokens >= 999_995 {
        (1_000_000., "M")
    } else {
        (1_000., "K")
    };
    let value = format!("{:.2}", tokens as f64 / scale);
    format!(
        "{}{suffix}",
        value.trim_end_matches('0').trim_end_matches('.')
    )
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

fn disclosure_arrow(expanded: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let points = if expanded {
                [(-3.5, -1.75), (0., 1.75), (3.5, -1.75)]
            } else {
                [(-1.75, -3.5), (1.75, 0.), (-1.75, 3.5)]
            };
            let mut path = PathBuilder::stroke(px(1.5));
            path.move_to(center + point(px(points[0].0), px(points[0].1)));
            for (x, y) in &points[1..] {
                path.line_to(center + point(px(*x), px(*y)));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(0x9797a5));
            }
        },
    )
    .size(px(12.))
    .flex_shrink_0()
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

fn date_label(timestamp: u64) -> String {
    let minutes = space::model::now().saturating_sub(timestamp) / 60_000;
    match minutes {
        0 => "Just now".into(),
        1..60 => format!("{minutes}m ago"),
        60..1440 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

fn tool_label(part: &serde_json::Value) -> Option<String> {
    let call = part.get("functionCall")?;
    let name = call["name"].as_str().unwrap_or("tool");
    Some(match call["args"]["path"].as_str() {
        Some(path) => format!("{name}  {path}"),
        None => name.to_owned(),
    })
}
