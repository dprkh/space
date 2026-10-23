use super::{Event, SidebarCommand};
use crate::{
    project::{DirectoryListing, FileContent, Project},
    store::Store,
};
use anyhow::Result;
use async_channel::{Receiver, Sender};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashMap,
    future::pending,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct WatchedFile {
    request_id: Uuid,
    path: PathBuf,
    content: Option<Result<Arc<FileContent>, String>>,
    reading: Option<JoinHandle<Result<Arc<FileContent>, String>>>,
    changed: bool,
    cancel: CancellationToken,
}

impl WatchedFile {
    fn refresh(&mut self, project: &Arc<Project>) {
        if self.reading.is_some() {
            self.changed = true;
            return;
        }
        self.changed = false;
        let project = project.clone();
        let path = self.path.clone();
        let cancel = self.cancel.clone();
        self.reading = Some(tokio::task::spawn_blocking(move || {
            project
                .read_file(&path, &cancel)
                .map(Arc::new)
                .map_err(|error| format!("{error:#}"))
        }));
    }

    async fn read(&mut self) -> Result<Arc<FileContent>, String> {
        let Some(reading) = &mut self.reading else {
            return pending().await;
        };
        let content = reading
            .await
            .unwrap_or_else(|error| Err(format!("Cannot read file: {error}")));
        self.reading = None;
        content
    }

    fn publish(&mut self, content: Result<Arc<FileContent>, String>, events: &Sender<Event>) {
        if self.content.as_ref() == Some(&content) {
            return;
        }
        self.content = Some(content.clone());
        let _ = events.try_send(Event::File {
            request_id: self.request_id,
            path: self.path.clone(),
            content,
        });
    }
}

impl Drop for WatchedFile {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(reading) = &self.reading {
            reading.abort();
        }
    }
}

pub(super) async fn serve(
    project: Arc<Project>,
    store: &Store,
    receiver: &Receiver<SidebarCommand>,
    events: &Sender<Event>,
    exit: &CancellationToken,
) {
    let mut directories = HashMap::new();
    let mut file: Option<WatchedFile> = None;
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
            content = async {
                match &mut file {
                    Some(file) => file.read().await,
                    None => pending().await,
                }
            } => {
                if let Some(file) = &mut file {
                    file.publish(content, events);
                    if file.changed {
                        file.refresh(&project);
                    }
                }
                continue;
            }
            Ok(()) = changes.recv(), if watcher.is_some() => {
                let paths: Vec<_> = directories.keys().cloned().collect();
                for path in paths {
                    update_directory(&project, path, &mut directories, events);
                }
                if let Some(file) = &mut file {
                    file.refresh(&project);
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
                    update_directory(&project, path, &mut directories, events);
                }
            }
            SidebarCommand::OpenFile { request_id, path } => {
                let mut opened = WatchedFile {
                    request_id,
                    path,
                    content: None,
                    reading: None,
                    changed: false,
                    cancel: exit.child_token(),
                };
                opened.refresh(&project);
                file = Some(opened);
            }
            SidebarCommand::CloseFile => file = None,
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
                if relevant_change(&event, &root) || event.need_rescan() {
                    // Coalesce queued events; reload visible folders and the open file.
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

fn relevant_change(event: &notify::Event, root: &Path) -> bool {
    !event.kind.is_access()
        && (event.paths.is_empty()
            || event.paths.iter().any(|path| {
                path.strip_prefix(root).is_ok_and(|relative| {
                    !relative.components().any(|part| {
                        part.as_os_str()
                            .to_string_lossy()
                            .eq_ignore_ascii_case(".space")
                    })
                })
            }))
}
