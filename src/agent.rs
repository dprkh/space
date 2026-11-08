mod cache_metrics;
mod chats;
mod run;
mod sidebar;

use crate::{
    model::{Chat, Model},
    pdf::Pdf,
    project::{DirectoryListing, FileContent, Project},
    store::{ChatSummary, Store, UiSettings},
};
use anyhow::Result;
use async_channel::{Receiver, Sender};
use std::{
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub enum Command {
    Send {
        chat_id: Uuid,
        submission_id: Uuid,
        text: String,
        pdfs: Vec<Pdf>,
    },
    EditMessage {
        chat_id: Uuid,
        submission_id: Uuid,
        content_index: usize,
        text: String,
    },
    SelectMessageVersion {
        chat_id: Uuid,
        content_index: usize,
        version: usize,
    },
    DeleteMessage {
        chat_id: Uuid,
        content_index: usize,
    },
    Retry(Uuid),
    Stop(Uuid),
    Rename {
        chat_id: Uuid,
        title: String,
    },
    SetArchived {
        chat_id: Uuid,
        archived: bool,
    },
    SetModel {
        chat_id: Uuid,
        model: Model,
    },
    Select(Uuid),
    Delete(Uuid),
    New,
}

pub enum Event {
    SendResult {
        chat_id: Uuid,
        submission_id: Uuid,
        result: Result<(), String>,
    },
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
    WorkspaceView(Result<Option<String>, String>),
    ChatView {
        chat_id: Uuid,
        result: Result<Option<String>, String>,
    },
    SettingsError(String),
    SidebarError(String),
    Directory {
        path: PathBuf,
        listing: Result<DirectoryListing, String>,
    },
    File {
        request_id: Uuid,
        path: PathBuf,
        content: Result<Arc<FileContent>, String>,
    },
}

pub enum SidebarCommand {
    SaveSettings(UiSettings),
    SaveWorkspaceView(String),
    SaveChatView { chat_id: Uuid, json: String },
    LoadChatView(Uuid),
    WatchDirectories(Vec<PathBuf>),
    OpenFile { request_id: Uuid, path: PathBuf },
    CloseFile,
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
                let project = Arc::new(project);
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(async move {
                        let chats = async {
                            let result =
                                chats::serve(&project, &store, &receiver, &events, &exit).await;
                            exit.cancel();
                            result
                        };
                        let (result, ()) = tokio::join!(
                            chats,
                            sidebar::serve(
                                project.clone(),
                                &store,
                                &sidebar_receiver,
                                &events,
                                &exit
                            )
                        );
                        // Save pending layout changes before the worker exits.
                        while let Ok(command) = sidebar_receiver.try_recv() {
                            match command {
                                SidebarCommand::SaveSettings(settings) => {
                                    store.save_ui_settings(settings)?
                                }
                                SidebarCommand::SaveWorkspaceView(json) => {
                                    store.save_workspace_view(&json)?
                                }
                                SidebarCommand::SaveChatView { chat_id, json } => {
                                    store.save_chat_view(chat_id, &json)?
                                }
                                _ => {}
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

fn publish(chat: &Chat, events: &Sender<Event>, opened: bool, run_id: Option<Uuid>) {
    let _ = events.try_send(Event::State {
        chat: Box::new(chat.clone()),
        opened,
        run_id,
    });
}
