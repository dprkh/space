use std::{borrow::Cow, ops::Range};

use gpui::{prelude::*, *};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Newline
    ]
);

const LINE_HEIGHT: f32 = 22.;

pub fn bind_keys(cx: &mut App) {
    for context in ["Composer", "ChatName"] {
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, Some(context)),
            KeyBinding::new("delete", Delete, Some(context)),
            KeyBinding::new("left", Left, Some(context)),
            KeyBinding::new("right", Right, Some(context)),
            KeyBinding::new("up", Up, Some(context)),
            KeyBinding::new("down", Down, Some(context)),
            KeyBinding::new("shift-left", SelectLeft, Some(context)),
            KeyBinding::new("shift-right", SelectRight, Some(context)),
            KeyBinding::new("shift-up", SelectUp, Some(context)),
            KeyBinding::new("shift-down", SelectDown, Some(context)),
            KeyBinding::new("cmd-a", SelectAll, Some(context)),
            KeyBinding::new("cmd-v", Paste, Some(context)),
            KeyBinding::new("cmd-c", Copy, Some(context)),
            KeyBinding::new("cmd-x", Cut, Some(context)),
            KeyBinding::new("cmd-left", Home, Some(context)),
            KeyBinding::new("cmd-right", End, Some(context)),
        ]);
    }
    cx.bind_keys([KeyBinding::new("shift-enter", Newline, Some("Composer"))]);
}

struct InputLine {
    line: WrappedLine,
    start: usize,
    origin: Point<Pixels>,
}

pub struct Input {
    focus: FocusHandle,
    text: String,
    revision: u64,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    lines: Vec<InputLine>,
    bounds: Bounds<Pixels>,
    scroll: Pixels,
    content_height: Pixels,
    reveal_cursor: bool,
    dragging: bool,
    single_line: bool,
    scroll_x: Pixels,
}

impl Input {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            text: String::new(),
            revision: 0,
            selection: 0..0,
            reversed: false,
            marked: None,
            lines: Vec::new(),
            bounds: Bounds::default(),
            scroll: px(0.),
            content_height: px(0.),
            reveal_cursor: true,
            dragging: false,
            single_line: false,
            scroll_x: px(0.),
        }
    }

    pub fn single_line(text: String, cx: &mut Context<Self>) -> Self {
        let mut input = Self::new(cx);
        input.selection = 0..text.len();
        input.text = text;
        input.single_line = true;
        input
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        self.text.clear();
        self.selection = 0..0;
        self.marked = None;
        self.scroll = px(0.);
        self.scroll_x = px(0.);
        self.content_height = px(0.);
        self.reveal_cursor = true;
        cx.notify();
    }

    pub fn is_composing(&self) -> bool {
        self.marked.is_some()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn vertical(&mut self, direction: f32, extend: bool, cx: &mut Context<Self>) {
        if let Some(caret) = self.caret_bounds(self.cursor()) {
            let target = point(
                caret.left(),
                caret.top() + px((direction + 0.5) * LINE_HEIGHT),
            );
            self.move_to(self.index_at(target), extend, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1., false, cx);
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1., false, cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1., true, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1., true, cx);
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor()]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.text[self.cursor()..]
            .graphemes(true)
            .next()
            .map_or(self.text.len(), |g| self.cursor() + g.len())
    }

    fn move_to(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        let anchor = if extend {
            if self.reversed {
                self.selection.end
            } else {
                self.selection.start
            }
        } else {
            offset
        };
        self.selection = anchor.min(offset)..anchor.max(offset);
        self.reversed = offset < anchor;
        self.reveal_cursor = true;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(
            if self.selection.is_empty() {
                self.previous()
            } else {
                self.selection.start
            },
            false,
            cx,
        );
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(
            if self.selection.is_empty() {
                self.next()
            } else {
                self.selection.end
            },
            false,
            cx,
        );
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous(), true, cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next(), true, cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = 0..self.text.len();
        self.reveal_cursor = true;
        cx.notify();
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, false, cx);
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.text.len(), false, cx);
    }
    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_to(self.previous(), true, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_to(self.next(), true, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].into(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        self.replace_text_in_range(None, "", window, cx);
    }
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace("\r\n", "\n"), window, cx);
        }
    }

    fn max_scroll(&self) -> Pixels {
        (self.content_height - self.bounds.size.height).max(px(0.))
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT)).y;
        let scroll = (self.scroll - delta).clamp(px(0.), self.max_scroll());
        self.reveal_cursor = false;
        if scroll != self.scroll {
            self.scroll = scroll;
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        for entry in &self.lines {
            let height = px(LINE_HEIGHT * (entry.line.wrap_boundaries.len() + 1) as f32);
            if position.y < entry.origin.y + height {
                let relative = point(
                    position.x - entry.origin.x,
                    (position.y - entry.origin.y).max(px(0.)),
                );
                let index = entry
                    .line
                    .closest_index_for_position(relative, px(LINE_HEIGHT))
                    .unwrap_or_else(|i| i);
                return (entry.start + index).min(self.text.len());
            }
        }
        self.text.len()
    }

    fn caret_bounds(&self, index: usize) -> Option<Bounds<Pixels>> {
        self.lines.iter().find_map(|entry| {
            if index < entry.start || index > entry.start + entry.line.len() {
                return None;
            }
            let position = entry
                .line
                .position_for_index(index - entry.start, px(LINE_HEIGHT))?;
            Some(Bounds::new(
                entry.origin + position,
                size(px(1.5), px(LINE_HEIGHT)),
            ))
        })
    }

    fn utf8_offset(&self, utf16: usize) -> usize {
        let mut count = 0;
        self.text
            .char_indices()
            .find_map(|(index, ch)| {
                if count >= utf16 {
                    return Some(index);
                }
                count += ch.len_utf16();
                None
            })
            .unwrap_or(self.text.len())
    }
    fn utf16_offset(&self, utf8: usize) -> usize {
        self.text[..utf8].encode_utf16().count()
    }
    fn range_to_utf8(&self, range: Range<usize>) -> Range<usize> {
        self.utf8_offset(range.start)..self.utf8_offset(range.end)
    }
    fn to_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.utf16_offset(range.start)..self.utf16_offset(range.end)
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.bounds = bounds;
        if !self.layout_lines(bounds, window) {
            return;
        }
        self.reveal_caret(bounds);
        self.position_lines();
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, cx.entity()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.paint_visible_lines(bounds, window, cx);
            self.paint_caret(window);
        });
    }

    fn styled_text(&self, window: &Window) -> (SharedString, Vec<TextRun>) {
        let placeholder = self.text.is_empty();
        let text: SharedString = if placeholder {
            if self.single_line {
                "Chat name"
            } else {
                "Send a message…"
            }
            .into()
        } else {
            self.text.clone().into()
        };
        let mut boundaries = vec![0, text.len()];
        if !placeholder {
            boundaries.extend([self.selection.start, self.selection.end]);
            if let Some(marked) = &self.marked {
                boundaries.extend([marked.start, marked.end]);
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let runs: Vec<_> = boundaries
            .windows(2)
            .map(|range| TextRun {
                len: range[1] - range[0],
                font: window.text_style().font(),
                color: rgb(if placeholder { 0x85858c } else { 0xe5e5e8 }).into(),
                background_color: (!placeholder && self.selection.contains(&range[0]))
                    .then(|| rgba(0x7070a866).into()),
                underline: self
                    .marked
                    .as_ref()
                    .filter(|r| r.contains(&range[0]))
                    .map(|_| UnderlineStyle {
                        thickness: px(1.),
                        color: None,
                        wavy: false,
                    }),
                strikethrough: None,
            })
            .collect();
        (text, runs)
    }

    fn layout_lines(&mut self, bounds: Bounds<Pixels>, window: &mut Window) -> bool {
        let (text, runs) = self.styled_text(window);
        let Ok(shaped) = window.text_system().shape_text(
            text,
            px(14.),
            &runs,
            (!self.single_line).then_some(bounds.size.width),
            None,
        ) else {
            return false;
        };
        self.lines.clear();
        let mut origin = bounds.origin;
        let mut start = 0;
        for line in shaped {
            let height = px(LINE_HEIGHT * (line.wrap_boundaries.len() + 1) as f32);
            let length = line.len();
            self.lines.push(InputLine {
                start,
                line,
                origin,
            });
            start += length + 1;
            origin.y += height;
        }
        self.content_height = origin.y - bounds.top();
        true
    }

    fn reveal_caret(&mut self, bounds: Bounds<Pixels>) {
        if std::mem::take(&mut self.reveal_cursor)
            && let Some(caret) = self.caret_bounds(self.cursor())
        {
            self.scroll = self.scroll.min(caret.top() - bounds.top()).max(px(0.));
            if caret.bottom() - self.scroll > bounds.bottom() {
                self.scroll = caret.bottom() - bounds.bottom();
            }
            if self.single_line {
                self.scroll_x = self.scroll_x.min(caret.left() - bounds.left()).max(px(0.));
                if caret.right() - self.scroll_x > bounds.right() {
                    self.scroll_x = caret.right() - bounds.right();
                }
            }
        }
    }

    fn position_lines(&mut self) {
        self.scroll = self.scroll.clamp(px(0.), self.max_scroll());
        for entry in &mut self.lines {
            entry.origin.y -= self.scroll;
            entry.origin.x -= self.scroll_x;
        }
    }

    fn paint_visible_lines(
        &self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for entry in &self.lines {
            if entry.origin.y >= bounds.bottom() {
                break;
            }
            let height = px(LINE_HEIGHT * (entry.line.wrap_boundaries.len() + 1) as f32);
            if entry.origin.y + height <= bounds.top() {
                continue;
            }
            let _ = entry.line.paint_background(
                entry.origin,
                px(LINE_HEIGHT),
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            );
            let _ = entry.line.paint(
                entry.origin,
                px(LINE_HEIGHT),
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            );
        }
    }

    fn paint_caret(&self, window: &mut Window) {
        if self.focus.is_focused(window)
            && self.selection.is_empty()
            && let Some(caret) = self.caret_bounds(self.cursor())
        {
            window.paint_quad(fill(caret, rgb(0xb7b7ed)));
        }
    }
}

impl EntityInputHandler for Input {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_to_utf8(range);
        *actual = Some(self.to_utf16(range.clone()));
        Some(self.text[range].into())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.to_utf16(self.selection.clone()),
            reversed: self.reversed,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.clone().map(|r| self.to_utf16(r))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = if self.single_line {
            Cow::Owned(text.replace(['\n', '\r'], " "))
        } else {
            Cow::Borrowed(text)
        };
        let range = range
            .map(|r| self.range_to_utf8(r))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        self.text.replace_range(range.clone(), &text);
        self.revision += 1;
        self.marked = None;
        self.move_to(range.start + text.len(), false, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|r| self.range_to_utf8(r))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        let base_utf16 = self.utf16_offset(range.start);
        self.replace_text_in_range(Some(self.to_utf16(range.clone())), text, window, cx);
        self.marked = (!text.is_empty()).then_some(range.start..range.start + text.len());
        if let Some(selected) = selected {
            self.selection =
                self.range_to_utf8(base_utf16 + selected.start..base_utf16 + selected.end);
        }
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.caret_bounds(self.utf8_offset(range.start))
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.utf16_offset(self.index_at(point)))
    }
}

impl Focusable for Input {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Input {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let input = cx.entity();
        div()
            .w_full()
            .h_full()
            .min_h_0()
            .min_w_0()
            .key_context(if self.single_line {
                "ChatName"
            } else {
                "Composer"
            })
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::newline))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.focus.focus(window);
                    this.dragging = true;
                    this.move_to(this.index_at(event.position), event.modifiers.shift, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.dragging {
                    this.move_to(this.index_at(event.position), true, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        input.update(cx, |input, cx| input.paint(bounds, window, cx));
                    },
                )
                .size_full(),
            )
    }
}
