mod text;

pub use text::CachedText;
use text::{InlineStyle, RichText, RichTextView};

use std::{
    cell::OnceCell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{prelude::*, *};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag};

use super::{icons::Icon, syntax};

const FADE_DURATION: Duration = Duration::from_millis(220);

#[derive(Default)]
pub struct Markdown {
    blocks: Vec<Block>,
}

struct Block {
    node: Node,
    updated_at: Option<Instant>,
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
                && let Some(runs) = syntax::highlight(&text.text, "py")
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
        let mut previous = std::mem::take(&mut self.blocks).into_iter();
        self.blocks = nodes
            .into_iter()
            .map(|mut node| {
                let mut updated_at = animate.then_some(now);
                if let Some(previous) = previous.next() {
                    if node == previous.node {
                        updated_at = previous.updated_at;
                    }
                    node.reuse(previous.node);
                }
                Block { node, updated_at }
            })
            .collect();
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn render_block(&self, index: usize, id: impl Into<ElementId>) -> Stateful<Div> {
        let block = &self.blocks[index];
        let animate = block
            .updated_at
            .is_some_and(|updated| updated.elapsed() < FADE_DURATION);
        div()
            .id(id)
            .min_w_0()
            .text_size(px(14.))
            .line_height(px(23.))
            .child(render_block(&block.node, animate))
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
                            .child(match start {
                                Some(start) => div()
                                    .child(format!("{}.", start.saturating_add(index as u64)))
                                    .into_any_element(),
                                None => div()
                                    .h(px(23.))
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .child(Icon::Bullet.render(14., 0x9797a5))
                                    .into_any_element(),
                            }),
                    )
                    .child(div().flex_1().min_w_0().child(render_block(item, animate)))
            }))
            .into_any_element(),
        Tag::Item => {
            let Some(checked) = task_state(node) else {
                return render_nodes(node, animate).into_any_element();
            };
            div()
                .min_w_0()
                .flex()
                .gap_2()
                .child(
                    div()
                        .h(px(23.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .child(Icon::Checkbox(checked).render(14., 0x9797a5)),
                )
                .child(div().flex_1().min_w_0().child(render_nodes(node, animate)))
                .into_any_element()
        }
        Tag::Table(alignments) => render_table(alignments, children, animate).into_any_element(),
        _ => render_nodes(node, animate).into_any_element(),
    }
}

fn task_state(node: &Node) -> Option<bool> {
    match &node.kind {
        NodeKind::Leaf(Event::TaskListMarker(checked)) => Some(*checked),
        NodeKind::Element(Tag::Item | Tag::Paragraph, children) => {
            children.first().and_then(task_state)
        }
        _ => None,
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
