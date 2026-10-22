use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
    rc::Rc,
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, bail, ensure};
use async_channel::{Receiver, Sender};
use futures_util::{StreamExt, stream::FuturesUnordered};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    cache,
    gemini::Gemini,
    model::{Chat, Content, Model, Role, Status, ToolChange, function_response, now},
    project::{DirectoryListing, Project, declarations},
    store::{ChatSummary, Preferences, Store, UiSettings},
};

pub enum Command {
    Send { chat_id: Uuid, text: String },
    Stop(Uuid),
    Rename { chat_id: Uuid, title: String },
    SetArchived { chat_id: Uuid, archived: bool },
    SetModel { chat_id: Uuid, model: Model },
    Select(Uuid),
    Delete(Uuid),
    New,
}

pub enum Event {
    State {
        chat: Box<Chat>,
        opened: bool,
        run_id: Option<Uuid>,
    },
    Chats(Vec<ChatSummary>),
    Activity {
        chat_id: Uuid,
        run_id: Uuid,
        timestamp: u64,
    },
    Delta {
        chat_id: Uuid,
        run_id: Uuid,
        text: String,
    },
    ThoughtDelta {
        chat_id: Uuid,
        run_id: Uuid,
        text: String,
    },
    ContextTokens {
        chat_id: Uuid,
        run_id: Uuid,
        tokens: u64,
        model: Model,
    },
    RunError {
        chat_id: Uuid,
        run_id: Uuid,
        error: String,
    },
    Error(String),
    UiSettings(UiSettings),
    SettingsError(String),
    SidebarError(String),
    Directory {
        path: PathBuf,
        listing: Result<DirectoryListing, String>,
    },
}

struct LiveChat {
    state: Rc<RefCell<Chat>>,
    run: Option<(Uuid, CancellationToken)>,
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
            self.run.as_ref().map(|run| run.0),
        );
    }
}

pub enum SidebarCommand {
    SaveSettings(UiSettings),
    WatchDirectories(Vec<PathBuf>),
}

pub struct Worker {
    pub commands: Sender<Command>,
    pub sidebar_commands: Sender<SidebarCommand>,
    pub events: Receiver<Event>,
    pub shutdown: CancellationToken,
    pub thread: JoinHandle<Result<()>>,
}

impl Worker {
    pub fn spawn(project: Project, store: Store) -> Result<Self> {
        let (commands, receiver) = async_channel::unbounded();
        let (sidebar_commands, sidebar_receiver) = async_channel::unbounded();
        let (events, event_receiver) = async_channel::unbounded();
        let shutdown = CancellationToken::new();
        let exit = shutdown.clone();
        let thread = thread::Builder::new()
            .name("space-agent".into())
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(async move {
                        let chats = async {
                            let result =
                                serve_chats(&project, &store, &receiver, &events, &exit).await;
                            exit.cancel();
                            result
                        };
                        let (result, ()) = tokio::join!(
                            chats,
                            serve_sidebar(&project, &store, &sidebar_receiver, &events, &exit)
                        );
                        // Save pending layout changes before the worker exits.
                        while let Ok(command) = sidebar_receiver.try_recv() {
                            if let SidebarCommand::SaveSettings(settings) = command {
                                store.save_ui_settings(settings)?;
                            }
                        }
                        result
                    })
            })?;
        Ok(Self {
            commands,
            sidebar_commands,
            events: event_receiver,
            shutdown,
            thread,
        })
    }
}

async fn serve_chats(
    project: &Project,
    store: &Store,
    receiver: &Receiver<Command>,
    events: &Sender<Event>,
    exit: &CancellationToken,
) -> Result<()> {
    let chat = match store.restore() {
        Ok(chat) => chat,
        Err(error) => {
            let _ = events.try_send(Event::Error(format!(
                "Cannot restore the last chat: {error:#}"
            )));
            let chat = store.create_chat()?;
            store.select(chat.id)?;
            chat
        }
    };
    // No saved run belongs to this process yet. Recover without repeating tools.
    for summary in store.list()? {
        if summary.status == Status::Running
            && let Err(error) = store.resume(summary.id)
        {
            let _ = events.try_send(Event::Error(format!("{error:#}")));
        }
    }
    let gemini = Gemini::new(std::env::var("GEMINI_API_KEY").unwrap_or_default())?;
    let mut selected = chat.id;
    let mut chats = HashMap::from([(chat.id, LiveChat::new(chat))]);
    let mut runs = FuturesUnordered::new();
    let mut cleanups = FuturesUnordered::new();
    let _ = events.try_send(Event::Chats(store.list()?));
    chats[&selected].publish(events, true);
    loop {
        for name in store.take_cache_cleanup() {
            cleanups.push(gemini.delete_cache(name));
        }
        let command = tokio::select! {
            biased;
            _ = exit.cancelled() => break,
            Some(()) = cleanups.next(), if !cleanups.is_empty() => continue,
            Some((id, run_id, result)) = runs.next(), if !runs.is_empty() => {
                finish_run(store, &mut chats, &mut selected, id, run_id, result, events);
                chats.retain(|id, live| *id == selected || live.run.is_some());
                continue;
            }
            command = receiver.recv() => match command { Ok(command) => command, Err(_) => break },
        };
        let result: Result<()> = (|| {
            match command {
                Command::Send { chat_id, text } => {
                    let live = live_chat(&mut chats, store, chat_id)?;
                    ensure!(live.run.is_none(), "This chat is already running");
                    let mut chat = live.state.borrow_mut();
                    ensure!(
                        !chat.archived,
                        "Unarchive this chat before sending a message"
                    );
                    ensure!(!text.trim().is_empty(), "The message is empty");
                    let model = Model::from_id(&chat.model)
                        .context("Select a supported model before sending a message")?;
                    chat.system = project.system_prompt()?;
                    chat.tools = declarations();
                    chat.push_user(&text);
                    chat.partial_model = model;
                    if let Err(error) = store.save(&mut chat) {
                        chat.interrupt(&format!("{error:#}"));
                        drop(chat);
                        live.publish(events, false);
                        return Err(error);
                    }
                    drop(chat);
                    let run_id = Uuid::new_v4();
                    let cancel = exit.child_token();
                    live.run = Some((run_id, cancel.clone()));
                    live.publish(events, false);
                    runs.push(run_chat(
                        project,
                        store,
                        &gemini,
                        live.state.clone(),
                        run_id,
                        events,
                        cancel,
                    ));
                }
                Command::Stop(id) => {
                    if let Some((_, cancel)) = chats.get(&id).and_then(|live| live.run.as_ref()) {
                        cancel.cancel();
                    }
                }
                Command::Rename { chat_id, title } => {
                    let title = title.trim();
                    ensure!(!title.is_empty(), "Enter a chat name");
                    ensure!(
                        !title.contains(['\n', '\r']),
                        "Use one line for the chat name"
                    );
                    let live = live_chat(&mut chats, store, chat_id)?;
                    let mut chat = live.state.borrow_mut();
                    let previous = (chat.title.clone(), chat.title_is_custom);
                    chat.title = title.into();
                    chat.title_is_custom = true;
                    if let Err(error) = store.save(&mut chat) {
                        (chat.title, chat.title_is_custom) = previous;
                        return Err(error);
                    }
                    drop(chat);
                    live.publish(events, false);
                }
                Command::SetArchived { chat_id, archived } => {
                    let live = live_chat(&mut chats, store, chat_id)?;
                    let mut chat = live.state.borrow_mut();
                    let previous = chat.archived;
                    chat.archived = archived;
                    if let Err(error) = store.save(&mut chat) {
                        chat.archived = previous;
                        return Err(error);
                    }
                    drop(chat);
                    live.publish(events, false);
                }
                Command::SetModel { chat_id, model } => {
                    let live = live_chat(&mut chats, store, chat_id)?;
                    let mut chat = live.state.borrow_mut();
                    chat.cost_usd = Some(chat.token_cost_usd());
                    let previous = std::mem::replace(&mut chat.model, model.id().into());
                    if let Err(error) = store.save(&mut chat) {
                        chat.model = previous;
                        return Err(error);
                    }
                    if let Err(error) = store.save_preferences(&Preferences {
                        default_model: model,
                    }) {
                        chat.model = previous;
                        store
                            .save(&mut chat)
                            .context("Cannot restore the previous model choice")?;
                        drop(chat);
                        live.publish(events, false);
                        return Err(error);
                    }
                    drop(chat);
                    live.publish(events, false);
                }
                Command::Select(id) => {
                    let live = live_chat(&mut chats, store, id)?;
                    store.select(id)?;
                    selected = id;
                    live.publish(events, true);
                }
                Command::Delete(id) => {
                    if let Some(live) = chats.get_mut(&id)
                        && let Some((_, cancel)) = &live.run
                    {
                        live.deleting = true;
                        cancel.cancel();
                    } else {
                        delete_chat(store, &mut chats, &mut selected, id, events)?;
                    }
                }
                Command::New => {
                    let chat = store.create_chat()?;
                    store.select(chat.id)?;
                    selected = chat.id;
                    let live = chats.entry(chat.id).or_insert_with(|| LiveChat::new(chat));
                    live.publish(events, true);
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = events.try_send(Event::Error(format!("{error:#}")));
        }
        // Keep only the selected chat and chats with live runs in memory.
        chats.retain(|id, live| *id == selected || live.run.is_some());
    }
    exit.cancel();
    while let Some((id, run_id, result)) = runs.next().await {
        finish_run(store, &mut chats, &mut selected, id, run_id, result, events);
    }
    for name in store.take_cache_cleanup() {
        cleanups.push(gemini.delete_cache(name));
    }
    while cleanups.next().await.is_some() {}
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

fn finish_run(
    store: &Store,
    chats: &mut HashMap<Uuid, LiveChat>,
    selected: &mut Uuid,
    id: Uuid,
    run_id: Uuid,
    result: Result<()>,
    events: &Sender<Event>,
) {
    let Some(live) = chats.get_mut(&id) else {
        return;
    };
    if live.run.as_ref().is_none_or(|run| run.0 != run_id) {
        return;
    }
    if let Err(error) = result {
        let _ = events.try_send(Event::RunError {
            chat_id: id,
            run_id,
            error: format!("{error:#}"),
        });
    }
    live.run = None;
    if live.deleting {
        live.deleting = false;
        if let Err(error) = delete_chat(store, chats, selected, id, events) {
            let _ = events.try_send(Event::Error(format!("{error:#}")));
            if let Some(live) = chats.get(&id) {
                live.publish(events, false);
            }
        }
    } else {
        live.publish(events, false);
    }
}

fn delete_chat(
    store: &Store,
    chats: &mut HashMap<Uuid, LiveChat>,
    selected: &mut Uuid,
    id: Uuid,
    events: &Sender<Event>,
) -> Result<()> {
    if *selected == id {
        let next = store
            .list()?
            .into_iter()
            .filter(|summary| summary.id != id && !summary.archived)
            .find_map(|summary| live_chat(chats, store, summary.id).ok().map(|_| summary.id));
        let next = match next {
            Some(id) => id,
            None => {
                let chat = store.create_chat()?;
                let next = chat.id;
                chats.insert(next, LiveChat::new(chat));
                next
            }
        };
        // Keep the active file valid before removing the selected chat.
        store.select(next)?;
        *selected = next;
        chats[&next].publish(events, true);
    }
    let result = store.delete(id);
    if result.is_ok() {
        chats.remove(&id);
    }
    let _ = events.try_send(Event::Chats(store.list()?));
    result
}

async fn run_chat(
    project: &Project,
    store: &Store,
    gemini: &Gemini,
    state: Rc<RefCell<Chat>>,
    run_id: Uuid,
    events: &Sender<Event>,
    cancel: CancellationToken,
) -> (Uuid, Uuid, Result<()>) {
    let result = run(project, store, gemini, &state, run_id, events, &cancel).await;
    let mut chat = state.borrow_mut();
    if let Err(error) = result {
        chat.interrupt(&format!("{error:#}"));
    }
    let result = store.save(&mut chat);
    (chat.id, run_id, result)
}

async fn serve_sidebar(
    project: &Project,
    store: &Store,
    receiver: &Receiver<SidebarCommand>,
    events: &Sender<Event>,
    exit: &CancellationToken,
) {
    let mut directories = HashMap::new();
    let (changed, changes) = async_channel::bounded(1);
    let watcher = match watch_project(&project.path, changed, events.clone()) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            let _ = events.try_send(Event::SidebarError(format!(
                "Cannot watch the project: {error:#}"
            )));
            None
        }
    };
    match store.ui_settings() {
        Ok(settings) => {
            let _ = events.try_send(Event::UiSettings(settings));
        }
        Err(error) => {
            let _ = events.try_send(Event::SettingsError(format!(
                "Cannot load layout settings: {error:#}"
            )));
        }
    }
    loop {
        let command = tokio::select! {
            biased;
            _ = exit.cancelled() => break,
            command = receiver.recv() => match command { Ok(command) => command, Err(_) => break },
            Ok(()) = changes.recv(), if watcher.is_some() => {
                let paths: Vec<_> = directories.keys().cloned().collect();
                for path in paths {
                    update_directory(project, path, &mut directories, events);
                }
                continue;
            }
        };
        match command {
            SidebarCommand::SaveSettings(settings) => {
                if let Err(error) = store.save_ui_settings(settings) {
                    let _ = events.try_send(Event::SettingsError(format!(
                        "Cannot save layout settings: {error:#}"
                    )));
                }
            }
            SidebarCommand::WatchDirectories(paths) => {
                directories.retain(|path, _| paths.contains(path));
                for path in paths {
                    update_directory(project, path, &mut directories, events);
                }
            }
        }
    }
}

fn watch_project(
    path: &Path,
    changed: Sender<()>,
    events: Sender<Event>,
) -> Result<RecommendedWatcher> {
    let root = path.to_path_buf();
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| match result {
            Ok(event) => {
                let relevant = !event.kind.is_access()
                    && (event.paths.is_empty()
                        || event.paths.iter().any(|path| {
                            path.strip_prefix(&root).is_ok_and(|relative| {
                                !relative.components().any(|part| {
                                    part.as_os_str()
                                        .to_string_lossy()
                                        .eq_ignore_ascii_case(".space")
                                })
                            })
                        }));
                if relevant || event.need_rescan() {
                    // Coalesce queued events; the worker reloads all visible folders.
                    let _ = changed.try_send(());
                }
            }
            Err(error) => {
                let _ = events.try_send(Event::SidebarError(format!(
                    "Cannot watch the project: {error}"
                )));
                let _ = changed.try_send(());
            }
        },
        Config::default().with_follow_symlinks(false),
    )?;
    watcher.watch(path, RecursiveMode::Recursive)?;
    Ok(watcher)
}

fn update_directory(
    project: &Project,
    path: PathBuf,
    directories: &mut HashMap<PathBuf, Result<DirectoryListing, String>>,
    events: &Sender<Event>,
) {
    let listing = project
        .list_directory(&path)
        .map_err(|error| format!("{error:#}"));
    if directories.get(&path) != Some(&listing) {
        directories.insert(path.clone(), listing.clone());
        let _ = events.try_send(Event::Directory { path, listing });
    }
}

fn publish(chat: &Chat, events: &Sender<Event>, opened: bool, run_id: Option<Uuid>) {
    let _ = events.try_send(Event::State {
        chat: Box::new(chat.clone()),
        opened,
        run_id,
    });
}

fn record_received_activity(
    state: &RefCell<Chat>,
    received: &Cell<bool>,
    run_id: Uuid,
    events: &Sender<Event>,
) {
    if received.replace(true) {
        return;
    }
    let mut chat = state.borrow_mut();
    chat.record_activity();
    let _ = events.try_send(Event::Activity {
        chat_id: chat.id,
        run_id,
        timestamp: chat.updated_at,
    });
}

pub(crate) async fn run(
    project: &Project,
    store: &Store,
    gemini: &Gemini,
    state: &RefCell<Chat>,
    run_id: Uuid,
    events: &Sender<Event>,
    cancel: &CancellationToken,
) -> Result<()> {
    let model = state.borrow().partial_model;
    let mut cache_disabled = false;
    for round in 0..=32 {
        let (chat_id, request) = {
            let chat = state.borrow();
            (chat.id, Gemini::request(&chat))
        };
        let prepared = if cache_disabled {
            None
        } else {
            let prepared = tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("Run stopped"),
                result = cache::prepare(gemini, store, state, model, &request) => result,
            };
            match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    cache_disabled = true;
                    state.borrow_mut().cache_notice = Some(format!("Cache unavailable: {error:#}"));
                    None
                }
            }
        };
        publish(&state.borrow(), events, false, Some(run_id));
        let mut using_cache = prepared.is_some();
        let received = Cell::new(false);
        let mut usage = None;
        let started_at = now();
        let generation = loop {
            let body = if using_cache {
                &prepared.as_ref().unwrap().request
            } else {
                &request
            };
            let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow::anyhow!("Run stopped")),
            result = gemini.generate(
                model.id(),
                body,
                |text| {
                    record_received_activity(state, &received, run_id, events);
                    state.borrow_mut().partial.push_str(text);
                    let _ = events.try_send(Event::Delta { chat_id, run_id, text: text.into() });
                },
                |text| {
                    record_received_activity(state, &received, run_id, events);
                    state.borrow_mut().partial_thoughts.push_str(text);
                    let _ = events.try_send(Event::ThoughtDelta { chat_id, run_id, text: text.into() });
                },
                |reported| {
                    usage = Some(reported);
                    let tokens = model.context_tokens(reported);
                    {
                        let mut chat = state.borrow_mut();
                        chat.context_tokens = Some(tokens);
                        chat.context_model = model;
                    }
                    let _ = events.try_send(Event::ContextTokens { chat_id, run_id, tokens, model });
                },
            ) => result,
            };
            if using_cache
                && usage.is_none()
                && result.as_ref().is_err_and(crate::gemini::cache_rejected)
            {
                let cache = &prepared.as_ref().unwrap().cache;
                {
                    let mut chat = state.borrow_mut();
                    if chat.partial.is_empty() && chat.partial_thoughts.is_empty() {
                        chat.cache.entries.retain(|entry| entry.name != cache.name);
                        chat.cache_notice =
                            Some("Cache unavailable. Using the full conversation.".into());
                        store.save(&mut chat)?;
                    } else {
                        break result;
                    }
                }
                using_cache = false;
                cache_disabled = true;
                continue;
            }
            break result;
        };
        let (calls, content_index) = {
            let mut chat = state.borrow_mut();
            if let Some(usage) = usage {
                chat.record_usage(model, usage, started_at);
                if generation
                    .as_ref()
                    .is_ok_and(|generation| generation.finish_reason == "STOP")
                {
                    let explicit_tokens = if using_cache {
                        prepared.as_ref().unwrap().cache.tokens
                    } else {
                        0
                    };
                    chat.cache
                        .observe(model, &request, usage, explicit_tokens, started_at);
                }
            } else if generation.is_ok()
                || !chat.partial.is_empty()
                || !chat.partial_thoughts.is_empty()
                || cancel.is_cancelled()
            {
                chat.usage_complete = false;
            }
            let generation = generation?;
            let calls: Vec<_> = generation
                .content
                .parts
                .iter()
                .enumerate()
                .filter_map(|(index, part)| {
                    part.get("functionCall").cloned().map(|call| (index, call))
                })
                .collect();
            chat.partial.clear();
            chat.partial_thoughts.clear();
            let content_index = chat.contents.len();
            chat.contents.push(generation.content);
            chat.response_models.insert(content_index, model);
            chat.record_activity();
            // Save the complete call and signature before any file operation starts.
            store.save(&mut chat)?;
            publish(&chat, events, false, Some(run_id));
            if generation.finish_reason != "STOP" {
                bail!(
                    "Response stopped: {}. Any requested file operations were not run",
                    generation.finish_reason
                );
            }
            if calls.is_empty() {
                chat.status = Status::Idle;
                return Ok(());
            }
            if round == 32 {
                bail!("Reached the limit of 32 tool rounds");
            }
            chat.contents.push(Content {
                role: Role::User,
                parts: Vec::new(),
            });
            (calls, content_index)
        };
        for (part_index, call) in calls {
            // Let the worker process chat controls between synchronous file operations.
            tokio::task::yield_now().await;
            if cancel.is_cancelled() {
                bail!("Run stopped");
            }
            // This worker owns all file I/O. No UI thread or competing run can enter here.
            let (result, change) = project.execute(&call);
            let mut chat = state.borrow_mut();
            if let Some(change) = change {
                chat.file_changes.push(ToolChange {
                    content_index,
                    part_index,
                    change,
                });
            }
            chat.contents
                .last_mut()
                .context("Missing tool response")?
                .parts
                .push(function_response(&call, result));
            store.save(&mut chat)?;
            publish(&chat, events, false, Some(run_id));
        }
    }
    unreachable!("The last round returns before executing tools")
}
