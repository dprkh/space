use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use async_channel::Sender;
use gpui::{prelude::*, *};
use space::{agent::SidebarCommand, project::DirectoryListing};

use super::icons::Icon;

enum DirectoryState {
    Loading,
    Loaded(DirectoryListing),
    Error(String),
}

pub struct OpenFile(pub PathBuf);

pub struct FileTree {
    commands: Sender<SidebarCommand>,
    directories: HashMap<PathBuf, DirectoryState>,
    expanded: HashSet<PathBuf>,
    watching: Vec<PathBuf>,
    active: bool,
    selected: Option<PathBuf>,
}

impl EventEmitter<OpenFile> for FileTree {}

impl FileTree {
    pub fn new(commands: Sender<SidebarCommand>) -> Self {
        Self {
            commands,
            directories: HashMap::new(),
            expanded: HashSet::from([PathBuf::from(".")]),
            watching: Vec::new(),
            active: false,
            selected: None,
        }
    }

    pub fn select(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.selected = path;
        cx.notify();
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        self.watch(cx);
    }

    fn visible_directories(&self, path: &Path, paths: &mut Vec<PathBuf>) {
        paths.push(path.to_path_buf());
        if let Some(DirectoryState::Loaded(listing)) = self.directories.get(path) {
            for entry in &listing.entries {
                if entry.directory && self.expanded.contains(&entry.path) {
                    self.visible_directories(&entry.path, paths);
                }
            }
        }
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        let mut paths = Vec::new();
        if self.active {
            self.visible_directories(Path::new("."), &mut paths);
        }
        paths.sort();
        if paths == self.watching {
            return;
        }
        if self
            .commands
            .try_send(SidebarCommand::WatchDirectories(paths.clone()))
            .is_err()
        {
            self.directories.insert(
                PathBuf::from("."),
                DirectoryState::Error("The file worker is unavailable".into()),
            );
            cx.notify();
            return;
        }
        for path in &paths {
            self.directories
                .entry(path.clone())
                .or_insert(DirectoryState::Loading);
        }
        self.watching = paths;
    }

    pub fn directory(
        &mut self,
        path: PathBuf,
        listing: Result<DirectoryListing, String>,
        cx: &mut Context<Self>,
    ) {
        self.directories.insert(
            path,
            match listing {
                Ok(listing) => DirectoryState::Loaded(listing),
                Err(error) => DirectoryState::Error(error),
            },
        );
        self.watch(cx);
        cx.notify();
    }

    fn toggle(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.watch(cx);
        cx.notify();
    }

    fn rows(&self, path: &Path, depth: usize, cx: &Context<Self>, rows: &mut Vec<AnyElement>) {
        let listing = match self.directories.get(path) {
            Some(DirectoryState::Loaded(listing)) => listing,
            state => {
                let label = match state {
                    Some(DirectoryState::Error(error)) => error.clone(),
                    _ => "Loading…".into(),
                };
                rows.push(
                    div()
                        .pl(px(depth as f32 * 14.))
                        .py_1()
                        .mb_1()
                        .text_xs()
                        .text_color(rgb(0x9898a6))
                        .child(label)
                        .into_any_element(),
                );
                return;
            }
        };
        for entry in &listing.entries {
            let expanded = self.expanded.contains(&entry.path);
            let name = entry
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let path = entry.path.clone();
            let directory = entry.directory;
            let markdown = path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
            });
            rows.push(
                div()
                    .id(SharedString::from(format!("file-{}", path.display())))
                    .flex()
                    .items_center()
                    .min_w_0()
                    .pl(px(depth as f32 * 14.))
                    .pr_1()
                    .py_1()
                    .mb_1()
                    .rounded_sm()
                    .when(self.selected.as_ref() == Some(&path), |row| {
                        row.bg(rgb(0x303039))
                    })
                    .hover(|style| style.bg(rgb(0x292930)))
                    .child(div().w(px(16.)).flex_shrink_0().flex().items_center().when(
                        entry.directory,
                        |view| {
                            let icon = if expanded {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            };
                            view.child(icon.render(12., 0x9898a6))
                        },
                    ))
                    .child(div().min_w_0().truncate().child(name))
                    .when(directory || markdown, |row| {
                        row.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if directory {
                                    this.toggle(path.clone(), cx);
                                } else {
                                    cx.emit(OpenFile(path.clone()));
                                }
                            }))
                    })
                    .into_any_element(),
            );
            if entry.directory && expanded {
                self.rows(&entry.path, depth + 1, cx, rows);
            }
        }
        if listing.truncated {
            rows.push(
                div()
                    .pl(px(depth as f32 * 14.))
                    .py_1()
                    .mb_1()
                    .text_xs()
                    .text_color(rgb(0x9898a6))
                    .child("Only the first 1,000 entries are shown")
                    .into_any_element(),
            );
        }
    }
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = Vec::new();
        self.rows(Path::new("."), 0, cx, &mut rows);
        div()
            .id("file-tree")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .children(rows)
    }
}
