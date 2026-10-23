use super::*;
use space::store::{MAX_COMPOSER_HEIGHT, MIN_COMPOSER_HEIGHT, UiSettings};

#[derive(Clone, Copy)]
struct ComposerResize {
    pointer_y: Pixels,
    height: Pixels,
}

pub(super) struct LayoutState {
    pub(super) sidebar_width: Pixels,
    pub(super) composer_height: Pixels,
    pub(super) composer_expanded: bool,
    composer_resize: Option<ComposerResize>,
    pub(super) dirty: bool,
    pub(super) error: Option<String>,
}

impl Default for LayoutState {
    fn default() -> Self {
        let settings = UiSettings::default();
        Self {
            sidebar_width: px(settings.sidebar_width),
            composer_height: px(settings.composer_height),
            composer_expanded: false,
            composer_resize: None,
            dirty: false,
            error: None,
        }
    }
}

impl Workspace {
    pub(super) fn send(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        if self.file.is_some()
            || self.running()
            || self.waiting
            || self.rename.is_some()
            || self.chat_menu.is_some()
            || self.model_menu.is_some()
            || self.input.read(cx).is_composing()
        {
            return;
        }
        let text = self.input.read(cx).text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Some(chat) = &self.chat else {
            return;
        };
        if chat.archived {
            return;
        }
        let chat_id = chat.id;
        if self
            .commands
            .try_send(Command::Send { chat_id, text })
            .is_err()
        {
            self.error = Some("The agent worker is unavailable".into());
        } else {
            self.waiting = true;
            self.error = None;
            self.input.update(cx, |input, cx| input.clear(cx));
            if self.layout.composer_expanded {
                self.toggle_composer(window, cx);
            }
            self.follow_latest();
        }
        cx.notify();
    }

    pub(super) fn settings(&self) -> UiSettings {
        UiSettings {
            sidebar_width: self.layout.sidebar_width.into(),
            composer_height: self.layout.composer_height.into(),
        }
    }

    pub(super) fn composer_max_height(window: &Window) -> Pixels {
        (window.viewport_size().height - px(200.))
            .clamp(px(MIN_COMPOSER_HEIGHT), px(MAX_COMPOSER_HEIGHT))
    }

    pub(super) fn toggle_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout.composer_expanded = !self.layout.composer_expanded;
        self.input.read(cx).focus_handle(cx).focus(window);
        cx.notify();
    }

    pub(super) fn resize_composer(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resize) = self.layout.composer_resize else {
            return;
        };
        let height = (resize.height + resize.pointer_y - event.position.y)
            .clamp(px(MIN_COMPOSER_HEIGHT), Self::composer_max_height(window));
        if height != self.layout.composer_height {
            self.layout.composer_height = height;
            self.layout.dirty = true;
            cx.notify();
        }
    }

    pub(super) fn finish_resize(
        &mut self,
        event: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        self.layout.composer_resize = None;
        if self.layout.dirty {
            match self
                .sidebar_commands
                .try_send(SidebarCommand::SaveSettings(self.settings()))
            {
                Ok(()) => {
                    self.layout.dirty = false;
                    self.layout.error = None;
                }
                Err(_) => {
                    self.layout.error =
                        Some("Cannot save the layout settings: the worker is unavailable".into())
                }
            }
            cx.notify();
        }
    }

    pub(super) fn composer(&self, window: &Window, cx: &Context<Self>) -> Stateful<Div> {
        let expanded = self.layout.composer_expanded;
        let height = self
            .layout
            .composer_height
            .clamp(px(MIN_COMPOSER_HEIGHT), Self::composer_max_height(window));
        div()
            .id("composer")
            .relative()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x454550))
            .bg(rgb(0x27272e))
            .mx_5()
            .mb_3()
            .when(expanded, |view| view.flex_1().mt_3())
            .when(!expanded, |view| view.h(height).flex_shrink_0())
            .when(!expanded, |view| {
                view.child(
                    div()
                        .id("composer-resize")
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(8.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_t_lg()
                        .border_t_1()
                        .border_color(transparent_black())
                        .cursor(CursorStyle::ResizeUpDown)
                        .hover(|style| style.border_color(rgb(0x626270)))
                        .child(div().w(px(32.)).h(px(2.)).rounded_full().bg(rgb(0x626270)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                this.layout.composer_resize = Some(ComposerResize {
                                    pointer_y: event.position.y,
                                    height: this.layout.composer_height.clamp(
                                        px(MIN_COMPOSER_HEIGHT),
                                        Self::composer_max_height(window),
                                    ),
                                });
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
            })
            .child(
                div()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .p_2()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .w_full()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .flex()
                            .items_start()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .overflow_hidden()
                                    .child(self.input.clone()),
                            )
                            .child(
                                div()
                                    .id("expand-composer")
                                    .size(px(22.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(0x383842)))
                                    .child(composer_icon(expanded))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_composer(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_shrink_0()
                            .gap_2()
                            .justify_between()
                            .items_end()
                            .mt_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .line_height(px(16.))
                                    .text_color(rgb(0x91919f))
                                    .child(
                                        if self.chat.as_ref().is_some_and(|chat| chat.archived) {
                                            "Unarchive this chat to send a message"
                                        } else if self.running() {
                                            ""
                                        } else if self.waiting {
                                            "Loading…"
                                        } else {
                                            "Enter to send · Shift+Enter for a new line"
                                        },
                                    ),
                            )
                            .child(self.model_selector(cx))
                            .child(if self.running() {
                                button(
                                    "stop",
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(spinner("response-spinner"))
                                        .child("Stop"),
                                )
                                .flex_shrink_0()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.stop();
                                    cx.notify();
                                }))
                                .into_any_element()
                            } else {
                                button("send", "Send")
                                    .flex_shrink_0()
                                    .when(
                                        self.chat.as_ref().is_some_and(|chat| chat.archived),
                                        |button| button.opacity(0.4).cursor_default(),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.send(&SendMessage, window, cx)
                                    }))
                                    .into_any_element()
                            }),
                    ),
            )
    }
}

fn composer_icon(expanded: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let (tip, tail, corner) = if expanded {
                (1.5, 5., 5.)
            } else {
                (5., 0., 1.5)
            };
            let mut path = PathBuilder::stroke(px(1.5));
            for sign in [-1., 1.] {
                path.move_to(center + point(px(tail * sign), px(-tail * sign)));
                path.line_to(center + point(px(tip * sign), px(-tip * sign)));
                path.move_to(center + point(px(corner * sign), px(-tip * sign)));
                path.line_to(center + point(px(tip * sign), px(-tip * sign)));
                path.line_to(center + point(px(tip * sign), px(-corner * sign)));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(0x9797a5));
            }
        },
    )
    .size(px(14.))
}
