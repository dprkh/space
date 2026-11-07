use std::{
    rc::Rc,
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::{Result, ensure};
use gpui::{prelude::*, *};
use mermaid_rs_renderer::{
    LayoutConfig, Theme, compute_layout, measure_svg_dimensions, parse_mermaid_strict,
    render::render_svg_with_dimensions,
};
use resvg::{tiny_skia, usvg};

use super::super::{diagram::ExpandDiagram, expansion_button, spinner};
use super::{RichText, RichTextView, render_code_block};

const IMAGE_SCALE: f32 = 2.;
const PREVIEW_MAX_WIDTH: f32 = 720.;
const PREVIEW_MAX_HEIGHT: f32 = 360.;
const RENDER_DELAY: Duration = Duration::from_millis(200);
const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_IMAGE_DIMENSION: f32 = 8192.;
const MAX_IMAGE_PIXELS: f32 = 16_000_000.;

pub(super) struct Diagram {
    text: Rc<RichText>,
    image: Option<Result<Arc<RenderImage>>>,
    _render: Task<()>,
}

impl Diagram {
    pub(super) fn new(text: Rc<RichText>, cx: &mut Context<Self>) -> Self {
        let source = text.text.clone();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |view, cx| {
            // Cancel pending work when streamed source changes before this delay ends.
            Timer::after(RENDER_DELAY).await;
            let image = background
                .spawn(async move { render_diagram(&source) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.image = Some(image);
                cx.notify();
            });
        });
        cx.on_release(|view, cx| {
            if let Some(Ok(image)) = &view.image {
                cx.drop_image(image.clone(), None);
            }
        })
        .detach();
        Self {
            text,
            image: None,
            _render: task,
        }
    }
}

impl Render for Diagram {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if let Some(Err(_)) = &self.image {
            return div()
                .min_w_0()
                .child(
                    div()
                        .mb_2()
                        .text_xs()
                        .text_color(rgb(0x9797a5))
                        .child("Cannot display this Mermaid diagram. The source is shown below."),
                )
                .child(render_code_block(
                    "mermaid",
                    RichTextView::new(self.text.clone(), false),
                ))
                .into_any_element();
        }
        let image = self.image.as_ref().and_then(|result| result.as_ref().ok());
        let size = image
            .map(|image| image.size(0).map(|value| px(value.0 as f32 / IMAGE_SCALE)))
            .unwrap_or_else(|| size(px(PREVIEW_MAX_WIDTH), px(PREVIEW_MAX_HEIGHT)));
        let preview_scale = (px(PREVIEW_MAX_WIDTH) / size.width)
            .min(px(PREVIEW_MAX_HEIGHT) / size.height)
            .min(1.);
        let display_size = size.map(|value| value * preview_scale);
        let panel = div()
            .id("mermaid")
            .relative()
            .min_w_0()
            .max_w_full()
            .flex()
            .flex_col()
            .rounded_md()
            .bg(rgb(0x18181e))
            .p_3()
            .child(
                div()
                    .id("diagram-image")
                    .min_w_0()
                    .max_w_full()
                    .w(display_size.width)
                    .h(display_size.height)
                    .overflow_x_scroll()
                    .child(match image {
                        Some(image) => img(image.clone())
                            .w(display_size.width)
                            .h(display_size.height)
                            .flex_shrink_0()
                            .into_any_element(),
                        None => div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(spinner("mermaid-loading"))
                            .into_any_element(),
                    }),
            )
            .when_some(image.cloned(), |view, expanded_image| {
                view.child(
                    expansion_button("expand-diagram", false)
                        .absolute()
                        .top_2()
                        .right_2()
                        .bg(rgb(0x18181e))
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(
                                Box::new(ExpandDiagram(expanded_image.clone())),
                                cx,
                            );
                        }),
                )
            });
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .child(panel)
            .into_any_element()
    }
}

fn render_diagram(source: &str) -> Result<Arc<RenderImage>> {
    ensure!(
        source.len() <= MAX_SOURCE_BYTES,
        "Diagram source is too large"
    );
    let parsed = parse_mermaid_strict(source)?;
    let theme = Theme {
        background: "#18181e".into(),
        font_family: "Arial, sans-serif".into(),
        ..Theme::dark()
    };
    let config = LayoutConfig::default();
    let layout = compute_layout(&parsed.graph, &theme, &config);
    let dimensions = measure_svg_dimensions(&layout, &config, None);
    let width = dimensions.width * IMAGE_SCALE;
    let height = dimensions.height * IMAGE_SCALE;
    ensure!(
        width.is_finite()
            && height.is_finite()
            && width >= 1.
            && height >= 1.
            && width <= MAX_IMAGE_DIMENSION
            && height <= MAX_IMAGE_DIMENSION
            && width * height <= MAX_IMAGE_PIXELS,
        "Diagram image is too large"
    );
    let svg = render_svg_with_dimensions(&layout, &theme, &config, Some((width, height)));
    rasterize(&svg)
}

fn rasterize(svg: &str) -> Result<Arc<RenderImage>> {
    static OPTIONS: LazyLock<usvg::Options<'static>> = LazyLock::new(|| {
        let mut options = usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        options.image_href_resolver.resolve_string = Box::new(|_, _| None);
        options
    });
    let tree = usvg::Tree::from_str(svg, &OPTIONS)?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| anyhow::anyhow!("Cannot allocate the diagram image"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let pixels = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.blue(), color.green(), color.red(), color.alpha()]
        })
        .collect();
    let buffer = image::RgbaImage::from_raw(size.width(), size.height(), pixels)
        .ok_or_else(|| anyhow::anyhow!("Invalid diagram image size"))?;
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}
