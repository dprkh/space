use std::{cell::OnceCell, ops::Range};

use gpui::{prelude::*, *};

use crate::markdown::CachedText;

const LINES_PER_BLOCK: usize = 24;

pub struct FileDiff {
    source: SharedString,
    blocks: Vec<DiffBlock>,
    pub added: usize,
    pub removed: usize,
}

struct DiffBlock {
    range: Range<usize>,
    numbers: LineNumbers,
    lines: OnceCell<Vec<DiffLine>>,
}

struct DiffLine {
    kind: LineKind,
    old: Option<usize>,
    new: Option<usize>,
    text: CachedText,
}

#[derive(Clone, Copy, Default)]
struct LineNumbers {
    old: usize,
    new: usize,
}

#[derive(Clone, Copy)]
enum LineKind {
    Context,
    Added,
    Removed,
    Hunk,
    Note,
}

impl FileDiff {
    pub fn new(patch: &str) -> Self {
        let mut diff = Self {
            source: patch.to_owned().into(),
            blocks: Vec::new(),
            added: 0,
            removed: 0,
        };
        let mut offset = 0;
        let mut numbers = LineNumbers::default();
        for (index, line) in patch.split_inclusive('\n').enumerate() {
            let end = offset + line.len();
            if index >= 2 {
                if (index - 2).is_multiple_of(LINES_PER_BLOCK) {
                    diff.blocks.push(DiffBlock {
                        range: offset..end,
                        numbers,
                        lines: OnceCell::new(),
                    });
                } else if let Some(block) = diff.blocks.last_mut() {
                    block.range.end = end;
                }
                match parse_line(line, &mut numbers).0 {
                    LineKind::Added => diff.added += 1,
                    LineKind::Removed => diff.removed += 1,
                    _ => {}
                }
            }
            offset = end;
        }
        diff
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len().max(1)
    }

    pub fn render_block(&self, index: usize) -> Div {
        let view = div()
            .min_w_0()
            .flex()
            .flex_col()
            .font_family("Menlo")
            .text_size(px(12.))
            .line_height(px(20.))
            .bg(rgb(0x1b1b21))
            .border_l_1()
            .border_color(rgb(0x3b3b45));
        let Some(block) = self.blocks.get(index) else {
            return view
                .px_3()
                .py_2()
                .text_color(rgb(0x858592))
                .child("No text changes.");
        };
        let lines = block.lines.get_or_init(|| {
            let mut numbers = block.numbers;
            self.source[block.range.clone()]
                .split_inclusive('\n')
                .map(|line| {
                    let (kind, old, new, text) = parse_line(line, &mut numbers);
                    DiffLine {
                        kind,
                        old,
                        new,
                        text: CachedText::new(text.to_owned()),
                    }
                })
                .collect()
        });
        view.children(lines.iter().map(|line| {
            let (marker, foreground, background) = match line.kind {
                LineKind::Added => ("+", 0xb0d6b8, 0x20332a),
                LineKind::Removed => ("−", 0xe2b0b0, 0x39262b),
                LineKind::Hunk => ("", 0x96abc9, 0x222835),
                LineKind::Context => ("", 0xb3b3c0, 0x1b1b21),
                LineKind::Note => ("", 0x858592, 0x1b1b21),
            };
            div()
                .min_w_0()
                .min_h(px(20.))
                .flex()
                .bg(rgb(background))
                .text_color(rgb(foreground))
                .child(line_number(line.old))
                .child(line_number(line.new))
                .child(div().w(px(20.)).flex_shrink_0().child(marker))
                .child(div().flex_1().min_w_0().pr_2().child(line.text.clone()))
        }))
    }
}

fn line_number(number: Option<usize>) -> Div {
    div()
        .w(px(52.))
        .pr_2()
        .flex_shrink_0()
        .text_right()
        .text_color(rgb(0x73737f))
        .child(number.map_or_else(String::new, |number| number.to_string()))
}

fn parse_line<'a>(
    line: &'a str,
    numbers: &mut LineNumbers,
) -> (LineKind, Option<usize>, Option<usize>, &'a str) {
    let line = line.trim_end_matches(['\n', '\r']);
    if line.starts_with("@@ ") {
        let mut fields = line.split_whitespace().skip(1);
        if let (Some(old), Some(new)) = (fields.next(), fields.next()) {
            numbers.old = range_start(old);
            numbers.new = range_start(new);
        }
        return (LineKind::Hunk, None, None, line);
    }
    let old = numbers.old;
    let new = numbers.new;
    match line.as_bytes().first() {
        Some(b'+') => {
            numbers.new += 1;
            (LineKind::Added, None, Some(new), &line[1..])
        }
        Some(b'-') => {
            numbers.old += 1;
            (LineKind::Removed, Some(old), None, &line[1..])
        }
        Some(b' ') => {
            numbers.old += 1;
            numbers.new += 1;
            (LineKind::Context, Some(old), Some(new), &line[1..])
        }
        _ => (LineKind::Note, None, None, line),
    }
}

fn range_start(range: &str) -> usize {
    range
        .get(1..)
        .unwrap_or_default()
        .split(',')
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_default()
}
