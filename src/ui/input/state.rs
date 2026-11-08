use super::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub(in crate::ui) struct InputState {
    text: String,
    revision: u64,
    selection: Range<usize>,
    reversed: bool,
    scroll: f32,
    scroll_x: f32,
    vim: Vim,
}

impl InputState {
    pub(in crate::ui) fn revision(&self) -> u64 {
        self.revision
    }
}

impl Input {
    pub(in crate::ui) fn saved_state(&self) -> InputState {
        InputState {
            text: self.text.clone(),
            revision: self.revision,
            selection: self.selection.clone(),
            reversed: self.reversed,
            scroll: self.scroll.into(),
            scroll_x: self.scroll_x.into(),
            vim: self.vim.clone(),
        }
    }

    pub(in crate::ui) fn restore_state(
        &mut self,
        state: InputState,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        ensure!(
            state.selection.start <= state.selection.end
                && state.text.is_char_boundary(state.selection.start)
                && state.text.is_char_boundary(state.selection.end),
            "Invalid saved input selection"
        );
        ensure!(
            state.scroll.is_finite() && state.scroll_x.is_finite(),
            "Invalid saved input scroll position"
        );
        state.vim.validate(&state.text)?;
        self.clear(cx);
        self.text = state.text;
        self.revision = state.revision;
        self.selection = state.selection;
        self.reversed = state.reversed;
        self.scroll = px(state.scroll.max(0.));
        self.scroll_x = px(state.scroll_x.max(0.));
        self.vim = state.vim;
        self.reveal_cursor = false;
        cx.notify();
        Ok(())
    }
}
