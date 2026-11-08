use super::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use space::pdf::Pdf;
use std::path::PathBuf;

#[derive(Deserialize, Serialize)]
struct WorkspaceView {
    version: u32,
    sidebar_visible: bool,
    sidebar_files: bool,
    sidebar_archived: bool,
    expanded_directories: Vec<PathBuf>,
    file: Option<file::SavedFile>,
}

#[derive(Deserialize, Serialize)]
struct ChatView {
    version: u32,
    branch: Option<Uuid>,
    input: input::InputState,
    pdfs: Vec<Pdf>,
    edit: Option<messages::SavedMessageEdit>,
    transcript: transcript::SavedTranscript,
    #[serde(default)]
    submission: Option<composer::Submission>,
}

impl Workspace {
    pub(super) fn save_chat_view(&self, cx: &App) -> Result<()> {
        let Some(chat) = &self.chat else {
            return Ok(());
        };
        if self.view_loaded != Some(chat.id) {
            return Ok(());
        }
        let state = ChatView {
            version: 1,
            branch: chat.active_branch(),
            input: self.input.read(cx).saved_state(),
            pdfs: self.attachments.pdfs.clone(),
            edit: self.message_edit.as_ref().map(|edit| edit.saved_state(cx)),
            transcript: self.transcript.saved_state(chat),
            submission: self.submission.clone(),
        };
        let json = serde_json::to_string(&state)?;
        self.sidebar_commands
            .try_send(SidebarCommand::SaveChatView {
                chat_id: chat.id,
                json,
            })?;
        Ok(())
    }

    pub(super) fn save_view(&self, cx: &App) -> Result<()> {
        let workspace_result = self.save_workspace_view(cx);
        self.save_chat_view(cx)?;
        workspace_result?;
        if !self.workspace_view_loaded {
            return Ok(());
        }
        self.sidebar_commands
            .try_send(SidebarCommand::SaveSettings(self.layout.settings()))?;
        Ok(())
    }

    pub(super) fn save_workspace_view(&self, cx: &App) -> Result<()> {
        if !self.workspace_view_loaded {
            return Ok(());
        }
        let state = WorkspaceView {
            version: 1,
            sidebar_visible: self.sidebar.visible,
            sidebar_files: self.sidebar.files,
            sidebar_archived: self.sidebar.archived,
            expanded_directories: self.sidebar.tree.read(cx).expanded_directories(),
            file: self.file.as_ref().map(|file| file.read(cx).saved_state()),
        };
        self.sidebar_commands
            .try_send(SidebarCommand::SaveWorkspaceView(serde_json::to_string(
                &state,
            )?))?;
        Ok(())
    }

    pub(super) fn restore_workspace_view(
        &mut self,
        result: Result<Option<String>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = result.map_err(anyhow::Error::msg).and_then(|json| {
            let Some(json) = json else {
                return Ok(());
            };
            let state: WorkspaceView = serde_json::from_str(&json)?;
            ensure!(state.version == 1, "Unsupported workspace view version");
            if !self.sidebar.visibility_changed {
                self.sidebar.visible = state.sidebar_visible;
            }
            self.sidebar.files = state.sidebar_files;
            self.sidebar.archived = state.sidebar_archived;
            self.sidebar.tree.update(cx, |tree, cx| {
                tree.restore_expanded(state.expanded_directories, cx);
                tree.set_active(self.sidebar.visible && self.sidebar.files, cx);
            });
            if let Some(file) = state.file {
                self.open_file(file.path.clone(), window, cx);
                if let Some(view) = &self.file {
                    view.update(cx, |view, cx| view.restore_scroll(file, cx));
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                self.workspace_view_loaded = true;
                if self.sidebar.visibility_changed
                    && let Err(error) = self.save_workspace_view(cx)
                {
                    self.layout.error = Some(format!("Cannot save the workspace view: {error:#}"));
                }
            }
            Err(error) => {
                self.layout.error = Some(format!("Cannot restore workspace view: {error:#}"))
            }
        }
    }

    pub(super) fn restore_chat_view(
        &mut self,
        chat_id: Uuid,
        result: Result<Option<String>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_view != Some(chat_id)
            || self.chat.as_ref().is_none_or(|chat| chat.id != chat_id)
        {
            return;
        }
        let result = result.map_err(anyhow::Error::msg).and_then(|json| {
            let Some(json) = json else {
                return Ok(());
            };
            let state: ChatView = serde_json::from_str(&json)?;
            self.apply_chat_view(state, cx)
        });
        self.restoring_view = None;
        self.waiting = self.pending_open;
        match result {
            Ok(()) => self.view_loaded = Some(chat_id),
            Err(error) => {
                self.layout.error = Some(format!("Cannot restore saved input: {error:#}"))
            }
        }
        if self.file.is_none() && self.rename.is_none() {
            self.input.read(cx).focus_handle(cx).focus(window);
        }
        cx.notify();
    }

    fn apply_chat_view(&mut self, mut state: ChatView, cx: &mut Context<Self>) -> Result<()> {
        ensure!(state.version == 1, "Unsupported chat view version");
        let chat = self.chat.as_ref().unwrap();
        let accepted = state
            .submission
            .as_ref()
            .is_some_and(|submission| submission.accepted(chat));
        let unchanged = state
            .submission
            .as_ref()
            .is_some_and(|submission| submission.revision == state.input.revision());
        let restore_draft =
            (accepted && unchanged) || (state.branch != chat.active_branch() && !accepted);
        if restore_draft && let Some(edit) = state.edit.take() {
            let (input, attachments) = edit.restore_draft(cx)?;
            self.input = input;
            self.attachments = attachments;
            self.transcript
                .restore_state(state.transcript, self.chat.as_ref().unwrap());
            return Ok(());
        }
        let input = cx.new(Input::new);
        if !accepted || !unchanged {
            input.update(cx, |input, cx| input.restore_state(state.input, cx))?;
        }
        let edit = state.edit.map(|edit| edit.restore(chat, cx)).transpose()?;
        self.input = input;
        self.attachments = attachments::AttachmentState {
            pdfs: if accepted { Vec::new() } else { state.pdfs },
            loading: None,
        };
        self.message_edit = edit;
        self.transcript
            .restore_state(state.transcript, self.chat.as_ref().unwrap());
        Ok(())
    }
}
