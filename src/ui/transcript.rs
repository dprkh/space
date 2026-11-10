use super::*;
use super::{
    diff::FileDiff,
    markdown::{CachedText, Markdown},
};
use serde::{Deserialize, Serialize};
use space::model::Role;
use std::collections::{HashMap, HashSet};

const TRANSCRIPT_PADDING: f32 = 20.;

#[derive(Default)]
pub(super) struct MessageText {
    pub(super) answer: Markdown,
    pub(super) thoughts: Markdown,
}

#[derive(Clone)]
enum TranscriptRow {
    User(usize, CachedText),
    ModelHeader(usize),
    Thinking(usize),
    Thought(usize, usize),
    Model(usize, usize),
    MessageControls(usize),
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
            | Self::MessageControls(index)
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
            Self::MessageControls(index) => (8, *index, 0),
        }
    }
}

pub(super) struct TranscriptState {
    markdown: Vec<MessageText>,
    pub(super) partial_markdown: MessageText,
    expanded_thoughts: HashSet<usize>,
    diffs: Vec<FileDiff>,
    change_indices: HashMap<(usize, usize), usize>,
    collapsed_changes: HashSet<usize>,
    pub(super) list: ListState,
    rows: Vec<TranscriptRow>,
    pub(super) follow_output: bool,
}

#[derive(Deserialize, Serialize)]
pub(super) struct SavedTranscript {
    branch: Option<Uuid>,
    #[serde(default)]
    expanded_thoughts: HashSet<usize>,
    collapsed_changes: HashSet<usize>,
    scroll_key: Option<(u8, usize, usize)>,
    item_index: usize,
    offset: f32,
    follow_output: bool,
}

impl TranscriptState {
    pub(super) fn saved_state(&self, chat: &Chat) -> SavedTranscript {
        let scroll = self.list.logical_scroll_top();
        SavedTranscript {
            branch: chat.active_branch(),
            expanded_thoughts: self.expanded_thoughts.clone(),
            collapsed_changes: self.collapsed_changes.clone(),
            scroll_key: self.rows.get(scroll.item_ix).map(TranscriptRow::key),
            item_index: scroll.item_ix,
            offset: scroll.offset_in_item.into(),
            follow_output: self.follow_output,
        }
    }

    pub(super) fn restore_state(&mut self, state: SavedTranscript, chat: &Chat) {
        if state.branch != chat.active_branch() {
            return;
        }
        self.expanded_thoughts = state
            .expanded_thoughts
            .into_iter()
            .filter(|&index| {
                chat.contents
                    .get(index)
                    .is_some_and(|content| content.role == Role::Model)
                    || (index == chat.contents.len() && !chat.partial_thoughts.is_empty())
            })
            .collect();
        self.collapsed_changes = state
            .collapsed_changes
            .into_iter()
            .filter(|&index| index < self.diffs.len())
            .collect();
        self.follow_output = state.follow_output;
        self.refresh_rows(chat, 0, true);
        if !self.follow_output {
            let item_ix = self
                .rows
                .iter()
                .position(|row| Some(row.key()) == state.scroll_key)
                .unwrap_or(state.item_index.min(self.rows.len()));
            let offset = if state.offset.is_finite() {
                state.offset.max(0.)
            } else {
                0.
            };
            self.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(offset),
            });
        }
    }

    pub(super) fn update_snapshot(
        &mut self,
        previous_chat: Option<&Chat>,
        chat: &Chat,
        opened: bool,
        cx: &mut App,
    ) -> (usize, bool) {
        let changed = previous_chat
            .is_none_or(|old| old.id != chat.id || old.active_branch() != chat.active_branch());
        let mut changed_content = previous_chat.filter(|_| !changed).map_or(0, |old| {
            old.contents
                .iter()
                .zip(&chat.contents)
                .position(|(old, new)| old.role != new.role || old.parts != new.parts)
                .unwrap_or(old.contents.len().min(chat.contents.len()))
        });
        if changed {
            self.expanded_thoughts.clear();
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
        self.update_markdown(previous_chat, chat, changed_content, changed, cx);
        self.expanded_thoughts.retain(|&index| {
            chat.contents
                .get(index)
                .is_some_and(|content| content.role == Role::Model)
                || (index == chat.contents.len() && !chat.partial_thoughts.is_empty())
        });
        if changed {
            self.follow_output = true;
        }
        (changed_content, changed)
    }

    fn update_markdown(
        &mut self,
        previous_chat: Option<&Chat>,
        chat: &Chat,
        changed_content: usize,
        changed: bool,
        cx: &mut App,
    ) {
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
                    } else if previous_chat.is_some_and(|old| index == old.contents.len()) {
                        std::mem::take(&mut partial)
                    } else {
                        old
                    };
                    markdown
                        .answer
                        .update(&content.visible_text(), !changed, cx);
                    markdown
                        .thoughts
                        .update(&content.thought_text(), !changed, cx);
                    markdown
                } else {
                    MessageText::default()
                }
            })
            .collect();
        if changed {
            partial = MessageText::default();
        }
        if changed || previous_chat.is_none_or(|old| old.partial != chat.partial) {
            partial.answer.update(&chat.partial, !changed, cx);
        }
        if changed || previous_chat.is_none_or(|old| old.partial_thoughts != chat.partial_thoughts)
        {
            partial
                .thoughts
                .update(&chat.partial_thoughts, !changed, cx);
        }
        self.partial_markdown = partial;
    }

    pub(super) fn new(list: ListState) -> Self {
        Self {
            markdown: Vec::new(),
            partial_markdown: MessageText::default(),
            expanded_thoughts: HashSet::new(),
            diffs: Vec::new(),
            change_indices: HashMap::new(),
            collapsed_changes: HashSet::new(),
            list,
            rows: Vec::new(),
            follow_output: true,
        }
    }
}

impl TranscriptState {
    pub(super) fn show_message_controls(&mut self, content_index: usize) {
        let Some(item_ix) = self.rows.iter().position(
            |row| matches!(row, TranscriptRow::MessageControls(index) if *index == content_index),
        ) else {
            return;
        };
        self.follow_output = false;
        self.list.scroll_to_reveal_item(item_ix);
    }

    pub(super) fn follow_latest(&mut self) {
        self.follow_output = true;
        self.list.scroll_to(ListOffset {
            item_ix: self.rows.len(),
            offset_in_item: px(0.),
        });
    }

    fn model_rows(&self, content_index: usize, text: &MessageText, rows: &mut Vec<TranscriptRow>) {
        if text.answer.block_count() == 0 && text.thoughts.block_count() == 0 {
            return;
        }
        rows.push(TranscriptRow::ModelHeader(content_index));
        if text.thoughts.block_count() > 0 {
            rows.push(TranscriptRow::Thinking(content_index));
            if self.expanded_thoughts.contains(&content_index) {
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

    fn build_rows(&self, chat: &Chat, changed_content: usize, stable: usize) -> Vec<TranscriptRow> {
        let mut rows = self.rows[..stable].to_vec();
        for (content_index, content) in chat.contents.iter().enumerate().skip(changed_content) {
            let start = rows.len();
            if content.role == Role::Model {
                self.model_rows(content_index, &self.markdown[content_index], &mut rows);
            } else {
                let mut text = content.visible_text();
                for part in &content.parts {
                    if part["inlineData"]["mimeType"] == "application/pdf" {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str("PDF: ");
                        text.push_str(part["inlineData"]["displayName"].as_str().unwrap_or("PDF"));
                    }
                }
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
            if rows.len() > start {
                rows.push(TranscriptRow::MessageControls(content_index));
            }
        }
        let start = rows.len();
        self.model_rows(chat.contents.len(), &self.partial_markdown, &mut rows);
        if rows.len() > start {
            rows.push(TranscriptRow::MessageControls(chat.contents.len()));
        }
        if chat.notice.is_some() {
            rows.push(TranscriptRow::Notice);
        }
        rows
    }

    fn update_list(&mut self, rows: &[TranscriptRow], stable: usize) {
        let prefix = self
            .rows
            .iter()
            .zip(rows)
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
            self.list
                .splice(prefix..self.rows.len() - suffix, replaced_end - prefix);
        }
        let viewport = self.list.viewport_bounds();
        for (index, row) in rows.iter().enumerate().skip(stable) {
            if (prefix..replaced_end).contains(&index) || matches!(row, TranscriptRow::Diff(..)) {
                continue;
            }
            let visible = self.list.bounds_for_item(index).is_some_and(|bounds| {
                bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom()
            });
            // Visible rows are measured on each frame. Keep their scroll anchor.
            if !visible {
                self.list.splice(index..index + 1, 1);
            }
        }
    }

    pub(super) fn refresh_rows(&mut self, chat: &Chat, changed_content: usize, reset: bool) {
        if !reset
            && !self.follow_output
            && self.list.logical_scroll_top().item_ix == self.rows.len()
        {
            let height = self.list.viewport_bounds().size.height;
            if height > px(TRANSCRIPT_PADDING * 2.) {
                // Turn the bottom pin into an explicit viewport anchor while reading.
                self.list.scroll_by(px(TRANSCRIPT_PADDING * 2.) - height);
            }
        }
        let stable = if reset {
            0
        } else {
            self.rows
                .iter()
                .position(|row| row.content_index() >= changed_content)
                .unwrap_or(self.rows.len())
        };
        let rows = self.build_rows(chat, changed_content, stable);
        if reset {
            self.list.reset(rows.len());
        } else {
            self.update_list(&rows, stable);
        }
        self.rows = rows;
        if self.follow_output {
            self.follow_latest();
        }
    }
}

impl Workspace {
    pub(super) fn follow_latest(&mut self) {
        self.transcript.follow_latest();
    }

    pub(super) fn refresh_rows(&mut self, changed_content: usize, reset: bool) {
        if let Some(chat) = &self.chat {
            self.transcript.refresh_rows(chat, changed_content, reset);
        }
    }

    pub(super) fn toggle_thoughts(&mut self, content_index: usize, cx: &mut Context<Self>) {
        if !self.transcript.expanded_thoughts.remove(&content_index) {
            self.transcript.expanded_thoughts.insert(content_index);
        }
        self.transcript.follow_output = false;
        self.refresh_rows(content_index, false);
        cx.notify();
    }

    pub(super) fn toggle_diff(
        &mut self,
        change_index: usize,
        content_index: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.transcript.collapsed_changes.remove(&change_index) {
            self.transcript.collapsed_changes.insert(change_index);
        }
        self.transcript.follow_output = false;
        self.refresh_rows(content_index, false);
        cx.notify();
    }

    pub(super) fn transcript_row(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        let Some(chat) = &self.chat else {
            return Empty.into_any_element();
        };
        let previous_is_tool = self.transcript.rows[..index]
            .last()
            .is_some_and(|row| matches!(row, TranscriptRow::Tool(..) | TranscriptRow::Diff(..)));
        let row = match self.transcript.rows[index].clone() {
            TranscriptRow::User(content_index, text) => self.user_row(chat, content_index, text),
            TranscriptRow::ModelHeader(content_index) => {
                self.model_header_row(chat, content_index, previous_is_tool)
            }
            TranscriptRow::Thinking(content_index) => self.thinking_row(chat, content_index, cx),
            TranscriptRow::Thought(content_index, block) => {
                self.thought_row(chat, content_index, block)
            }
            TranscriptRow::Model(content_index, block) => {
                self.model_row(chat, content_index, block)
            }
            TranscriptRow::MessageControls(content_index) => {
                self.message_controls_row(chat, content_index, cx)
            }
            TranscriptRow::Tool(content_index, part_index, label) => {
                self.tool_row(content_index, part_index, label, previous_is_tool, cx)
            }
            TranscriptRow::Diff(_, change_index, block) => self.diff_row(change_index, block),
            TranscriptRow::Notice => self.notice_row(chat, cx),
        };
        div().px_6().min_w_0().child(row).into_any_element()
    }

    fn user_row(&self, chat: &Chat, content_index: usize, text: CachedText) -> AnyElement {
        div()
            .id(SharedString::from(format!(
                "message-{}-{content_index}",
                chat.id
            )))
            .pb_2()
            .child(
                div()
                    .mb_2()
                    .text_xs()
                    .text_color(rgb(0x9797a5))
                    .child("YOU"),
            )
            .child(div().text_size(px(14.)).line_height(px(23.)).child(text))
            .into_any_element()
    }

    fn message_controls_row(
        &self,
        chat: &Chat,
        content_index: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let controls = if chat
            .contents
            .get(content_index)
            .is_some_and(|content| content.role == Role::User)
        {
            self.user_message_controls(chat, content_index, cx)
        } else {
            self.message_actions(chat, content_index, cx)
        };
        div()
            .id(SharedString::from(format!(
                "message-controls-{}-{content_index}",
                chat.id
            )))
            .w_full()
            .flex()
            .justify_end()
            .pb_6()
            .child(controls)
            .into_any_element()
    }

    fn user_message_controls(&self, chat: &Chat, content_index: usize, cx: &Context<Self>) -> Div {
        let enabled = self.can_change_branch();
        div()
            .flex()
            .flex_wrap()
            .flex_shrink_0()
            .max_w_full()
            .items_center()
            .justify_end()
            .gap_1()
            .when_some(
                chat.message_versions(content_index),
                |view, (selected, count)| {
                    view.child(self.message_version_controls(content_index, selected, count, cx))
                },
            )
            .when(!chat.archived, |view| {
                view.child(
                    icon_button("edit-message", Icon::Pencil.render(14., 0x9797a5))
                        .when(!enabled, |view| view.opacity(0.3).cursor_default())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_message_edit(content_index, window, cx);
                        })),
                )
            })
            .child(self.message_actions(chat, content_index, cx))
    }

    fn message_version_controls(
        &self,
        content_index: usize,
        selected: usize,
        count: usize,
        cx: &Context<Self>,
    ) -> Div {
        let enabled = self.can_change_branch();
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .child(
                icon_button("previous-version", Icon::ChevronLeft.render(12., 0x9797a5))
                    .when(!enabled || selected == 0, |view| {
                        view.opacity(0.3).cursor_default()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if selected > 0 {
                            this.select_message_version(content_index, selected - 1, cx);
                        }
                    })),
            )
            .child(format!("{} / {count}", selected + 1))
            .child(
                icon_button("next-version", Icon::ChevronRight.render(12., 0x9797a5))
                    .when(!enabled || selected + 1 >= count, |view| {
                        view.opacity(0.3).cursor_default()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_message_version(content_index, selected + 1, cx);
                    })),
            )
    }

    fn model_header_row(
        &self,
        chat: &Chat,
        content_index: usize,
        previous_is_tool: bool,
    ) -> AnyElement {
        div()
            .id(SharedString::from(format!(
                "model-{}-{content_index}",
                chat.id
            )))
            .when(previous_is_tool, |view| view.pt_3())
            .pb_2()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(0xc5c5d2))
            .child(chat.response_model(content_index).label())
            .into_any_element()
    }

    fn thinking_row(&self, chat: &Chat, content_index: usize, cx: &Context<Self>) -> AnyElement {
        let expanded = self.transcript.expanded_thoughts.contains(&content_index);
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
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_thoughts(content_index, cx)),
                    ),
            )
            .into_any_element()
    }

    fn thought_row(&self, chat: &Chat, content_index: usize, block: usize) -> AnyElement {
        let markdown = &self.transcript.message_text(content_index).thoughts;
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

    fn model_row(&self, chat: &Chat, content_index: usize, block: usize) -> AnyElement {
        let markdown = &self.transcript.message_text(content_index).answer;
        div()
            .id(SharedString::from(format!(
                "message-{}-{content_index}-{block}",
                chat.id
            )))
            .pb(if block + 1 == markdown.block_count() {
                px(8.)
            } else {
                px(12.)
            })
            .child(markdown.render_block(block, ("markdown", block)))
            .into_any_element()
    }

    fn tool_row(
        &self,
        content_index: usize,
        part_index: usize,
        label: SharedString,
        previous_is_tool: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let change_index = self
            .transcript
            .change_indices
            .get(&(content_index, part_index))
            .copied();
        let marker = match change_index {
            Some(index) if self.transcript.collapsed_changes.contains(&index) => Icon::ChevronRight,
            Some(_) => Icon::ChevronDown,
            None => Icon::Tool,
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
                    .child(marker.render(12., 0x626270))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when_some(change_index, |view, change_index| {
                        let diff = &self.transcript.diffs[change_index];
                        view.cursor_pointer()
                            .when(diff.added > 0, |view| {
                                view.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_color(rgb(0x87ac90))
                                        .child(Icon::Plus.render(10., 0x87ac90))
                                        .child(diff.added.to_string()),
                                )
                            })
                            .when(diff.removed > 0, |view| {
                                view.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_color(rgb(0xb58589))
                                        .child(Icon::Minus.render(10., 0xb58589))
                                        .child(diff.removed.to_string()),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_diff(change_index, content_index, cx);
                            }))
                    }),
            )
            .into_any_element()
    }

    fn diff_row(&self, change_index: usize, block: usize) -> AnyElement {
        div()
            .id(SharedString::from(format!("diff-{change_index}-{block}")))
            .min_w_0()
            .pb(
                if block + 1 == self.transcript.diffs[change_index].block_count() {
                    px(8.)
                } else {
                    px(0.)
                },
            )
            .child(self.transcript.diffs[change_index].render_block(block))
            .into_any_element()
    }

    fn notice_row(&self, chat: &Chat, cx: &Context<Self>) -> AnyElement {
        div()
            .pt_4()
            .flex()
            .items_start()
            .gap_3()
            .text_sm()
            .text_color(rgb(0xd5b98c))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(chat.notice.clone().unwrap_or_default()),
            )
            .when(
                chat.status == Status::Interrupted
                    && !chat.archived
                    && !chat.contents.is_empty()
                    && !self.waiting,
                |view| {
                    view.child(
                        button("retry", "Retry")
                            .flex_shrink_0()
                            .text_color(rgb(0xe5e5e8))
                            .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                    )
                },
            )
            .into_any_element()
    }

    pub(super) fn render_transcript(&self, cx: &Context<Self>) -> Stateful<Div> {
        div().id("transcript").flex_1().min_h_0().min_w_0().child(
            list(
                self.transcript.list.clone(),
                cx.processor(|this, index, _, cx| this.transcript_row(index, cx)),
            )
            .size_full()
            .py(px(TRANSCRIPT_PADDING)),
        )
    }
}

impl Workspace {
    pub(super) fn transcript_panel(&self, cx: &Context<Self>) -> Div {
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_transcript(cx))
            .when(!self.transcript.follow_output, |view| {
                view.child(div().absolute().right_6().bottom_3().child(
                    button("latest", "Latest").on_click(cx.listener(|this, _, _, cx| {
                        this.follow_latest();
                        cx.notify();
                    })),
                ))
            })
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
