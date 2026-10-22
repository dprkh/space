use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, bail};
use async_channel::{Receiver, Sender};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    gemini::Gemini,
    model::{Chat, Content, Role, Status, ToolChange, function_response, now},
    project::{DirectoryListing, Project, declarations},
    store::{ChatSummary, Store, UiSettings},
};

pub enum Command {
    Send {
        chat_id: Uuid,
        text: String,
        cancel: CancellationToken,
    },
    Select(Uuid),
    Delete(Uuid),
    New,
}

pub enum Event {
    State {
        chat: Box<Chat>,
        opened: bool,
    },
    Chats(Vec<ChatSummary>),
    Delta(String),
    ThoughtDelta(String),
    ContextTokens(u64),
    Error(String),
    UiSettings(UiSettings),
    SettingsError(String),
    SidebarError(String),
    Directory {
        path: PathBuf,
        listing: Result<DirectoryListing, String>,
    },
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
    let mut chat = match store.restore() {
        Ok(chat) => chat,
        Err(error) => {
            let _ = events.try_send(Event::Error(format!(
                "Cannot restore the last chat: {error:#}"
            )));
            let mut chat = Chat::default();
            store.save(&mut chat)?;
            store.select(chat.id)?;
            chat
        }
    };
    let gemini = Gemini::new(std::env::var("GEMINI_API_KEY").unwrap_or_default())?;
    let _ = events.try_send(Event::Chats(store.list()?));
    publish(&chat, events, true);
    let mut pending = VecDeque::new();
    while !exit.is_cancelled() {
        let command = if let Some(command) = pending.pop_front() {
            command
        } else {
            tokio::select! {
                biased;
                _ = exit.cancelled() => break,
                command = receiver.recv() => match command { Ok(command) => command, Err(_) => break }
            }
        };
        let opening = matches!(command, Command::Select(_) | Command::New)
            || matches!(command, Command::Delete(id) if id == chat.id);
        let result: Result<()> = match command {
            Command::Send { chat_id, .. } if chat_id != chat.id => Err(anyhow::anyhow!(
                "The selected chat changed. Send the message again"
            )),
            Command::Send { text, cancel, .. } => {
                let result = async {
                    chat.system = project.system_prompt()?;
                    chat.tools = declarations();
                    chat.push_user(&text);
                    store.save(&mut chat)?;
                    publish(&chat, events, false);
                    let active_id = chat.id;
                    let response = run(project, store, &gemini, &mut chat, events, &cancel);
                    tokio::pin!(response);
                    loop {
                        tokio::select! {
                            biased;
                            result = &mut response => break result,
                            command = receiver.recv(), if !receiver.is_closed() => {
                                match command {
                                    Ok(Command::Delete(id)) if id != active_id => {
                                        if let Err(error) = delete_saved_chat(store, id, events) {
                                            let _ = events.try_send(Event::Error(format!("{error:#}")));
                                        }
                                    }
                                    Ok(command) => {
                                        cancel.cancel();
                                        pending.push_back(command);
                                    }
                                    Err(_) => cancel.cancel(),
                                }
                            }
                        }
                    }
                }
                .await;
                if let Err(error) = result {
                    chat.interrupt(&format!("{error:#}"));
                }
                store.save(&mut chat)
            }
            Command::Select(id) => store.resume(id).and_then(|loaded| {
                store.select(id)?;
                chat = loaded;
                Ok(())
            }),
            Command::Delete(id) => (|| {
                if id == chat.id {
                    let next = store
                        .list()?
                        .into_iter()
                        .filter(|summary| summary.id != id)
                        .find_map(|summary| store.resume(summary.id).ok());
                    let next = match next {
                        Some(chat) => chat,
                        None => {
                            let mut chat = Chat::default();
                            store.save(&mut chat)?;
                            chat
                        }
                    };
                    // Keep the active file valid before removing the selected chat.
                    store.select(next.id)?;
                    chat = next;
                }
                delete_saved_chat(store, id, events)
            })(),
            Command::New => {
                let mut new = Chat::default();
                store.save(&mut new).and_then(|()| {
                    store.select(new.id)?;
                    chat = new;
                    Ok(())
                })
            }
        };
        if let Err(error) = result {
            let _ = events.try_send(Event::Error(format!("{error:#}")));
        }
        publish(&chat, events, opening);
    }
    Ok(())
}

fn delete_saved_chat(store: &Store, id: Uuid, events: &Sender<Event>) -> Result<()> {
    let result = store.delete(id);
    let _ = events.try_send(Event::Chats(store.list()?));
    result
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

fn publish(chat: &Chat, events: &Sender<Event>, opened: bool) {
    let _ = events.try_send(Event::State {
        chat: Box::new(chat.clone()),
        opened,
    });
}

pub(crate) async fn run(
    project: &Project,
    store: &Store,
    gemini: &Gemini,
    chat: &mut Chat,
    events: &Sender<Event>,
    cancel: &CancellationToken,
) -> Result<()> {
    for round in 0..=32 {
        let mut partial = String::new();
        let mut partial_thoughts = String::new();
        let mut usage = None;
        let started_at = now();
        let generation = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(anyhow::anyhow!("Run stopped")),
            result = gemini.generate(
                chat,
                |text| {
                    partial.push_str(text);
                    let _ = events.try_send(Event::Delta(text.into()));
                },
                |text| {
                    partial_thoughts.push_str(text);
                    let _ = events.try_send(Event::ThoughtDelta(text.into()));
                },
                |reported| {
                    usage = Some(reported);
                    let _ = events.try_send(Event::ContextTokens(reported.context_tokens()));
                },
            ) => result,
        };
        if let Some(usage) = usage {
            chat.record_usage(usage, started_at);
        } else if generation.is_ok()
            || !partial.is_empty()
            || !partial_thoughts.is_empty()
            || cancel.is_cancelled()
        {
            chat.usage_complete = false;
        }
        let generation = match generation {
            Ok(generation) => generation,
            Err(error) => {
                chat.partial = partial;
                chat.partial_thoughts = partial_thoughts;
                return Err(error);
            }
        };
        let calls: Vec<_> = generation
            .content
            .parts
            .iter()
            .enumerate()
            .filter_map(|(index, part)| part.get("functionCall").cloned().map(|call| (index, call)))
            .collect();
        chat.partial.clear();
        chat.partial_thoughts.clear();
        let content_index = chat.contents.len();
        chat.contents.push(generation.content);
        // Save the complete call and signature before any file operation starts.
        store.save(chat)?;
        publish(chat, events, false);
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
        for (part_index, call) in calls {
            if cancel.is_cancelled() {
                bail!("Run stopped");
            }
            // This worker owns all file I/O. No UI thread or competing run can enter here.
            let (result, change) = project.execute(&call);
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
            store.save(chat)?;
            publish(chat, events, false);
        }
    }
    unreachable!("The last round returns before executing tools")
}
