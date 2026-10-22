use std::{
    cell::{OnceCell, RefCell},
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{prelude::*, *};
use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag};

const FADE_DURATION: Duration = Duration::from_millis(220);

#[derive(Default)]
pub struct Markdown {
    nodes: Vec<Node>,
    updated_at: Vec<Option<Instant>>,
}

struct Node {
    kind: NodeKind,
    text: OnceCell<Rc<RichText>>,
    groups: OnceCell<Vec<Group>>,
}

enum Group {
    Inline(Rc<RichText>),
    Block(usize),
}

#[derive(PartialEq)]
enum NodeKind {
    Element(Tag<'static>, Vec<Node>),
    Leaf(Event<'static>),
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Node {
    fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            text: OnceCell::new(),
            groups: OnceCell::new(),
        }
    }

    fn rich_text(&self, animate: bool) -> RichTextView {
        let text = self.text.get_or_init(|| {
            let mut text = match &self.kind {
                NodeKind::Element(tag, children) => {
                    RichText::new(children, !matches!(tag, Tag::CodeBlock(_)))
                }
                NodeKind::Leaf(_) => RichText::new([self], true),
            };
            if let NodeKind::Element(Tag::CodeBlock(CodeBlockKind::Fenced(info)), _) = &self.kind
                && info.split_whitespace().next().is_some_and(|language| {
                    ["python", "py", "python3", "py3"]
                        .iter()
                        .any(|alias| language.eq_ignore_ascii_case(alias))
                })
                && let Some(runs) = crate::syntax::python(&text.text)
            {
                text.runs = runs
                    .into_iter()
                    .map(|(len, highlight)| {
                        (
                            len,
                            InlineStyle {
                                highlight,
                                ..Default::default()
                            },
                        )
                    })
                    .collect();
            }
            Rc::new(text)
        });
        RichTextView::new(text.clone(), animate)
    }

    fn reuse(&mut self, previous: Self) {
        if *self == previous {
            *self = previous;
        } else if let (
            NodeKind::Element(tag, children),
            NodeKind::Element(previous_tag, previous_children),
        ) = (&mut self.kind, previous.kind)
            && *tag == previous_tag
        {
            for (child, previous) in children.iter_mut().zip(previous_children) {
                child.reuse(previous);
            }
        }
    }
}

impl Markdown {
    pub fn update(&mut self, source: &str, animate: bool) {
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        let nodes = parse_nodes(&mut Parser::new_ext(source, options));
        let now = Instant::now();
        self.updated_at = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                if self.nodes.get(index) == Some(node) {
                    self.updated_at[index]
                } else {
                    animate.then_some(now)
                }
            })
            .collect();
        let mut previous = std::mem::take(&mut self.nodes).into_iter();
        self.nodes = nodes
            .into_iter()
            .map(|mut node| {
                if let Some(previous) = previous.next() {
                    node.reuse(previous);
                }
                node
            })
            .collect();
    }

    pub fn block_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn render_block(&self, index: usize, id: impl Into<ElementId>) -> Stateful<Div> {
        let animate =
            self.updated_at[index].is_some_and(|updated| updated.elapsed() < FADE_DURATION);
        div()
            .id(id)
            .min_w_0()
            .text_size(px(14.))
            .line_height(px(23.))
            .child(render_block(&self.nodes[index], animate))
    }
}

fn parse_nodes(events: &mut Parser<'_>) -> Vec<Node> {
    let mut nodes = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(tag) => {
                nodes.push(Node::new(NodeKind::Element(
                    tag.into_static(),
                    parse_nodes(events),
                )));
            }
            Event::End(_) => break,
            event => nodes.push(Node::new(NodeKind::Leaf(event.into_static()))),
        }
    }
    nodes
}

fn group_nodes(nodes: &[Node]) -> Vec<Group> {
    let mut groups = Vec::new();
    let mut inline = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let block = match &node.kind {
            NodeKind::Element(tag, _) => !matches!(
                tag,
                Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Link { .. }
                    | Tag::Image { .. }
            ),
            NodeKind::Leaf(event) => matches!(event, Event::Rule),
        };
        if block {
            if !inline.is_empty() {
                groups.push(Group::Inline(Rc::new(RichText::new(
                    inline.drain(..),
                    true,
                ))));
            }
            groups.push(Group::Block(index));
        } else {
            inline.push(node);
        }
    }
    if !inline.is_empty() {
        groups.push(Group::Inline(Rc::new(RichText::new(inline, true))));
    }
    groups
}

fn render_nodes(node: &Node, animate: bool) -> Div {
    let NodeKind::Element(_, children) = &node.kind else {
        return div();
    };
    div().min_w_0().flex().flex_col().gap_3().children(
        node.groups
            .get_or_init(|| group_nodes(children))
            .iter()
            .enumerate()
            .map(|(index, group)| {
                div().id(("block", index)).min_w_0().child(match group {
                    Group::Inline(text) => {
                        RichTextView::new(text.clone(), animate).into_any_element()
                    }
                    Group::Block(index) => render_block(&children[*index], animate),
                })
            }),
    )
}

fn render_block(node: &Node, animate: bool) -> AnyElement {
    let NodeKind::Element(tag, children) = &node.kind else {
        return if matches!(node.kind, NodeKind::Leaf(Event::Rule)) {
            div().h(px(1.)).my_2().bg(rgb(0x454550)).into_any_element()
        } else {
            div().child(node.rich_text(animate)).into_any_element()
        };
    };
    match tag {
        Tag::Paragraph => div().child(node.rich_text(animate)).into_any_element(),
        Tag::Heading { level, .. } => {
            let size = match *level as u8 {
                1 => 26.,
                2 => 22.,
                3 => 18.,
                _ => 15.,
            };
            div()
                .mt_2()
                .text_size(px(size))
                .line_height(px(size + 8.))
                .font_weight(FontWeight::BOLD)
                .child(node.rich_text(animate))
                .into_any_element()
        }
        Tag::BlockQuote(_) => div()
            .pl_4()
            .border_l_2()
            .border_color(rgb(0x656579))
            .text_color(rgb(0xb1b1c0))
            .child(render_nodes(node, animate))
            .into_any_element(),
        Tag::CodeBlock(kind) => {
            let language = match kind {
                CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
                CodeBlockKind::Indented => "",
            };
            let text = node.rich_text(animate);
            div()
                .min_w_0()
                .rounded_md()
                .bg(rgb(0x18181e))
                .when(!language.is_empty(), |view| {
                    view.child(
                        div()
                            .px_3()
                            .pt_2()
                            .text_xs()
                            .text_color(rgb(0x9797a5))
                            .child(language.to_owned()),
                    )
                })
                .child(
                    div()
                        .id("code")
                        .min_w_0()
                        .overflow_x_scroll()
                        .p_3()
                        .font_family("Menlo")
                        .text_size(px(13.))
                        .line_height(px(21.))
                        .whitespace_nowrap()
                        .child(text),
                )
                .into_any_element()
        }
        Tag::List(start) => div()
            .flex()
            .flex_col()
            .gap_2()
            .children(children.iter().enumerate().map(|(index, item)| {
                let marker = start.map_or_else(
                    || "•".to_owned(),
                    |start| format!("{}.", start.saturating_add(index as u64)),
                );
                div()
                    .id(("item", index))
                    .min_w_0()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .min_w(px(24.))
                            .flex_shrink_0()
                            .text_align(TextAlign::Right)
                            .text_color(rgb(0x9797a5))
                            .child(marker),
                    )
                    .child(div().flex_1().min_w_0().child(render_block(item, animate)))
            }))
            .into_any_element(),
        Tag::Table(alignments) => render_table(alignments, children, animate).into_any_element(),
        _ => render_nodes(node, animate).into_any_element(),
    }
}

fn render_table(alignments: &[Alignment], rows: &[Node], animate: bool) -> Stateful<Div> {
    div().id("table").min_w_0().overflow_x_scroll().child(
        div()
            .min_w(px(alignments.len() as f32 * 140.))
            .border_t_1()
            .border_l_1()
            .border_color(rgb(0x454550))
            .children(rows.iter().enumerate().filter_map(|(index, row)| {
                let NodeKind::Element(tag, cells) = &row.kind else {
                    return None;
                };
                let header = matches!(tag, Tag::TableHead);
                Some(
                    div()
                        .id(("row", index))
                        .flex()
                        .when(header, |view| {
                            view.bg(rgb(0x303039)).font_weight(FontWeight::BOLD)
                        })
                        .children(cells.iter().enumerate().map(|(index, cell)| {
                            let alignment = match alignments.get(index) {
                                Some(Alignment::Center) => TextAlign::Center,
                                Some(Alignment::Right) => TextAlign::Right,
                                _ => TextAlign::Left,
                            };
                            let NodeKind::Element(_, _) = &cell.kind else {
                                return div().id(("cell", index));
                            };
                            div()
                                .id(("cell", index))
                                .flex_1()
                                .min_w_0()
                                .px_3()
                                .py_2()
                                .border_r_1()
                                .border_b_1()
                                .border_color(rgb(0x454550))
                                .text_align(alignment)
                                .child(cell.rich_text(animate))
                        })),
                )
            })),
    )
}

#[derive(Clone, Copy, Default)]
struct InlineStyle {
    highlight: HighlightStyle,
    code: bool,
}

#[derive(Default)]
struct RichText {
    text: String,
    runs: Vec<(usize, InlineStyle)>,
    links: Vec<(Range<usize>, SharedString)>,
    shared_text: OnceCell<SharedString>,
    layout: RefCell<Option<TextCache>>,
}

impl RichText {
    fn new<'a>(nodes: impl IntoIterator<Item = &'a Node>, autolink: bool) -> Self {
        let mut text = Self::default();
        for node in nodes {
            text.append(node, InlineStyle::default(), None);
        }
        if autolink {
            text.linkify();
        }
        text
    }

    fn linkify(&mut self) {
        let mut protected: Vec<_> = self.links.iter().map(|(range, _)| range.clone()).collect();
        let mut offset = 0;
        for (len, style) in &self.runs {
            if style.code {
                protected.push(offset..offset + len);
            }
            offset += len;
        }
        let mut finder = LinkFinder::new();
        finder.url_must_have_scheme(false);
        for link in finder.links(&self.text) {
            let range = link.start()..link.end();
            if protected
                .iter()
                .any(|other| range.start < other.end && other.start < range.end)
            {
                continue;
            }
            let target = match link.kind() {
                LinkKind::Email => link_target(&format!("mailto:{}", link.as_str())),
                LinkKind::Url => link_target(link.as_str()),
                _ => None,
            };
            if let Some(target) = target {
                self.links.push((range, target));
            }
        }
        self.links.sort_by_key(|(range, _)| range.start);
        let mut links = self.links.iter().peekable();
        let mut offset = 0;
        let mut runs = Vec::new();
        for (len, style) in std::mem::take(&mut self.runs) {
            let end = offset + len;
            while offset < end {
                while links.peek().is_some_and(|(range, _)| range.end <= offset) {
                    links.next();
                }
                let mut linked_style = style;
                let stop = match links.peek() {
                    Some((range, _)) if range.start <= offset => {
                        linked_style.highlight.color = Some(rgb(0x9dbbff).into());
                        linked_style.highlight.underline = Some(UnderlineStyle {
                            thickness: px(1.),
                            ..Default::default()
                        });
                        end.min(range.end)
                    }
                    Some((range, _)) => end.min(range.start),
                    None => end,
                };
                runs.push((stop - offset, linked_style));
                offset = stop;
            }
        }
        self.runs = runs;
    }

    fn append(&mut self, node: &Node, mut style: InlineStyle, link: Option<&SharedString>) {
        match &node.kind {
            NodeKind::Element(tag, children) => {
                match tag {
                    Tag::Strong => style.highlight.font_weight = Some(FontWeight::BOLD),
                    Tag::Emphasis => style.highlight.font_style = Some(FontStyle::Italic),
                    Tag::Strikethrough => {
                        style.highlight.strikethrough = Some(StrikethroughStyle {
                            thickness: px(1.),
                            color: None,
                        });
                    }
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                        if let Some(url) = link_target(dest_url) {
                            style.highlight.color = Some(rgb(0x9dbbff).into());
                            style.highlight.underline = Some(UnderlineStyle {
                                thickness: px(1.),
                                ..Default::default()
                            });
                            for child in children {
                                self.append(child, style, Some(&url));
                            }
                            return;
                        }
                    }
                    _ => {}
                }
                for child in children {
                    self.append(child, style, link);
                }
            }
            NodeKind::Leaf(event) => {
                let text = match event {
                    Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                        text.as_ref()
                    }
                    Event::Code(text) => {
                        style.code = true;
                        style.highlight.color = Some(rgb(0xe0b5d1).into());
                        style.highlight.background_color = Some(rgb(0x303039).into());
                        text.as_ref()
                    }
                    Event::SoftBreak => " ",
                    Event::HardBreak => "\n",
                    Event::TaskListMarker(true) => "☑ ",
                    Event::TaskListMarker(false) => "☐ ",
                    _ => return,
                };
                if text.is_empty() {
                    return;
                }
                let start = self.text.len();
                self.text.push_str(text);
                self.runs.push((text.len(), style));
                if let Some(url) = link {
                    self.links.push((start..self.text.len(), url.clone()));
                }
            }
        }
    }

    fn shared_text(&self) -> SharedString {
        self.shared_text
            .get_or_init(|| self.text.clone().into())
            .clone()
    }

    fn text_runs(&self, style: &TextStyle, fade: &TextFade, now: Instant) -> Vec<TextRun> {
        let mut runs = Vec::with_capacity(self.runs.len());
        let mut offset = 0;
        for (len, inline) in &self.runs {
            let mut style = style.clone().highlight(inline.highlight);
            if inline.code {
                style.font_family = "Menlo".into();
            }
            fade.append_runs(style.to_run(*len), offset, now, &mut runs);
            offset += len;
        }
        runs
    }
}

fn link_target(text: &str) -> Option<SharedString> {
    let text = if text.starts_with("www.") {
        format!("https://{text}")
    } else {
        text.to_owned()
    };
    let url = reqwest::Url::parse(&text).ok()?;
    matches!(url.scheme(), "http" | "https" | "mailto").then(|| url.to_string().into())
}

#[derive(IntoElement)]
struct RichTextView {
    text: Rc<RichText>,
    animate: bool,
}

impl RichTextView {
    fn new(text: Rc<RichText>, animate: bool) -> Self {
        Self { text, animate }
    }
}

#[derive(Clone, Default)]
struct TextFade {
    text: SharedString,
    spans: Vec<(Range<usize>, Instant)>,
}

impl TextFade {
    fn update(&mut self, text: SharedString, animate: bool, now: Instant) {
        let prefix = if text.starts_with(&**self.text) {
            self.text.len()
        } else {
            self.text
                .chars()
                .zip(text.chars())
                .take_while(|(old, new)| old == new)
                .map(|(character, _)| character.len_utf8())
                .sum::<usize>()
        };
        self.spans.retain_mut(|(range, started)| {
            range.end = range.end.min(prefix);
            range.start < range.end && now.duration_since(*started) < FADE_DURATION
        });
        if animate && prefix < text.len() {
            self.spans.push((prefix..text.len(), now));
        }
        self.text = text;
    }

    fn append_runs(&self, run: TextRun, start: usize, now: Instant, output: &mut Vec<TextRun>) {
        let end = start + run.len;
        let mut cursor = start;
        for (range, started) in &self.spans {
            let start = range.start.max(cursor);
            let stop = range.end.min(end);
            if start >= stop {
                continue;
            }
            if cursor < start {
                output.push(TextRun {
                    len: start - cursor,
                    ..run.clone()
                });
            }
            let progress =
                (now.duration_since(*started).as_secs_f32() / FADE_DURATION.as_secs_f32()).min(1.);
            let opacity = progress * progress * (3. - 2. * progress);
            output.push(TextRun {
                len: stop - start,
                color: run.color.opacity(opacity),
                background_color: run.background_color.map(|color| color.opacity(opacity)),
                ..run.clone()
            });
            cursor = stop;
        }
        if cursor < end {
            output.push(TextRun {
                len: end - cursor,
                ..run
            });
        }
    }
}

impl RenderOnce for RichTextView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let now = Instant::now();
        let text = self.text.shared_text();
        let state = window.use_keyed_state("text-fade", cx, |_, _| TextFade::default());
        let fade = state.update(cx, |fade, _| {
            fade.update(text.clone(), self.animate, now);
            fade.clone()
        });
        if !fade.spans.is_empty() {
            window.request_animation_frame();
        }
        let default_style = window.text_style();
        if fade.spans.is_empty() && self.text.links.is_empty() {
            return CachedText(self.text).into_any_element();
        }
        let runs = self.text.text_runs(&default_style, &fade, now);
        let text = StyledText::new(text).with_runs(runs);
        if self.text.links.is_empty() {
            return text.into_any_element();
        }
        let (ranges, urls): (Vec<_>, Vec<_>) = self.text.links.iter().cloned().unzip();
        InteractiveText::new("text", text)
            .on_click(ranges, move |index, _, cx| {
                cx.open_url(&urls[index]);
            })
            .into_any_element()
    }
}

#[derive(Clone)]
pub struct CachedText(Rc<RichText>);

impl CachedText {
    pub fn new(text: String) -> Self {
        let len = text.len();
        Self(Rc::new(RichText {
            text,
            runs: vec![(len, InlineStyle::default())],
            ..Default::default()
        }))
    }
}

struct TextCache {
    style: TextStyle,
    rem_size: Pixels,
    runs: Vec<TextRun>,
    element: StyledText,
}

impl IntoElement for CachedText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for CachedText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = window.text_style();
        let mut cache = self.0.layout.borrow_mut();
        if cache
            .as_ref()
            .is_none_or(|cache| cache.style != style || cache.rem_size != window.rem_size())
        {
            let runs = self
                .0
                .text_runs(&style, &TextFade::default(), Instant::now());
            *cache = Some(TextCache {
                style,
                rem_size: window.rem_size(),
                runs,
                element: StyledText::new(self.0.shared_text()),
            });
        }
        let cache = cache.as_mut().unwrap();
        // StyledText consumes its runs during layout. Restore them for width changes
        // while keeping the shaped text in its TextLayout.
        let element = std::mem::replace(&mut cache.element, StyledText::new(""));
        cache.element = element.with_runs(cache.runs.clone());
        cache.element.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0
            .layout
            .borrow_mut()
            .as_mut()
            .unwrap()
            .element
            .prepaint(id, inspector_id, bounds, state, window, cx);
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.layout.borrow_mut().as_mut().unwrap().element.paint(
            id,
            inspector_id,
            bounds,
            state,
            prepaint,
            window,
            cx,
        );
    }
}
