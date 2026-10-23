use super::*;
use space::{model::Model, store::ChatSummary};

impl Workspace {
    pub(super) fn event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Chats(chats) => self.apply_chats(chats),
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
            } => self.apply_delta(chat_id, run_id, text),
            Event::ThoughtDelta {
                chat_id,
                run_id,
                text,
            } => self.apply_thought_delta(chat_id, run_id, text),
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
            Event::UiSettings(settings) => {
                if !self.layout.dirty {
                    self.layout.sidebar_width = px(settings.sidebar_width);
                    self.layout.composer_height = px(settings.composer_height);
                }
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
                self.waiting = false;
            }
        }
    }

    fn apply_chats(&mut self, chats: Vec<ChatSummary>) {
        self.sidebar
            .runs
            .retain(|id, _| chats.iter().any(|chat| chat.id == *id));
        self.sidebar.chats = chats;
    }

    fn apply_activity(&mut self, chat_id: Uuid, run_id: Uuid, timestamp: u64) {
        if self.sidebar.runs.get(&chat_id) == Some(&run_id) {
            if let Some(summary) = self
                .sidebar
                .chats
                .iter_mut()
                .find(|chat| chat.id == chat_id)
            {
                summary.updated_at = timestamp;
            }
            self.sidebar
                .chats
                .sort_by_key(|chat| std::cmp::Reverse(chat.updated_at));
            if let Some(chat) = &mut self.chat
                && chat.id == chat_id
            {
                chat.updated_at = timestamp;
            }
        }
    }

    fn apply_state(
        &mut self,
        chat: Chat,
        opened: bool,
        run_id: Option<Uuid>,
        cx: &mut Context<Self>,
    ) {
        if let Some(run_id) = run_id {
            self.sidebar.runs.insert(chat.id, run_id);
        } else {
            self.sidebar.runs.remove(&chat.id);
        }
        let summary = ChatSummary::from(&chat);
        if let Some(existing) = self
            .sidebar
            .chats
            .iter_mut()
            .find(|item| item.id == chat.id)
        {
            *existing = summary;
        } else {
            self.sidebar.chats.push(summary);
        }
        self.sidebar
            .chats
            .sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        if !opened
            && self
                .chat
                .as_ref()
                .is_none_or(|selected| selected.id != chat.id)
        {
            return;
        }
        if self.chat.is_none() {
            self.sidebar.archived = chat.archived;
        }
        if opened {
            self.pending_open = false;
        }
        let previous_chat = self.chat.take();
        let (changed_content, changed) =
            self.transcript
                .update_snapshot(previous_chat.as_ref(), &chat, opened);
        if changed {
            self.model_menu = None;
            self.input.update(cx, |input, cx| input.clear(cx));
        }
        if !self.pending_open {
            self.waiting = false;
        }
        self.chat = Some(chat);
        self.refresh_rows(changed_content, changed);
    }

    fn apply_delta(&mut self, chat_id: Uuid, run_id: Uuid, text: String) {
        if self.sidebar.runs.get(&chat_id) == Some(&run_id)
            && let Some(chat) = &mut self.chat
            && chat.id == chat_id
        {
            chat.partial.push_str(&text);
            self.transcript
                .partial_markdown
                .answer
                .update(&chat.partial, true);
            let content_index = chat.contents.len();
            self.refresh_rows(content_index, false);
        }
    }

    fn apply_thought_delta(&mut self, chat_id: Uuid, run_id: Uuid, text: String) {
        if self.sidebar.runs.get(&chat_id) == Some(&run_id)
            && let Some(chat) = &mut self.chat
            && chat.id == chat_id
        {
            chat.partial_thoughts.push_str(&text);
            self.transcript
                .partial_markdown
                .thoughts
                .update(&chat.partial_thoughts, true);
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
