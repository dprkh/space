use super::*;
use space::model::Model;

impl Workspace {
    pub(super) fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::SendResult {
                chat_id,
                submission_id,
                result,
            } => {
                self.complete_send(chat_id, submission_id, result, window, cx);
            }
            Event::Chats(chats) => self.sidebar.replace_chats(chats),
            Event::Activity {
                chat_id,
                run_id,
                timestamp,
            } => self.apply_activity(chat_id, run_id, timestamp),
            Event::State {
                chat,
                opened,
                run_id,
            } => self.apply_state(*chat, opened, run_id, cx),
            Event::Delta {
                chat_id,
                run_id,
                text,
            } => self.apply_delta(chat_id, run_id, text, cx),
            Event::ThoughtDelta {
                chat_id,
                run_id,
                text,
            } => self.apply_thought_delta(chat_id, run_id, text, cx),
            Event::ContextTokens {
                chat_id,
                run_id,
                tokens,
                model,
            } => self.apply_context(chat_id, run_id, tokens, model),
            Event::RunError {
                chat_id,
                run_id,
                error,
            } => self.apply_run_error(chat_id, run_id, error),
            Event::UiSettings(settings) => self.layout.apply_settings(settings),
            Event::WorkspaceView(result) => self.restore_workspace_view(result, window, cx),
            Event::ChatView { chat_id, result } => {
                self.restore_chat_view(chat_id, result, window, cx)
            }
            Event::SettingsError(error) => self.layout.error = Some(error),
            Event::SidebarError(error) => {
                if let Some(file) = &self.file {
                    file.update(cx, |file, cx| file.set_watch_error(error.clone(), cx));
                }
                self.sidebar.error = Some(error);
            }
            Event::Directory { path, listing } => self
                .sidebar
                .tree
                .update(cx, |tree, cx| tree.directory(path, listing, cx)),
            Event::File {
                request_id,
                path,
                content,
            } => {
                if let Some(file) = &self.file
                    && file.read(cx).matches(request_id, &path)
                {
                    file.update(cx, |file, cx| file.update_content(content, cx));
                }
            }
            Event::Error(error) => {
                self.error = Some(error);
                self.pending_open = false;
                self.pending_message_version = None;
                self.waiting = false;
            }
        }
    }

    fn apply_activity(&mut self, chat_id: Uuid, run_id: Uuid, timestamp: u64) {
        if self.sidebar.record_activity(chat_id, run_id, timestamp)
            && let Some(chat) = &mut self.chat
            && chat.id == chat_id
        {
            chat.updated_at = timestamp;
        }
    }

    fn apply_state(
        &mut self,
        chat: Chat,
        opened: bool,
        run_id: Option<Uuid>,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.update_chat(&chat, run_id);
        if !opened
            && self
                .chat
                .as_ref()
                .is_none_or(|selected| selected.id != chat.id)
        {
            return;
        }
        if self.chat.is_none() && !self.workspace_view_loaded {
            self.sidebar.archived = chat.archived;
        }
        if opened {
            self.pending_open = false;
        }
        let changed_chat = self.chat.as_ref().is_none_or(|old| old.id != chat.id);
        if changed_chat && let Err(error) = self.save_chat_view(cx) {
            self.layout.error = Some(format!("Cannot save the previous input: {error:#}"));
        }
        let previous_chat = self.chat.take();
        let (changed_content, changed) =
            self.transcript
                .update_snapshot(previous_chat.as_ref(), &chat, opened, cx);
        if changed {
            self.close_diagram(cx);
            self.model_menu = None;
        }
        if changed_chat {
            self.attachments = Default::default();
            self.submission = None;
            self.message_edit = None;
            self.pending_message_version = None;
            self.input.update(cx, |input, cx| input.clear(cx));
            self.view_loaded = None;
            self.restoring_view = Some(chat.id);
            if self
                .sidebar_commands
                .try_send(SidebarCommand::LoadChatView(chat.id))
                .is_err()
            {
                self.restoring_view = None;
                self.layout.error =
                    Some("Cannot load the saved input: the worker is unavailable".into());
            }
        }
        if !self.pending_open {
            self.waiting = self.restoring_view.is_some();
        }
        self.chat = Some(chat);
        self.refresh_rows(changed_content, changed);
        if opened && let Some(content_index) = self.pending_message_version.take() {
            self.transcript.show_message_controls(content_index);
        }
    }

    fn apply_delta(&mut self, chat_id: Uuid, run_id: Uuid, text: String, cx: &mut Context<Self>) {
        if self.sidebar.runs.get(&chat_id) == Some(&run_id)
            && let Some(chat) = &mut self.chat
            && chat.id == chat_id
        {
            chat.partial.push_str(&text);
            self.transcript
                .partial_markdown
                .answer
                .update(&chat.partial, true, cx);
            let content_index = chat.contents.len();
            self.refresh_rows(content_index, false);
        }
    }

    fn apply_thought_delta(
        &mut self,
        chat_id: Uuid,
        run_id: Uuid,
        text: String,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar.runs.get(&chat_id) == Some(&run_id)
            && let Some(chat) = &mut self.chat
            && chat.id == chat_id
        {
            chat.partial_thoughts.push_str(&text);
            self.transcript
                .partial_markdown
                .thoughts
                .update(&chat.partial_thoughts, true, cx);
            let content_index = chat.contents.len();
            self.refresh_rows(content_index, false);
        }
    }

    fn apply_context(&mut self, chat_id: Uuid, run_id: Uuid, tokens: u64, model: Model) {
        if self.sidebar.runs.get(&chat_id) == Some(&run_id)
            && let Some(chat) = &mut self.chat
            && chat.id == chat_id
        {
            chat.context_tokens = Some(tokens);
            chat.context_model = model;
        }
    }

    fn apply_run_error(&mut self, chat_id: Uuid, run_id: Uuid, error: String) {
        if self.sidebar.runs.get(&chat_id) == Some(&run_id)
            && self.chat.as_ref().is_some_and(|chat| chat.id == chat_id)
        {
            self.error = Some(error);
        }
    }
}
