use super::{FADE_DURATION, Node, NodeKind};
use gpui::{prelude::*, *};
use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Event, Tag};
use std::{
    cell::{OnceCell, RefCell},
    ops::Range,
    rc::Rc,
    time::Instant,
};

#[derive(Clone, Copy, Default)]
pub(super) struct InlineStyle {
    pub(super) highlight: HighlightStyle,
    pub(super) code: bool,
}

#[derive(Default)]
pub(super) struct RichText {
    pub(super) text: String,
    pub(super) runs: Vec<(usize, InlineStyle)>,
    links: Vec<(Range<usize>, SharedString)>,
    shared_text: OnceCell<SharedString>,
    layout: RefCell<Option<TextCache>>,
}

impl RichText {
    pub(super) fn new<'a>(nodes: impl IntoIterator<Item = &'a Node>, autolink: bool) -> Self {
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
        self.find_links();
        self.style_links();
    }

    fn find_links(&mut self) {
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
    }

    fn style_links(&mut self) {
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
pub(super) struct RichTextView {
    text: Rc<RichText>,
    animate: bool,
}

impl RichTextView {
    pub(super) fn new(text: Rc<RichText>, animate: bool) -> Self {
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
