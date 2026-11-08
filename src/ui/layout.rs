use super::*;
use space::store::{MAX_COMPOSER_HEIGHT, MIN_COMPOSER_HEIGHT, MIN_SIDEBAR_WIDTH, UiSettings};

pub(super) const MIN_CHAT_WIDTH: f32 = 320.;
pub(super) const MIN_INPUT_WIDTH: f32 = 320.;
pub(super) const MIN_WINDOW_WIDTH: f32 = MIN_SIDEBAR_WIDTH + MIN_CHAT_WIDTH + MIN_INPUT_WIDTH;

fn clamped_chat_fraction(fraction: f32, width: f32) -> f32 {
    let width = width.max(MIN_CHAT_WIDTH + MIN_INPUT_WIDTH);
    let minimum = (MIN_CHAT_WIDTH / width).max(0.1);
    let maximum = (1. - MIN_INPUT_WIDTH / width).min(0.9);
    fraction.clamp(minimum, maximum)
}

pub(super) struct SidebarResize;
pub(super) struct SplitResize;

#[derive(Clone, Copy)]
struct ComposerResize {
    pointer_y: Pixels,
    height: Pixels,
}

pub(super) struct LayoutState {
    pub(super) sidebar_width: Pixels,
    pub(super) composer_height: Pixels,
    pub(super) composer_expanded: bool,
    pub(super) composer_split: bool,
    pub(super) chat_fraction: f32,
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
            composer_expanded: settings.composer_expanded,
            composer_split: settings.composer_split,
            chat_fraction: settings.chat_fraction,
            composer_resize: None,
            dirty: false,
            error: None,
        }
    }
}

impl LayoutState {
    pub(super) fn apply_settings(&mut self, settings: UiSettings) {
        if self.dirty {
            return;
        }
        self.sidebar_width = px(settings.sidebar_width);
        self.composer_height = px(settings.composer_height);
        self.composer_split = settings.composer_split;
        self.composer_expanded = settings.composer_expanded;
        self.chat_fraction = settings.chat_fraction;
    }

    pub(super) fn settings(&self) -> UiSettings {
        UiSettings {
            sidebar_width: self.sidebar_width.into(),
            composer_height: self.composer_height.into(),
            composer_split: self.composer_split,
            composer_expanded: self.composer_expanded,
            chat_fraction: self.chat_fraction,
        }
    }
}

impl Workspace {
    pub(super) fn conversation_min_width(&self) -> Pixels {
        let split = self.layout.composer_split
            && !self.layout.composer_expanded
            && self.file.is_none()
            && self.expanded_diagram.is_none();
        px(if split {
            MIN_CHAT_WIDTH + MIN_INPUT_WIDTH
        } else {
            MIN_CHAT_WIDTH.max(MIN_INPUT_WIDTH)
        })
    }

    pub(super) fn split_chat_width(&self, window: &Window) -> Pixels {
        let width = f32::from(window.viewport_size().width - self.sidebar_width(window))
            .max(MIN_CHAT_WIDTH + MIN_INPUT_WIDTH);
        px(width * clamped_chat_fraction(self.layout.chat_fraction, width))
    }

    pub(super) fn resize_split(
        &mut self,
        event: &DragMoveEvent<SplitResize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = f32::from(event.bounds.size.width);
        if width <= 0. {
            return;
        }
        let fraction = clamped_chat_fraction(
            f32::from(event.event.position.x - event.bounds.left()) / width,
            width,
        );
        if self.layout.chat_fraction != fraction {
            self.layout.chat_fraction = fraction;
            self.layout.dirty = true;
            cx.notify();
        }
        cx.stop_propagation();
    }

    pub(super) fn split_resize_handle(&self, window: &Window) -> Stateful<Div> {
        div()
            .id("split-resize")
            .absolute()
            .top_0()
            .bottom_0()
            .left(self.split_chat_width(window))
            .ml(px(-3.))
            .w(px(6.))
            .cursor(CursorStyle::ResizeLeftRight)
            .hover(|style| style.bg(rgba(0x9797a550)))
            .on_drag(SplitResize, |_, _, _, cx| cx.new(|_| Empty))
    }

    pub(super) fn resize_sidebar(
        &mut self,
        event: &DragMoveEvent<SidebarResize>,
        max_width: Pixels,
        cx: &mut Context<Self>,
    ) {
        let width =
            (event.event.position.x - event.bounds.left()).clamp(px(MIN_SIDEBAR_WIDTH), max_width);
        if self.layout.sidebar_width != width {
            self.layout.sidebar_width = width;
            self.layout.dirty = true;
            cx.notify();
        }
        cx.stop_propagation();
    }

    pub(super) fn composer_max_height(window: &Window) -> Pixels {
        (window.viewport_size().height - px(200.))
            .clamp(px(MIN_COMPOSER_HEIGHT), px(MAX_COMPOSER_HEIGHT))
    }

    pub(super) fn composer_min_height(&self, window: &Window) -> Pixels {
        let height = if self.message_edit.is_some() {
            180.
        } else {
            MIN_COMPOSER_HEIGHT
        };
        px(height).min(Self::composer_max_height(window))
    }

    pub(super) fn toggle_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout.composer_expanded = !self.layout.composer_expanded;
        self.layout.composer_resize = None;
        self.layout.dirty = true;
        self.save_layout_settings(cx);
        self.input.read(cx).focus_handle(cx).focus(window);
        cx.notify();
    }

    pub(super) fn toggle_composer_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout.composer_split = !self.layout.composer_split;
        self.layout.composer_expanded = false;
        self.layout.composer_resize = None;
        self.layout.dirty = true;
        self.save_layout_settings(cx);
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
        let height = (resize.height + resize.pointer_y - event.position.y).clamp(
            self.composer_min_height(window),
            Self::composer_max_height(window),
        );
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
        self.save_layout_settings(cx);
    }

    fn save_layout_settings(&mut self, cx: &mut Context<Self>) {
        if self.layout.dirty {
            match self
                .sidebar_commands
                .try_send(SidebarCommand::SaveSettings(self.layout.settings()))
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

    pub(super) fn composer_resize_handle(&self, cx: &Context<Self>) -> Stateful<Div> {
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
                            this.composer_min_height(window),
                            Self::composer_max_height(window),
                        ),
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }
}
