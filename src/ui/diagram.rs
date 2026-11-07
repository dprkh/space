use std::sync::Arc;

use super::*;

#[derive(Clone, PartialEq, Action)]
#[action(namespace = space_ui, no_json)]
pub(super) struct ExpandDiagram(pub(super) Arc<RenderImage>);

pub(super) struct ExpandedDiagram {
    image: Arc<RenderImage>,
    focus: FocusHandle,
    window: AnyWindowHandle,
}

impl Focusable for ExpandedDiagram {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub(super) fn expand_diagram(
        &mut self,
        action: &ExpandDiagram,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_open || self.rename.is_some() || self.file.is_some() {
            return;
        }
        self.close_diagram(cx);
        self.chat_menu = None;
        self.model_menu = None;
        let focus = cx.focus_handle();
        focus.focus(window);
        self.expanded_diagram = Some(ExpandedDiagram {
            image: action.0.clone(),
            focus,
            window: window.window_handle(),
        });
        cx.notify();
    }

    pub(super) fn close_diagram(&mut self, cx: &mut Context<Self>) {
        let Some(diagram) = self.expanded_diagram.take() else {
            return;
        };
        let input_focus = self.input.read(cx).focus_handle(cx);
        cx.defer(move |cx| {
            let _ = diagram.window.update(cx, |_, window, cx| {
                cx.drop_image(diagram.image, Some(window));
                window.refresh();
                if diagram.focus.is_focused(window) {
                    input_focus.focus(window);
                }
            });
        });
        cx.notify();
    }

    pub(super) fn diagram_panel(&self, diagram: &ExpandedDiagram, cx: &Context<Self>) -> Div {
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .when_some(self.archived_banner(cx), |view, banner| view.child(banner))
            .child(
                div()
                    .id("expanded-diagram")
                    .relative()
                    .track_focus(&diagram.focus)
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .mx_5()
                    .mt_3()
                    .mb_3()
                    .p_2()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x454550))
                    .bg(rgb(0x18181e))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                img(diagram.image.clone())
                                    .absolute()
                                    .size_full()
                                    .object_fit(ObjectFit::Contain),
                            ),
                    )
                    .child(
                        expansion_button("reduce-diagram", true)
                            .absolute()
                            .top_2()
                            .right_2()
                            .bg(rgb(0x18181e))
                            .on_click(cx.listener(|this, _, _, cx| this.close_diagram(cx))),
                    ),
            )
            .child(self.usage_footer())
    }
}
