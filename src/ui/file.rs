use super::*;
use space::project::FileContent;
use std::{cell::OnceCell, ops::Range, path::PathBuf, sync::Arc};

const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 21.;
const PADDING: f32 = 16.;
const FONT_FAMILY: &str = "Menlo";

pub(super) fn initialize(window: &Window) {
    for (font_weight, font_style) in [
        (FontWeight::NORMAL, FontStyle::Normal),
        (FontWeight::BOLD, FontStyle::Normal),
        (FontWeight::NORMAL, FontStyle::Italic),
        (FontWeight::BOLD, FontStyle::Italic),
    ] {
        let style = TextStyle {
            font_family: FONT_FAMILY.into(),
            font_weight,
            font_style,
            ..Default::default()
        };
        window
            .text_system()
            .shape_line(" ".into(), px(FONT_SIZE), &[style.to_run(1)], None);
    }
}

pub(super) struct FileView {
    request_id: Uuid,
    path: PathBuf,
    focus: FocusHandle,
    document: Option<SourceDocument>,
    error: Option<String>,
    watch_error: Option<String>,
    scroll: UniformListScrollHandle,
}

struct SourceDocument {
    content: Arc<FileContent>,
    lines: Vec<SourceLine>,
    highlighter: Option<super::syntax::Highlighter>,
    // Offscreen lines extend this width when they become visible.
    width: Pixels,
}

struct SourceLine {
    text: SharedString,
    runs: Vec<TextRun>,
    width: OnceCell<Pixels>,
}

impl SourceDocument {
    fn new(content: Arc<FileContent>) -> Self {
        Self {
            content,
            lines: Vec::new(),
            highlighter: super::syntax::Highlighter::new("md"),
            width: px(0.),
        }
    }

    fn measure(&mut self, range: Range<usize>, window: &Window) {
        self.highlight_through(range.end);
        for line in &self.lines[range] {
            if line.text.is_empty() {
                continue;
            }
            let width = line.width.get_or_init(|| {
                window
                    .text_system()
                    .shape_line(line.text.clone(), px(FONT_SIZE), &line.runs, None)
                    .width
                    .ceil()
            });
            self.width = self.width.max(*width);
        }
    }

    fn highlight_through(&mut self, end: usize) {
        // Parse in order to retain Markdown block and embedded language state.
        while self.lines.len() < end {
            let index = self.lines.len();
            let start = self.content.line_starts[index];
            let end = self
                .content
                .line_starts
                .get(index + 1)
                .copied()
                .unwrap_or(self.content.text.len());
            let raw = &self.content.text[start..end];
            let highlights = self
                .highlighter
                .as_mut()
                .and_then(|highlighter| highlighter.line(raw))
                .unwrap_or_else(|| vec![(raw.len(), HighlightStyle::default())]);
            self.lines.push(SourceLine::new(raw, highlights));
        }
    }
}

impl SourceLine {
    fn new(raw: &str, highlights: Vec<(usize, HighlightStyle)>) -> Self {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let style = TextStyle {
            font_family: FONT_FAMILY.into(),
            color: rgb(0xe5e5e8).into(),
            ..Default::default()
        };
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut column = 0;
        let mut offset = 0;
        for (length, highlight) in highlights {
            if offset >= line.len() {
                break;
            }
            let end = (offset + length).min(line.len());
            let part = &line[offset..end];
            offset = end;
            let start = text.len();
            for character in part.chars() {
                if character == '\t' {
                    let spaces = 4 - column % 4;
                    text.extend(std::iter::repeat_n(' ', spaces));
                    column += spaces;
                } else {
                    text.push(character);
                    column += 1;
                }
            }
            if text.len() > start {
                runs.push(
                    style
                        .clone()
                        .highlight(highlight)
                        .to_run(text.len() - start),
                );
            }
        }
        Self {
            text: text.into(),
            runs,
            width: OnceCell::new(),
        }
    }

    fn render(&self, width: Pixels) -> Div {
        div()
            .h(px(LINE_HEIGHT))
            .min_w(width + px(PADDING * 2.))
            .px(px(PADDING))
            .whitespace_nowrap()
            .font_family(FONT_FAMILY)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .child(StyledText::new(self.text.clone()).with_runs(self.runs.clone()))
    }
}

impl FileView {
    pub(super) fn new(
        request_id: Uuid,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        Self {
            request_id,
            path,
            focus,
            document: None,
            error: None,
            watch_error: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub(super) fn matches(&self, request_id: Uuid, path: &std::path::Path) -> bool {
        self.request_id == request_id && self.path == path
    }

    pub(super) fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.document = None;
        self.error = Some(error);
        cx.notify();
    }

    pub(super) fn set_watch_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.watch_error = Some(error);
        cx.notify();
    }

    pub(super) fn update_content(
        &mut self,
        content: Result<Arc<FileContent>, String>,
        cx: &mut Context<Self>,
    ) {
        let source = match content {
            Ok(source) => source,
            Err(error) => return self.fail(error, cx),
        };
        self.error = None;
        self.document = Some(SourceDocument::new(source));
        cx.notify();
    }

    fn clamp_scroll(&self) {
        let Some(document) = &self.document else {
            return;
        };
        let scroll = self.scroll.0.borrow();
        let bounds = scroll.base_handle.bounds();
        let offset = scroll.base_handle.offset();
        let width = (document.width + px(PADDING * 2.) - bounds.size.width).max(px(0.));
        let height =
            (px(LINE_HEIGHT) * document.content.line_starts.len() - bounds.size.height).max(px(0.));
        scroll.base_handle.set_offset(point(
            offset.x.clamp(-width, px(0.)),
            offset.y.clamp(-height, px(0.)),
        ));
    }

    fn prepare_viewport(&mut self, window: &Window) {
        let Some(document) = &mut self.document else {
            return;
        };
        let scroll = self.scroll.0.borrow();
        let bounds = scroll.base_handle.bounds();
        let offset = scroll.base_handle.offset();
        let height = if bounds.size.height > px(0.) {
            bounds.size.height
        } else {
            window.viewport_size().height
        };
        let max_scroll =
            (px(LINE_HEIGHT) * document.content.line_starts.len() - height).max(px(0.));
        let top = (-offset.y).clamp(px(0.), max_scroll);
        let start = (top / px(LINE_HEIGHT)).floor() as usize;
        let end = ((top + height) / px(LINE_HEIGHT)).ceil() as usize;
        document.measure(start..end.min(document.content.line_starts.len()), window);
        drop(scroll);
        self.clamp_scroll();
    }

    fn render_lines(
        &mut self,
        range: Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let Some(document) = &mut self.document else {
            return Vec::new();
        };
        let width = document.width;
        document.measure(range.clone(), window);
        if document.width > width {
            cx.notify();
        }
        document.lines[range]
            .iter()
            .map(|line| line.render(document.width))
            .collect()
    }
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_viewport(window);
        div()
            .key_context("FileView")
            .track_focus(&self.focus)
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    this.focus.focus(window);
                }),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(div().p_4().text_color(rgb(0xf0aaaa)).child(error))
            })
            .when_some(self.watch_error.clone(), |view, error| {
                view.child(div().px_4().py_2().text_color(rgb(0xd5b98c)).child(error))
            })
            .when_some(self.document.as_ref(), |view, document| {
                view.child(
                    uniform_list(
                        "file-source",
                        document.content.line_starts.len(),
                        cx.processor(|this, range, window, cx| {
                            this.render_lines(range, window, cx)
                        }),
                    )
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .track_scroll(self.scroll.clone())
                    .flex_1()
                    .min_h_0()
                    .min_w_0(),
                )
            })
    }
}

impl Focusable for FileView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub(super) fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.is_some() {
            return;
        }
        self.close_diagram(cx);
        self.chat_menu = None;
        self.model_menu = None;
        if let Some(file) = &self.file
            && file.read(cx).path == path
            && file.read(cx).error.is_none()
            && file.read(cx).watch_error.is_none()
        {
            file.read(cx).focus_handle(cx).focus(window);
            cx.notify();
            return;
        }
        let request_id = Uuid::new_v4();
        let file = cx.new(|cx| FileView::new(request_id, path.clone(), window, cx));
        if let Some(error) = &self.sidebar.error {
            file.update(cx, |file, cx| file.set_watch_error(error.clone(), cx));
        }
        if self
            .sidebar_commands
            .try_send(SidebarCommand::OpenFile {
                request_id,
                path: path.clone(),
            })
            .is_err()
        {
            file.update(cx, |file, cx| {
                file.fail("The file worker is unavailable".into(), cx);
            });
        }
        self.sidebar
            .tree
            .update(cx, |tree, cx| tree.select(Some(path), cx));
        self.file = Some(file);
        cx.notify();
    }

    pub(super) fn close_file(
        &mut self,
        _: &BackToChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file.take().is_none() {
            return;
        }
        self.chat_menu = None;
        self.model_menu = None;
        if self
            .sidebar_commands
            .try_send(SidebarCommand::CloseFile)
            .is_err()
        {
            self.error = Some("The file worker is unavailable".into());
        }
        self.sidebar
            .tree
            .update(cx, |tree, cx| tree.select(None, cx));
        self.input.read(cx).focus_handle(cx).focus(window);
        cx.notify();
    }
}
