use super::*;
use space::store::{MAX_COMPOSER_HEIGHT, MIN_COMPOSER_HEIGHT, UiSettings};

pub(super) struct SidebarResize;

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

impl LayoutState {
    pub(super) fn apply_settings(&mut self, settings: UiSettings) {
        if self.dirty {
            return;
        }
        self.sidebar_width = px(settings.sidebar_width);
        self.composer_height = px(settings.composer_height);
    }

    pub(super) fn settings(&self) -> UiSettings {
        UiSettings {
            sidebar_width: self.sidebar_width.into(),
            composer_height: self.composer_height.into(),
        }
    }
}

impl Workspace {
    pub(super) fn resize_sidebar(
        &mut self,
        event: &DragMoveEvent<SidebarResize>,
        max_width: Pixels,
        cx: &mut Context<Self>,
    ) {
        let width = (event.event.position.x - event.bounds.left()).clamp(px(180.), max_width);
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
                        height: this
                            .layout
                            .composer_height
                            .clamp(px(MIN_COMPOSER_HEIGHT), Self::composer_max_height(window)),
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }
}
