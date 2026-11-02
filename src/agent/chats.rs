use super::run::RunTask;
use super::{Command, Event, publish};
use crate::{
    gemini::Gemini,
    model::{Chat, Model, Status},
    pdf::Pdf,
    project::{Project, declarations},
    store::{Preferences, Store, check_pdf_capacity},
};
use anyhow::{Context, Result, ensure};
use async_channel::{Receiver, Sender};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    cell::RefCell,
    collections::{HashMap, hash_map::Entry},
    rc::Rc,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct ActiveRun {
    id: Uuid,
    cancel: CancellationToken,
}

struct LiveChat {
    state: Rc<RefCell<Chat>>,
    run: Option<ActiveRun>,
    deleting: bool,
}

impl LiveChat {
    fn new(chat: Chat) -> Self {
        Self {
            state: Rc::new(RefCell::new(chat)),
            run: None,
            deleting: false,
        }
    }

    fn publish(&self, events: &Sender<Event>, opened: bool) {
        publish(
            &self.state.borrow(),
            events,
            opened,
            self.run.as_ref().map(|run| run.id),
        );
    }

    fn start_run(
        &mut self,
        store: &Store,
        events: &Sender<Event>,
        exit: &CancellationToken,
    ) -> Result<RunTask> {
        let mut chat = self.state.borrow_mut();
        if let Err(error) = store.save(&mut chat) {
            chat.interrupt(&format!("{error:#}"));
            drop(chat);
            self.publish(events, false);
            return Err(error);
        }
        drop(chat);
        Ok(self.launch_run(events, exit))
    }

    fn launch_run(&mut self, events: &Sender<Event>, exit: &CancellationToken) -> RunTask {
        let run_id = Uuid::new_v4();
        let cancel = exit.child_token();
        self.run = Some(ActiveRun {
            id: run_id,
            cancel: cancel.clone(),
        });
        self.publish(events, false);
        RunTask {
            state: self.state.clone(),
            id: run_id,
            cancel,
        }
    }
}

struct ChatController<'a> {
    project: &'a Project,
    store: &'a Store,
    events: &'a Sender<Event>,
    exit: &'a CancellationToken,
    selected: Uuid,
    chats: HashMap<Uuid, LiveChat>,
}

impl<'a> ChatController<'a> {
    fn restore(
        project: &'a Project,
        store: &'a Store,
        events: &'a Sender<Event>,
        exit: &'a CancellationToken,
    ) -> Result<Self> {
        let chat = restore_chat(store, events)?;
        recover_chats(store, events)?;
        Ok(Self {
            project,
            store,
            events,
            exit,
            selected: chat.id,
            chats: HashMap::from([(chat.id, LiveChat::new(chat))]),
        })
    }

    fn handle(&mut self, command: Command) -> Result<Option<RunTask>> {
        match command {
            Command::Send {
                chat_id,
                submission_id,
                text,
                pdfs,
            } => {
                let result = self.send(chat_id, text, pdfs);
                let _ = self.events.try_send(Event::SendResult {
                    chat_id,
                    submission_id,
                    result: result
                        .as_ref()
                        .map(|_| ())
                        .map_err(|error| format!("{error:#}")),
                });
                return Ok(result.ok());
            }
            Command::Retry(id) => return self.retry(id).map(Some),
            Command::Stop(id) => self.stop(id),
            Command::Rename { chat_id, title } => self.rename(chat_id, title)?,
            Command::SetArchived { chat_id, archived } => self.set_archived(chat_id, archived)?,
            Command::SetModel { chat_id, model } => self.set_model(chat_id, model)?,
            Command::Select(id) => self.select(id)?,
            Command::Delete(id) => self.delete(id)?,
            Command::New => self.new_chat()?,
        }
        Ok(None)
    }

    fn retain_live(&mut self) {
        self.chats
            .retain(|id, live| *id == self.selected || live.run.is_some());
    }

    fn send(&mut self, chat_id: Uuid, text: String, pdfs: Vec<Pdf>) -> Result<RunTask> {
        let live = live_chat(&mut self.chats, self.store, chat_id)?;
        ensure!(live.run.is_none(), "This chat is already running");
        let mut chat = live.state.borrow().clone();
        ensure!(
            !chat.archived,
            "Unarchive this chat before sending a message"
        );
        let text = text.trim();
        ensure!(!text.is_empty() || !pdfs.is_empty(), "The message is empty");
        let model = Model::from_id(&chat.model)
            .context("Select a supported model before sending a message")?;
        chat.system = self.project.system_prompt()?;
        chat.tools = declarations();
        let has_pdfs = !pdfs.is_empty();
        chat.push_user(text, pdfs);
        chat.partial_model = model;
        if has_pdfs {
            check_pdf_capacity(&chat)?;
        }
        self.store.save(&mut chat)?;
        *live.state.borrow_mut() = chat;
        Ok(live.launch_run(self.events, self.exit))
    }

    fn retry(&mut self, chat_id: Uuid) -> Result<RunTask> {
        let live = live_chat(&mut self.chats, self.store, chat_id)?;
        ensure!(live.run.is_none(), "This chat is already running");
        let mut chat = live.state.borrow_mut();
        ensure!(!chat.archived, "Unarchive this chat before trying again");
        ensure!(
            chat.status == Status::Interrupted && !chat.contents.is_empty(),
            "This chat has no interrupted request"
        );
        let model =
            Model::from_id(&chat.model).context("Select a supported model before trying again")?;
        chat.record_activity();
        chat.status = Status::Running;
        chat.notice = None;
        chat.cache_notice = None;
        chat.partial.clear();
        chat.partial_thoughts.clear();
        chat.partial_model = model;
        drop(chat);
        live.start_run(self.store, self.events, self.exit)
    }

    fn stop(&self, id: Uuid) {
        if let Some(ActiveRun { cancel, .. }) =
            self.chats.get(&id).and_then(|live| live.run.as_ref())
        {
            cancel.cancel();
        }
    }

    fn rename(&mut self, chat_id: Uuid, title: String) -> Result<()> {
        let title = title.trim();
        ensure!(!title.is_empty(), "Enter a chat name");
        ensure!(
            !title.contains(['\n', '\r']),
            "Use one line for the chat name"
        );
        let live = live_chat(&mut self.chats, self.store, chat_id)?;
        let mut chat = live.state.borrow_mut();
        let previous = (chat.title.clone(), chat.title_is_custom);
        chat.title = title.into();
        chat.title_is_custom = true;
        if let Err(error) = self.store.save(&mut chat) {
            (chat.title, chat.title_is_custom) = previous;
            return Err(error);
        }
        drop(chat);
        live.publish(self.events, false);

        Ok(())
    }

    fn set_archived(&mut self, chat_id: Uuid, archived: bool) -> Result<()> {
        let live = live_chat(&mut self.chats, self.store, chat_id)?;
        let mut chat = live.state.borrow_mut();
        let previous = chat.archived;
        chat.archived = archived;
        if let Err(error) = self.store.save(&mut chat) {
            chat.archived = previous;
            return Err(error);
        }
        drop(chat);
        live.publish(self.events, false);

        Ok(())
    }

    fn set_model(&mut self, chat_id: Uuid, model: Model) -> Result<()> {
        let live = live_chat(&mut self.chats, self.store, chat_id)?;
        let mut chat = live.state.borrow_mut();
        chat.cost_usd = Some(chat.token_cost_usd());
        let previous = std::mem::replace(&mut chat.model, model.id().into());
        if let Err(error) = self.store.save(&mut chat) {
            chat.model = previous;
            return Err(error);
        }
        if let Err(error) = self.store.save_preferences(&Preferences {
            default_model: model,
        }) {
            chat.model = previous;
            self.store
                .save(&mut chat)
                .context("Cannot restore the previous model choice")?;
            drop(chat);
            live.publish(self.events, false);
            return Err(error);
        }
        drop(chat);
        live.publish(self.events, false);

        Ok(())
    }

    fn select(&mut self, id: Uuid) -> Result<()> {
        let live = live_chat(&mut self.chats, self.store, id)?;
        self.store.select(id)?;
        self.selected = id;
        live.publish(self.events, true);

        Ok(())
    }

    fn delete(&mut self, id: Uuid) -> Result<()> {
        if let Some(live) = self.chats.get_mut(&id)
            && let Some(ActiveRun { cancel, .. }) = &live.run
        {
            live.deleting = true;
            cancel.cancel();
        } else {
            self.delete_chat(id)?;
        }

        Ok(())
    }

    fn new_chat(&mut self) -> Result<()> {
        let chat = self.store.create_chat()?;
        self.store.select(chat.id)?;
        self.selected = chat.id;
        let live = self
            .chats
            .entry(chat.id)
            .or_insert_with(|| LiveChat::new(chat));
        live.publish(self.events, true);

        Ok(())
    }

    fn finish_run(&mut self, id: Uuid, run_id: Uuid, result: Result<()>) {
        let Some(live) = self.chats.get_mut(&id) else {
            return;
        };
        if live.run.as_ref().is_none_or(|run| run.id != run_id) {
            return;
        }
        if let Err(error) = result {
            let _ = self.events.try_send(Event::RunError {
                chat_id: id,
                run_id,
                error: format!("{error:#}"),
            });
        }
        live.run = None;
        if live.deleting {
            live.deleting = false;
            if let Err(error) = self.delete_chat(id) {
                let _ = self.events.try_send(Event::Error(format!("{error:#}")));
                if let Some(live) = self.chats.get(&id) {
                    live.publish(self.events, false);
                }
            }
        } else {
            live.publish(self.events, false);
        }
    }

    fn delete_chat(&mut self, id: Uuid) -> Result<()> {
        if self.selected == id {
            let next = self
                .store
                .list()?
                .into_iter()
                .filter(|summary| summary.id != id && !summary.archived)
                .find_map(|summary| {
                    live_chat(&mut self.chats, self.store, summary.id)
                        .ok()
                        .map(|_| summary.id)
                });
            let next = match next {
                Some(id) => id,
                None => {
                    let chat = self.store.create_chat()?;
                    let next = chat.id;
                    self.chats.insert(next, LiveChat::new(chat));
                    next
                }
            };
            // Keep the active file valid before removing the selected chat.
            self.store.select(next)?;
            self.selected = next;
            self.chats[&next].publish(self.events, true);
        }
        let result = self.store.delete(id);
        if result.is_ok() {
            self.chats.remove(&id);
        }
        let _ = self.events.try_send(Event::Chats(self.store.list()?));
        result
    }
}

fn restore_chat(store: &Store, events: &Sender<Event>) -> Result<Chat> {
    match store.restore() {
        Ok(chat) => Ok(chat),
        Err(error) => {
            let _ = events.try_send(Event::Error(format!(
                "Cannot restore the last chat: {error:#}"
            )));
            let chat = store.create_chat()?;
            store.select(chat.id)?;
            Ok(chat)
        }
    }
}

fn recover_chats(store: &Store, events: &Sender<Event>) -> Result<()> {
    // No saved run belongs to this process yet. Recover without repeating tools.
    for summary in store.list()? {
        if summary.status == Status::Running
            && let Err(error) = store.resume(summary.id)
        {
            let _ = events.try_send(Event::Error(format!("{error:#}")));
        }
    }
    Ok(())
}

fn live_chat<'a>(
    chats: &'a mut HashMap<Uuid, LiveChat>,
    store: &Store,
    id: Uuid,
) -> Result<&'a mut LiveChat> {
    let live = match chats.entry(id) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => entry.insert(LiveChat::new(store.resume(id)?)),
    };
    ensure!(!live.deleting, "The chat is being deleted");
    Ok(live)
}

pub(super) async fn serve(
    project: &Project,
    store: &Store,
    receiver: &Receiver<Command>,
    events: &Sender<Event>,
    exit: &CancellationToken,
) -> Result<()> {
    let mut controller = ChatController::restore(project, store, events, exit)?;
    let gemini = Gemini::new(std::env::var("GEMINI_API_KEY").unwrap_or_default())?;
    let mut runs = FuturesUnordered::new();
    let mut cleanups = FuturesUnordered::new();
    let _ = events.try_send(Event::Chats(store.list()?));
    controller.chats[&controller.selected].publish(events, true);
    loop {
        for name in store.take_cache_cleanup() {
            cleanups.push(gemini.delete_cache(name));
        }
        let command = tokio::select! {
            biased;
            _ = exit.cancelled() => break,
            Some(()) = cleanups.next(), if !cleanups.is_empty() => continue,
            Some((id, run_id, result)) = runs.next(), if !runs.is_empty() => {
                controller.finish_run(id, run_id, result);
                controller.retain_live();
                continue;
            }
            command = receiver.recv() => match command { Ok(command) => command, Err(_) => break },
        };
        match controller.handle(command) {
            Ok(Some(task)) => runs.push(task.execute(project, store, &gemini, events)),
            Ok(None) => {}
            Err(error) => {
                let _ = events.try_send(Event::Error(format!("{error:#}")));
            }
        }
        controller.retain_live();
    }
    exit.cancel();
    while let Some((id, run_id, result)) = runs.next().await {
        controller.finish_run(id, run_id, result);
    }
    for name in store.take_cache_cleanup() {
        cleanups.push(gemini.delete_cache(name));
    }
    while cleanups.next().await.is_some() {}
    Ok(())
}
