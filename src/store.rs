mod write;

use std::{cell::RefCell, path::Path};

use anyhow::{Context, Result, ensure};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions, OpenOptionsExt};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    fs::{open_regular, read_text},
    model::{Chat, Model, Status},
    project::Project,
};

const MAX_CHAT_SIZE: u64 = 64 * 1024 * 1024;
pub const MAX_PDF_CHAT_SIZE: u64 = MAX_CHAT_SIZE - 1024 * 1024;

pub(crate) fn check_pdf_capacity(chat: &Chat) -> Result<()> {
    ensure!(
        serde_json::to_vec(chat)?.len() as u64 <= MAX_PDF_CHAT_SIZE,
        "PDF data does not fit in this chat. Start a new chat or select a smaller PDF"
    );
    Ok(())
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub default_model: Model,
}
pub const MIN_COMPOSER_HEIGHT: f32 = 96.;
pub const MAX_COMPOSER_HEIGHT: f32 = 1200.;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub sidebar_width: f32,
    pub composer_height: f32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            sidebar_width: 232.,
            composer_height: 144.,
        }
    }
}

impl UiSettings {
    fn validated(self) -> Result<Self> {
        ensure!(self.sidebar_width.is_finite(), "Invalid sidebar width");
        ensure!(self.composer_height.is_finite(), "Invalid input height");
        Ok(Self {
            sidebar_width: self.sidebar_width.clamp(180., 480.),
            composer_height: self
                .composer_height
                .clamp(MIN_COMPOSER_HEIGHT, MAX_COMPOSER_HEIGHT),
        })
    }
}

pub struct Store {
    dir: Dir,
    chats: Dir,
    _lock: std::fs::File,
    cache_cleanup: RefCell<Vec<String>>,
}

impl Store {
    pub fn open(project: &Project) -> Result<Self> {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match project.root.create_dir_with(".space", &builder) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let dir = project
            .root
            .open_dir_nofollow(".space")
            .context("Cannot open .space; symbolic links are not allowed")?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK)
            .follow(FollowSymlinks::No);
        let lock = dir.open_with("lock", &options)?;
        use cap_std::fs::MetadataExt;
        ensure!(
            lock.metadata()?.is_file() && lock.metadata()?.nlink() == 1,
            "Invalid project lock file"
        );
        let lock = lock.into_std();
        lock.try_lock_exclusive()
            .context("This project is already open in Space")?;
        match dir.create_dir_with("chats", &builder) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let chats = dir.open_dir_nofollow("chats")?;
        Ok(Self {
            dir,
            chats,
            _lock: lock,
            cache_cleanup: RefCell::new(Vec::new()),
        })
    }

    pub fn save(&self, chat: &mut Chat) -> Result<()> {
        // Freeze the estimate for legacy usage on its first save.
        chat.cost_usd = Some(chat.token_cost_usd());
        let bytes = serde_json::to_vec(chat)?;
        ensure!(
            bytes.len() as u64 <= MAX_CHAT_SIZE,
            "Chat exceeds the 64 MiB storage limit. Start a new chat"
        );
        write::replace(&self.chats, Path::new(&format!("{}.json", chat.id)), &bytes)
    }

    pub fn delete(&self, id: Uuid) -> Result<()> {
        let caches = self
            .load(id)
            .map(|chat| chat.cache.entries)
            .unwrap_or_default();
        self.chats
            .remove_file(format!("{id}.json"))
            .context("Cannot delete the saved chat")?;
        self.cache_cleanup
            .borrow_mut()
            .extend(caches.into_iter().map(|cache| cache.name));
        self.chats.try_clone()?.into_std_file().sync_all()?;
        Ok(())
    }

    pub fn take_cache_cleanup(&self) -> Vec<String> {
        std::mem::take(&mut *self.cache_cleanup.borrow_mut())
    }

    pub fn preferences(&self) -> Result<Preferences> {
        let mut file = match open_regular(&self.dir, Path::new("preferences.json")) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(Preferences::default());
            }
            Err(error) => return Err(error),
        };
        serde_json::from_str(&read_text(&mut file, 4096)?).context("Cannot read model preferences")
    }

    pub fn save_preferences(&self, preferences: &Preferences) -> Result<()> {
        write::replace(
            &self.dir,
            Path::new("preferences.json"),
            &serde_json::to_vec(preferences)?,
        )
    }

    pub fn create_chat(&self) -> Result<Chat> {
        let mut chat = Chat {
            model: self.preferences()?.default_model.id().into(),
            ..Chat::default()
        };
        self.save(&mut chat)?;
        Ok(chat)
    }

    pub fn ui_settings(&self) -> Result<UiSettings> {
        let mut file = match open_regular(&self.dir, Path::new("ui.json")) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(UiSettings::default());
            }
            Err(error) => return Err(error),
        };
        let settings: UiSettings = serde_json::from_str(&read_text(&mut file, 4096)?)?;
        settings.validated()
    }

    pub fn save_ui_settings(&self, settings: UiSettings) -> Result<()> {
        let settings = settings.validated()?;
        write::replace(
            &self.dir,
            Path::new("ui.json"),
            &serde_json::to_vec(&settings)?,
        )
    }

    pub fn select(&self, id: Uuid) -> Result<()> {
        write::replace(&self.dir, Path::new("active"), id.to_string().as_bytes())
    }

    pub fn load(&self, id: Uuid) -> Result<Chat> {
        let text = read_text(
            &mut open_regular(&self.chats, Path::new(&format!("{id}.json")))?,
            MAX_CHAT_SIZE,
        )?;
        let chat: Chat = serde_json::from_str(&text).context("Cannot read saved chat")?;
        ensure!(
            chat.version == 1,
            "Unsupported chat version: {}",
            chat.version
        );
        ensure!(chat.id == id, "Saved chat ID does not match its file name");
        Ok(chat)
    }

    pub fn resume(&self, id: Uuid) -> Result<Chat> {
        let mut chat = self.load(id)?;
        if chat.status == Status::Running {
            chat.usage_complete = false;
            chat.interrupt("The last run was interrupted. File operations were not repeated.");
            self.save(&mut chat)?;
        }
        Ok(chat)
    }

    pub fn list(&self) -> Result<Vec<ChatSummary>> {
        let mut summaries = Vec::new();
        for entry in self.chats.entries()? {
            let name = entry?.file_name();
            let path = Path::new(&name);
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            // A damaged chat stays visible and reports an error when selected.
            let summary = match self.load(id) {
                Ok(chat) => ChatSummary::from(&chat),
                Err(_) => ChatSummary {
                    id,
                    title: "Unreadable chat".into(),
                    updated_at: 0,
                    archived: false,
                    status: Status::Idle,
                },
            };
            summaries.push(summary);
        }
        summaries.sort_by_key(|chat| std::cmp::Reverse(chat.updated_at));
        Ok(summaries)
    }

    pub fn restore(&self) -> Result<Chat> {
        match open_regular(&self.dir, Path::new("active")) {
            Ok(mut file) => {
                let id = Uuid::parse_str(read_text(&mut file, 128)?.trim())
                    .context("Invalid active chat ID")?;
                self.resume(id)
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                let chat = self.create_chat()?;
                self.select(chat.id)?;
                Ok(chat)
            }
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone)]
pub struct ChatSummary {
    pub id: Uuid,
    pub title: String,
    pub updated_at: u64,
    pub archived: bool,
    pub status: Status,
}

impl From<&Chat> for ChatSummary {
    fn from(chat: &Chat) -> Self {
        Self {
            id: chat.id,
            title: chat.title.clone(),
            updated_at: chat.updated_at,
            archived: chat.archived,
            status: chat.status,
        }
    }
}
