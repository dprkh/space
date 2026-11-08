use super::*;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
pub(super) enum Mode {
    #[default]
    Normal,
    Insert,
    Visual {
        anchor: usize,
        cursor: usize,
        linewise: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
enum Operator {
    Delete,
    Change,
    Yank,
    Case(Case),
}

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
enum Case {
    Lower,
    Upper,
    Toggle,
}

impl Case {
    fn convert(self, text: &str) -> String {
        match self {
            Self::Lower => text.to_lowercase(),
            Self::Upper => text.to_uppercase(),
            Self::Toggle => {
                let mut result = String::with_capacity(text.len());
                for ch in text.chars() {
                    if ch.is_lowercase() {
                        result.extend(ch.to_uppercase());
                    } else {
                        result.extend(ch.to_lowercase());
                    }
                }
                result
            }
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Snapshot {
    text: String,
    cursor: usize,
}

#[derive(Clone, Deserialize, Serialize)]
struct Register {
    text: String,
    linewise: bool,
}

struct Motion {
    target: usize,
    inclusive: bool,
    linewise: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct Vim {
    pub(super) mode: Mode,
    count: Option<usize>,
    operator: Option<(Operator, usize)>,
    g_prefix: bool,
    replace: Option<usize>,
    text_object: Option<bool>,
    column: Option<usize>,
    register: Option<Register>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    insert_start: Option<Snapshot>,
    line_jump: Option<String>,
}

impl Vim {
    pub(super) fn validate(&self, text: &str) -> anyhow::Result<()> {
        if let Mode::Visual { anchor, cursor, .. } = self.mode {
            anyhow::ensure!(
                text.is_char_boundary(anchor) && text.is_char_boundary(cursor),
                "Invalid saved selection"
            );
        }
        for snapshot in self
            .undo
            .iter()
            .chain(&self.redo)
            .chain(self.insert_start.iter())
        {
            anyhow::ensure!(
                snapshot.text.is_char_boundary(snapshot.cursor),
                "Invalid saved undo position"
            );
        }
        for count in [
            self.count,
            self.replace,
            self.operator.map(|(_, count)| count),
        ]
        .into_iter()
        .flatten()
        {
            anyhow::ensure!((1..=10_000).contains(&count), "Invalid saved command count");
        }
        if let Some(command) = &self.line_jump {
            anyhow::ensure!(
                command.bytes().all(|byte| byte.is_ascii_digit()),
                "Invalid saved line command"
            );
        }
        Ok(())
    }

    fn reset_command(&mut self) {
        self.count = None;
        self.operator = None;
        self.g_prefix = false;
        self.replace = None;
        self.text_object = None;
        self.line_jump = None;
    }
}

impl Input {
    pub fn is_command_mode(&self) -> bool {
        self.vim.line_jump.is_some()
    }

    pub(super) fn is_insert_mode(&self) -> bool {
        self.single_line || matches!(self.vim.mode, Mode::Insert)
    }

    pub(super) fn key_context(&self) -> &'static str {
        if self.single_line {
            return "ChatName";
        }
        if self.is_command_mode() {
            return "Composer VimCommand";
        }
        match self.vim.mode {
            Mode::Normal => "Composer VimNormal",
            Mode::Insert => "Composer VimInsert",
            Mode::Visual { .. } => "Composer VimVisual",
        }
    }

    pub(super) fn vim_status(&self) -> SharedString {
        if let Some(command) = &self.vim.line_jump {
            return format!(":{command}").into();
        }
        match self.vim.mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual { linewise: true, .. } => "VISUAL LINE",
            Mode::Visual { .. } => "VISUAL",
        }
        .into()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor: self.cursor(),
        }
    }

    pub(super) fn edit(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        if self.text[range.clone()] == *text {
            self.marked = None;
            if !self.is_insert_mode() {
                self.vim.mode = Mode::Normal;
            }
            self.move_to(range.start + text.len(), false, cx);
            return;
        }
        if !self.single_line {
            if self.is_insert_mode() {
                if self.vim.insert_start.is_none() {
                    self.vim.insert_start = Some(self.snapshot());
                }
            } else {
                self.vim.undo.push(self.snapshot());
                self.vim.mode = Mode::Normal;
            }
            self.vim.redo.clear();
            self.vim.column = None;
        }
        self.text.replace_range(range.clone(), text);
        self.revision += 1;
        self.marked = None;
        self.move_to(range.start + text.len(), false, cx);
    }

    fn finish_insert(&mut self) {
        if let Some(snapshot) = self.vim.insert_start.take()
            && snapshot.text != self.text
        {
            self.vim.undo.push(snapshot);
        }
    }

    fn begin_insert(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.vim.insert_start = Some(self.snapshot());
        self.vim.mode = Mode::Insert;
        self.vim.reset_command();
        self.move_to(offset, false, cx);
    }

    pub(super) fn normal_mode(&mut self, _: &NormalMode, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.vim.mode, Mode::Normal)
            && self.vim.operator.is_none()
            && self.vim.count.is_none()
            && !self.vim.g_prefix
            && self.vim.replace.is_none()
            && self.vim.text_object.is_none()
            && !self.is_command_mode()
        {
            cx.propagate();
            return;
        }
        let mut cursor = self.cursor();
        if self.is_insert_mode() {
            self.finish_insert();
            if cursor > self.logical_line(cursor).start {
                cursor = self.previous_at(cursor);
            }
        }
        self.marked = None;
        self.vim.mode = Mode::Normal;
        self.vim.reset_command();
        self.vim.column = None;
        self.move_to(cursor, false, cx);
    }

    pub(super) fn vim_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_insert_mode() {
            return;
        }
        let stroke = &event.keystroke;
        if stroke.modifiers.platform || stroke.modifiers.alt {
            return;
        }
        if self.is_command_mode() {
            if !stroke.modifiers.control {
                let key = stroke.key_char.as_deref().unwrap_or(&stroke.key);
                self.line_jump_key(key, cx);
            } else if stroke.key == "c" {
                self.normal_mode(&NormalMode, window, cx);
            }
        } else if stroke.modifiers.control {
            match stroke.key.as_str() {
                "r" => {
                    let count = self.vim.count.take().unwrap_or(1);
                    self.vim_undo(true, count, cx);
                }
                "c" => self.normal_mode(&NormalMode, window, cx),
                _ => return,
            }
        } else {
            let key = stroke.key_char.as_deref().unwrap_or(&stroke.key);
            self.vim_command(key, cx);
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn line_jump_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(command) = &mut self.vim.line_jump else {
            return;
        };
        match key {
            "backspace" => {
                if command.pop().is_none() {
                    self.vim.line_jump = None;
                }
            }
            key if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() => command.push_str(key),
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn confirm_line_jump(
        &mut self,
        _: &ConfirmLineJump,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(command) = self.vim.line_jump.take() else {
            return;
        };
        self.vim.reset_command();
        self.vim.column = None;
        if !command.is_empty() {
            let line = command.parse::<usize>().unwrap_or(usize::MAX);
            let offset = self.first_nonblank(self.line_at(line));
            self.move_to(offset, false, cx);
        }
        cx.notify();
    }

    fn vim_command(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(around) = self.vim.text_object.take() {
            self.word_object(key, around, cx);
            return;
        }
        if let Some(count) = self.vim.replace.take() {
            self.vim_replace(key, count, cx);
            return;
        }
        if let Ok(digit) = key.parse::<usize>()
            && key.len() == 1
            && (digit != 0 || self.vim.count.is_some())
        {
            self.vim.count = Some((self.vim.count.unwrap_or(0) * 10 + digit).min(10_000));
            return;
        }
        if key == "g" && !self.vim.g_prefix {
            self.vim.g_prefix = true;
            return;
        }
        let key = if std::mem::take(&mut self.vim.g_prefix) {
            match key {
                "g" => "gg",
                "u" => "gu",
                "U" => "gU",
                "~" => "g~",
                _ => {
                    self.vim.reset_command();
                    return;
                }
            }
        } else {
            key
        };
        if matches!(key, "i" | "a")
            && (self.vim.operator.is_some() || matches!(self.vim.mode, Mode::Visual { .. }))
        {
            self.vim.text_object = Some(key == "a");
            return;
        }
        let explicit_count = self.vim.count.take();
        let count = explicit_count.unwrap_or(1);
        if let Some(operator) = self.vim.operator.take() {
            self.complete_operator(key, operator, explicit_count, cx);
            return;
        }
        if let Some(motion) = self.vim_motion(key, count, explicit_count.is_some()) {
            self.move_to(motion.target, false, cx);
            return;
        }
        if let Some(operator) = operator_key(key) {
            if let Mode::Visual { linewise, .. } = self.vim.mode {
                self.apply_operator(operator, self.selection.clone(), linewise, cx);
            } else {
                self.vim.operator = Some((operator, count));
            }
            return;
        }
        self.execute_command(key, count, cx);
    }

    fn execute_command(&mut self, key: &str, count: usize, cx: &mut Context<Self>) {
        match key {
            ":" if matches!(self.vim.mode, Mode::Normal) => {
                self.vim.reset_command();
                self.vim.line_jump = Some(String::new());
                cx.notify();
            }
            "i" => self.begin_insert(self.cursor(), cx),
            "a" => self.begin_insert(self.next().min(self.logical_line(self.cursor()).end), cx),
            "I" => self.begin_insert(self.first_nonblank(self.cursor()), cx),
            "A" => self.begin_insert(self.logical_line(self.cursor()).end, cx),
            "o" => self.open_line(false, cx),
            "O" => self.open_line(true, cx),
            "v" => self.toggle_visual(false, cx),
            "V" => self.toggle_visual(true, cx),
            "x" | "delete" | "s" => self.delete_characters(key == "s", false, count, cx),
            "X" => self.delete_characters(false, true, count, cx),
            "D" | "C" => {
                let end = self.line_end(count);
                let operator = if key == "C" {
                    Operator::Change
                } else {
                    Operator::Delete
                };
                self.apply_operator(operator, self.cursor()..end, false, cx);
            }
            "S" | "Y" => {
                let operator = if key == "S" {
                    Operator::Change
                } else {
                    Operator::Yank
                };
                self.apply_operator(operator, self.line_range(self.cursor(), count), true, cx);
            }
            "p" | "P" => self.vim_paste(key == "P", count, cx),
            "u" | "U" | "~" if matches!(self.vim.mode, Mode::Visual { .. }) => {
                let case = match key {
                    "u" => Case::Lower,
                    "U" => Case::Upper,
                    _ => Case::Toggle,
                };
                self.apply_operator(Operator::Case(case), self.selection.clone(), false, cx);
            }
            "~" => self.toggle_case(count, cx),
            "u" => self.vim_undo(false, count, cx),
            "r" => self.vim.replace = Some(count),
            _ => self.vim.reset_command(),
        }
    }

    fn complete_operator(
        &mut self,
        key: &str,
        (operator, prefix_count): (Operator, usize),
        count: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let explicit_count = count.is_some() || prefix_count > 1;
        let count = (count.unwrap_or(1) * prefix_count).min(10_000);
        let repeated_case = matches!(
            (operator, key),
            (Operator::Case(Case::Lower), "u")
                | (Operator::Case(Case::Upper), "U")
                | (Operator::Case(Case::Toggle), "~")
        );
        if repeated_case || operator_key(key) == Some(operator) {
            self.apply_operator(operator, self.line_range(self.cursor(), count), true, cx);
            return;
        }
        if operator == Operator::Change
            && matches!(key, "w" | "W")
            && self.word_class(self.cursor(), key == "W") != 0
        {
            let range = self.cursor()..self.word_end_after(key == "W", count);
            self.apply_operator(operator, range, false, cx);
            return;
        }
        let Some(motion) = self.vim_motion(key, count, explicit_count) else {
            return;
        };
        let mut range = self.operator_range(&motion);
        if matches!(key, "w" | "W") {
            let word_end = self.text[..range.end]
                .trim_end_matches(char::is_whitespace)
                .len();
            if word_end > range.start && self.text[word_end..range.end].contains('\n') {
                range.end = self.logical_line(self.previous_at(word_end)).end;
            }
        }
        self.apply_operator(operator, range, motion.linewise, cx);
    }

    fn logical_line(&self, offset: usize) -> Range<usize> {
        let start = self.text[..offset].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |i| offset + i);
        start..end
    }

    fn first_nonblank(&self, offset: usize) -> usize {
        let line = self.logical_line(offset);
        line.start
            + self.text[line]
                .find(|ch| ch != ' ' && ch != '\t')
                .unwrap_or(0)
    }

    fn normal_offset(&self, offset: usize) -> usize {
        let offset = offset.min(self.text.len());
        let line = self.logical_line(offset);
        self.text[line.clone()]
            .grapheme_indices(true)
            .take_while(|(index, _)| line.start + index <= offset)
            .last()
            .map_or(line.start, |(index, _)| line.start + index)
    }

    pub(super) fn vim_move_to(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        let offset = self.normal_offset(offset);
        self.vim.mode = match self.vim.mode {
            Mode::Visual {
                anchor, linewise, ..
            } => Mode::Visual {
                anchor,
                cursor: offset,
                linewise,
            },
            _ if extend => Mode::Visual {
                anchor: self.cursor(),
                cursor: offset,
                linewise: false,
            },
            _ => Mode::Normal,
        };
        if let Mode::Visual {
            anchor,
            cursor,
            linewise,
        } = self.vim.mode
        {
            let start = anchor.min(cursor);
            let end = anchor.max(cursor);
            self.selection = if linewise {
                self.logical_line(start).start..self.line_range(end, 1).end
            } else {
                start..self.next_at(end)
            };
            self.reversed = cursor < anchor;
        } else {
            self.selection = offset..offset;
            self.reversed = false;
        }
        self.reveal_cursor = true;
        cx.notify();
    }

    pub(super) fn vim_mouse_down(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        self.vim.reset_command();
        self.vim.column = None;
        if !extend {
            self.vim.mode = Mode::Normal;
        }
        self.move_to(offset, extend, cx);
    }

    fn toggle_visual(&mut self, linewise: bool, cx: &mut Context<Self>) {
        let cursor = self.cursor();
        self.vim.mode = match self.vim.mode {
            Mode::Visual {
                linewise: current, ..
            } if current == linewise => Mode::Normal,
            Mode::Visual { anchor, .. } => Mode::Visual {
                anchor,
                cursor,
                linewise,
            },
            _ => Mode::Visual {
                anchor: cursor,
                cursor,
                linewise,
            },
        };
        self.move_to(cursor, false, cx);
    }

    pub(super) fn vim_select_all(&mut self, cx: &mut Context<Self>) {
        self.vim.reset_command();
        self.vim.mode = Mode::Visual {
            anchor: 0,
            cursor: self.normal_offset(self.text.len()),
            linewise: true,
        };
        self.move_to(self.cursor(), false, cx);
    }

    fn line_range(&self, offset: usize, count: usize) -> Range<usize> {
        let start = self.logical_line(offset).start;
        let mut end = start;
        for _ in 0..count {
            let line = self.logical_line(end);
            end = if line.end < self.text.len() {
                line.end + 1
            } else {
                line.end
            };
            if end == self.text.len() {
                break;
            }
        }
        start..end
    }

    fn line_at(&self, number: usize) -> usize {
        let mut start = 0;
        for _ in 1..number {
            let Some(newline) = self.text[start..].find('\n') else {
                break;
            };
            start += newline + 1;
        }
        start
    }

    fn line_end(&self, count: usize) -> usize {
        let mut end = self.logical_line(self.cursor()).end;
        for _ in 1..count {
            if end == self.text.len() {
                break;
            }
            end = self.logical_line(end + 1).end;
        }
        end
    }

    fn vertical_target(&mut self, down: bool, count: usize) -> usize {
        let cursor = self.cursor();
        let line = self.logical_line(cursor);
        let column = *self
            .vim
            .column
            .get_or_insert_with(|| self.text[line.start..cursor].graphemes(true).count());
        let mut target = line.start;
        for _ in 0..count {
            let line = self.logical_line(target);
            if down && line.end < self.text.len() {
                target = line.end + 1;
            } else if !down && line.start > 0 {
                target = self.logical_line(line.start - 1).start;
            } else {
                break;
            }
        }
        let line = self.logical_line(target);
        self.text[line.clone()]
            .grapheme_indices(true)
            .nth(column)
            .map_or(line.end, |(i, _)| line.start + i)
    }

    fn vim_motion(&mut self, key: &str, count: usize, explicit_count: bool) -> Option<Motion> {
        let cursor = self.cursor();
        let line = self.logical_line(cursor);
        if !matches!(key, "j" | "k" | "up" | "down") {
            self.vim.column = None;
        }
        let mut inclusive = false;
        let mut linewise = false;
        let target = match key {
            "h" | "left" | "backspace" => {
                let mut target = cursor;
                for _ in 0..count.min(self.text.len() + 1) {
                    target = self.previous_at(target).max(line.start);
                }
                target
            }
            "l" | "right" | " " => {
                let mut target = cursor;
                for _ in 0..count.min(self.text.len() + 1) {
                    target = self.next_at(target).min(line.end);
                }
                target
            }
            "j" | "down" | "k" | "up" => {
                linewise = true;
                self.vertical_target(matches!(key, "j" | "down"), count)
            }
            "w" | "W" | "b" | "B" | "e" | "E" => {
                inclusive = matches!(key, "e" | "E");
                let mut target = cursor;
                for _ in 0..count.min(self.text.len() + 1) {
                    target = self.word_target(target, key);
                }
                target
            }
            "0" | "home" => line.start,
            "^" => self.first_nonblank(cursor),
            "$" | "end" => {
                inclusive = true;
                self.normal_offset(self.line_end(count))
            }
            "gg" | "G" => {
                linewise = true;
                let offset = if explicit_count {
                    self.line_at(count)
                } else if key == "gg" {
                    0
                } else {
                    self.text.len()
                };
                self.first_nonblank(offset)
            }
            "[" | "]" => self.code_block_target(key == "]", count),
            _ => return None,
        };
        Some(Motion {
            target,
            inclusive,
            linewise,
        })
    }

    fn code_block_target(&self, forward: bool, count: usize) -> usize {
        let cursor = self.cursor();
        let openings: Vec<_> = Parser::new(&self.text)
            .into_offset_iter()
            .filter_map(|(event, range)| match event {
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => Some(range.start),
                _ => None,
            })
            .collect();
        if forward {
            openings
                .into_iter()
                .filter(|&offset| offset > cursor)
                .take(count)
                .last()
                .unwrap_or(cursor)
        } else {
            openings
                .into_iter()
                .rev()
                .filter(|&offset| offset < cursor)
                .take(count)
                .last()
                .unwrap_or(cursor)
        }
    }

    fn word_class(&self, offset: usize, big: bool) -> u8 {
        match self.text[offset..].chars().next() {
            None => 0,
            Some(ch) if ch.is_whitespace() => 0,
            Some(ch) if big || ch.is_alphanumeric() || ch == '_' => 1,
            Some(_) => 2,
        }
    }

    fn word_target(&self, offset: usize, key: &str) -> usize {
        let big = matches!(key, "W" | "B" | "E");
        let mut target = offset;
        if matches!(key, "b" | "B") {
            target = self.previous_at(target);
            while target > 0 && self.word_class(target, big) == 0 {
                target = self.previous_at(target);
            }
            let class = self.word_class(target, big);
            while target > 0 && self.word_class(self.previous_at(target), big) == class {
                target = self.previous_at(target);
            }
            return target;
        }
        if matches!(key, "e" | "E") {
            target = self.next_at(target);
            while target < self.text.len() && self.word_class(target, big) == 0 {
                target = self.next_at(target);
            }
            let class = self.word_class(target, big);
            while self.next_at(target) < self.text.len()
                && self.word_class(self.next_at(target), big) == class
            {
                target = self.next_at(target);
            }
            return target;
        }
        let class = self.word_class(target, big);
        while target < self.text.len() && self.word_class(target, big) == class {
            target = self.next_at(target);
        }
        while target < self.text.len() && self.word_class(target, big) == 0 {
            target = self.next_at(target);
        }
        target
    }

    fn operator_range(&self, motion: &Motion) -> Range<usize> {
        let start = self.cursor().min(motion.target);
        let end = self.cursor().max(motion.target);
        if motion.linewise {
            self.logical_line(start).start..self.line_range(end, 1).end
        } else {
            start..if motion.inclusive && !self.text[end..].starts_with('\n') {
                self.next_at(end)
            } else {
                end
            }
        }
    }

    fn word_end_after(&self, big: bool, count: usize) -> usize {
        let mut end = self.cursor();
        for word in 0..count {
            if word > 0 {
                while end < self.text.len() && self.word_class(end, big) == 0 {
                    end = self.next_at(end);
                }
            }
            let class = self.word_class(end, big);
            while end < self.text.len() && self.word_class(end, big) == class {
                end = self.next_at(end);
            }
            if end == self.text.len() {
                break;
            }
        }
        end
    }

    fn word_object(&mut self, key: &str, around: bool, cx: &mut Context<Self>) {
        let operator = self.vim.operator.take();
        let count = self.vim.count.take().unwrap_or(1);
        let count = (count * operator.map_or(1, |(_, count)| count)).min(10_000);
        self.vim.reset_command();
        if !matches!(key, "w" | "W") || self.text.is_empty() {
            return;
        }
        let big = key == "W";
        let class = self.word_class(self.cursor(), big);
        let mut start = self.cursor();
        while start > self.logical_line(start).start
            && self.word_class(self.previous_at(start), big) == class
        {
            start = self.previous_at(start);
        }
        let mut end = self.word_end_after(big, count);
        if around {
            if class == 0 {
                let next_class = self.word_class(end, big);
                while end < self.text.len() && self.word_class(end, big) == next_class {
                    end = self.next_at(end);
                }
            }
            let word_end = end;
            while self.text[end..].starts_with([' ', '\t']) {
                end = self.next_at(end);
            }
            if end == word_end {
                while start > 0 && self.text[..start].ends_with([' ', '\t']) {
                    start = self.previous_at(start);
                }
            }
        }
        if start == end {
            return;
        }
        if let Some((operator, _)) = operator {
            self.apply_operator(operator, start..end, false, cx);
        } else {
            self.vim.mode = Mode::Visual {
                anchor: start,
                cursor: start,
                linewise: false,
            };
            self.move_to(self.previous_at(end), false, cx);
        }
    }

    fn apply_operator(
        &mut self,
        operator: Operator,
        range: Range<usize>,
        linewise: bool,
        cx: &mut Context<Self>,
    ) {
        self.vim.reset_command();
        self.vim.column = None;
        if let Operator::Case(case) = operator {
            self.change_case(range, case, cx);
            return;
        }
        self.write_register(range.clone(), linewise, cx);
        if operator == Operator::Yank {
            self.vim.mode = Mode::Normal;
            self.move_to(range.start, false, cx);
            return;
        }
        let mut deletion = range;
        if operator == Operator::Change {
            self.begin_insert(deletion.start, cx);
            if linewise && self.text[deletion.clone()].ends_with('\n') {
                deletion.end -= 1;
            }
        } else if linewise
            && deletion.end == self.text.len()
            && deletion.start > 0
            && !self.text[deletion.clone()].ends_with('\n')
        {
            deletion.start -= 1;
        }
        self.edit(deletion.clone(), "", cx);
        if operator == Operator::Delete {
            self.vim.mode = Mode::Normal;
        }
        let cursor = if linewise && operator == Operator::Delete {
            self.first_nonblank(deletion.start)
        } else {
            deletion.start
        };
        self.move_to(cursor, false, cx);
    }

    fn change_case(&mut self, range: Range<usize>, case: Case, cx: &mut Context<Self>) {
        let replacement = case.convert(&self.text[range.clone()]);
        let cursor = range.start;
        self.edit(range, &replacement, cx);
        self.move_to(cursor, false, cx);
    }

    fn toggle_case(&mut self, count: usize, cx: &mut Context<Self>) {
        let cursor = self.cursor();
        let line_end = self.logical_line(cursor).end;
        let mut end = cursor;
        for _ in 0..count.min(self.text.len() + 1) {
            end = self.next_at(end).min(line_end);
        }
        let replacement = Case::Toggle.convert(&self.text[cursor..end]);
        self.edit(cursor..end, &replacement, cx);
    }

    fn write_register(&mut self, range: Range<usize>, linewise: bool, cx: &mut Context<Self>) {
        let mut text = self.text[range].to_owned();
        if linewise && !text.ends_with('\n') {
            text.push('\n');
        }
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.vim.register = Some(Register { text, linewise });
    }

    pub(super) fn vim_copy(&mut self, cx: &mut Context<Self>) {
        let linewise = matches!(self.vim.mode, Mode::Visual { linewise: true, .. });
        self.write_register(self.selection.clone(), linewise, cx);
    }

    pub(super) fn vim_cut(&mut self, cx: &mut Context<Self>) {
        let linewise = matches!(self.vim.mode, Mode::Visual { linewise: true, .. });
        self.apply_operator(Operator::Delete, self.selection.clone(), linewise, cx);
    }

    fn delete_characters(
        &mut self,
        change: bool,
        backward: bool,
        count: usize,
        cx: &mut Context<Self>,
    ) {
        let operator = if change {
            Operator::Change
        } else {
            Operator::Delete
        };
        if let Mode::Visual { linewise, .. } = self.vim.mode {
            self.apply_operator(operator, self.selection.clone(), linewise, cx);
            return;
        }
        let cursor = self.cursor();
        let line = self.logical_line(cursor);
        let mut target = cursor;
        for _ in 0..count.min(self.text.len() + 1) {
            target = if backward {
                self.previous_at(target).max(line.start)
            } else {
                self.next_at(target).min(line.end)
            };
        }
        self.apply_operator(operator, target.min(cursor)..target.max(cursor), false, cx);
    }

    fn open_line(&mut self, above: bool, cx: &mut Context<Self>) {
        let line = self.logical_line(self.cursor());
        let offset = if above { line.start } else { line.end };
        self.begin_insert(offset, cx);
        self.edit(offset..offset, "\n", cx);
        self.move_to(if above { offset } else { offset + 1 }, false, cx);
    }

    fn vim_replace(&mut self, key: &str, count: usize, cx: &mut Context<Self>) {
        if key.graphemes(true).count() != 1 || key == "\n" {
            return;
        }
        let range = if matches!(self.vim.mode, Mode::Visual { .. }) {
            self.selection.clone()
        } else {
            let cursor = self.cursor();
            let line = self.logical_line(cursor);
            let Some((last, grapheme)) = self.text[cursor..line.end]
                .grapheme_indices(true)
                .nth(count - 1)
            else {
                return;
            };
            cursor..cursor + last + grapheme.len()
        };
        let replacement: String = self.text[range.clone()]
            .graphemes(true)
            .map(|g| if g == "\n" { "\n" } else { key })
            .collect();
        let cursor = range.start;
        self.edit(range, &replacement, cx);
        self.move_to(cursor, false, cx);
    }

    pub(super) fn vim_paste(&mut self, before: bool, count: usize, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .or_else(|| {
                self.vim
                    .register
                    .as_ref()
                    .map(|register| register.text.clone())
            })
        else {
            return;
        };
        let text = text.replace("\r\n", "\n");
        if text.is_empty() {
            return;
        }
        let linewise = self
            .vim
            .register
            .as_ref()
            .is_some_and(|register| register.linewise && register.text == text);
        let mut text = text.repeat(count);
        self.vim.reset_command();
        if let Mode::Visual { linewise, .. } = self.vim.mode {
            let start = self.selection.start;
            if linewise
                && self.text[self.selection.clone()].ends_with('\n')
                && !text.ends_with('\n')
            {
                text.push('\n');
            }
            self.edit(self.selection.clone(), &text, cx);
            self.move_to(start, false, cx);
            return;
        }
        let cursor = self.cursor();
        if linewise {
            let line = self.logical_line(cursor);
            let (offset, text, cursor) = if before {
                (line.start, text, line.start)
            } else if line.end < self.text.len() {
                (line.end + 1, text, line.end + 1)
            } else {
                (
                    line.end,
                    format!("\n{}", text.strip_suffix('\n').unwrap_or(&text)),
                    line.end + 1,
                )
            };
            self.edit(offset..offset, &text, cx);
            self.move_to(self.first_nonblank(cursor), false, cx);
        } else {
            let offset = if before {
                cursor
            } else {
                self.next().min(self.logical_line(cursor).end)
            };
            self.edit(offset..offset, &text, cx);
            self.move_to(self.previous_at(offset + text.len()), false, cx);
        }
    }

    fn vim_undo(&mut self, redo: bool, count: usize, cx: &mut Context<Self>) {
        self.vim.reset_command();
        for _ in 0..count {
            if !self.restore_snapshot(redo, cx) {
                break;
            }
        }
    }

    fn restore_snapshot(&mut self, redo: bool, cx: &mut Context<Self>) -> bool {
        let snapshot = if redo {
            self.vim.redo.pop()
        } else {
            self.vim.undo.pop()
        };
        let Some(snapshot) = snapshot else {
            return false;
        };
        let current = self.snapshot();
        if redo {
            self.vim.undo.push(current);
        } else {
            self.vim.redo.push(current);
        }
        self.text = snapshot.text;
        self.revision += 1;
        self.marked = None;
        self.vim.mode = Mode::Normal;
        self.vim.reset_command();
        self.vim.column = None;
        self.move_to(snapshot.cursor, false, cx);
        true
    }
}

fn operator_key(key: &str) -> Option<Operator> {
    match key {
        "d" => Some(Operator::Delete),
        "c" => Some(Operator::Change),
        "y" => Some(Operator::Yank),
        "gu" => Some(Operator::Case(Case::Lower)),
        "gU" => Some(Operator::Case(Case::Upper)),
        "g~" => Some(Operator::Case(Case::Toggle)),
        _ => None,
    }
}
