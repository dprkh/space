use super::*;
use crate::{
    diff::FileDiff,
    markdown::{CachedText, Markdown},
};
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

pub(super) struct TranscriptState {
    markdown: Vec<MessageText>,
    pub(super) partial_markdown: MessageText,
    collapsed_thoughts: HashSet<usize>,
    diffs: Vec<FileDiff>,
    change_indices: HashMap<(usize, usize), usize>,
    collapsed_changes: HashSet<usize>,
    pub(super) list: ListState,
    rows: Vec<TranscriptRow>,
    pub(super) follow_output: bool,
}

impl TranscriptState {
    pub(super) fn update_snapshot(
        &mut self,
        previous_chat: Option<&Chat>,
        chat: &Chat,
        opened: bool,
    ) -> (usize, bool) {
        let changed = previous_chat.is_none_or(|old| old.id != chat.id);
        let mut changed_content = previous_chat.filter(|_| !changed).map_or(0, |old| {
            old.contents
                .iter()
                .zip(&chat.contents)
                .position(|(old, new)| old.role != new.role || old.parts != new.parts)
                .unwrap_or(old.contents.len().min(chat.contents.len()))
        });
        if changed {
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
        self.update_markdown(previous_chat, chat, changed_content, changed);
        self.collapsed_thoughts.retain(|&index| {
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
        if changed || previous_chat.is_none_or(|old| old.partial != chat.partial) {
            partial.answer.update(&chat.partial, !changed);
        }
        if changed || previous_chat.is_none_or(|old| old.partial_thoughts != chat.partial_thoughts)
        {
            partial.thoughts.update(&chat.partial_thoughts, !changed);
        }
        self.partial_markdown = partial;
    }

    pub(super) fn new(list: ListState) -> Self {
        Self {
            markdown: Vec::new(),
            partial_markdown: MessageText::default(),
            collapsed_thoughts: HashSet::new(),
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

    fn build_rows(&self, chat: &Chat, changed_content: usize, stable: usize) -> Vec<TranscriptRow> {
        let mut rows = self.rows[..stable].to_vec();
        for (content_index, content) in chat.contents.iter().enumerate().skip(changed_content) {
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
        }
        self.model_rows(chat.contents.len(), &self.partial_markdown, &mut rows);
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
        if !self.transcript.collapsed_thoughts.remove(&content_index) {
            self.transcript.collapsed_thoughts.insert(content_index);
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
                .child(chat.response_model(content_index).label())
                .into_any_element(),
            TranscriptRow::Thinking(content_index) => {
                let expanded = !self.transcript.collapsed_thoughts.contains(&content_index);
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
            TranscriptRow::Model(content_index, block) => {
                let markdown = &self.transcript.message_text(content_index).answer;
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
                    .transcript
                    .change_indices
                    .get(&(content_index, part_index))
                    .copied();
                let marker = match change_index {
                    Some(index) if self.transcript.collapsed_changes.contains(&index) => {
                        Icon::ChevronRight
                    }
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
            TranscriptRow::Diff(_, change_index, block) => div()
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
                .into_any_element(),
            TranscriptRow::Notice => div()
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
                .into_any_element(),
        };
        div().px_6().min_w_0().child(row).into_any_element()
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
