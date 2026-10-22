use super::{Event, SidebarCommand};
use crate::{
    project::{DirectoryListing, Project},
    store::Store,
};
use anyhow::Result;
use async_channel::{Receiver, Sender};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub(super) async fn serve(
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
                if relevant_change(&event, &root) || event.need_rescan() {
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
