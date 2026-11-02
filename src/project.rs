mod tools;
mod write;

pub use tools::{ToolOutput, declarations};

use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use cap_fs_ext::DirExt;
use cap_std::{ambient_authority, fs::Dir};
use tokio_util::sync::CancellationToken;

use crate::fs::{open_regular, read_text};

pub const MAX_FILE_SIZE: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 1000;

pub struct Project {
    pub path: PathBuf,
    pub(crate) root: Dir,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub path: PathBuf,
    pub directory: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DirectoryListing {
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
}

#[derive(PartialEq, Eq)]
pub struct FileContent {
    pub text: String,
    pub line_starts: Vec<usize>,
}

impl FileContent {
    fn new(text: String) -> Self {
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(offset, _)| offset + 1))
            .collect();
        Self { text, line_starts }
    }
}

impl Project {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .context("Cannot open the project folder")?;
        let root = Dir::open_ambient_dir(&path, ambient_authority())?;
        Ok(Self { path, root })
    }

    pub fn system_prompt(&self) -> Result<String> {
        let mut prompt = crate::model::SYSTEM.to_owned();
        let mut file = match open_regular(&self.root, Path::new("AGENTS.md")) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(prompt);
            }
            Err(error) => return Err(error.context("Cannot read AGENTS.md")),
        };
        let instructions = read_text(&mut file, MAX_FILE_SIZE).context("Cannot read AGENTS.md")?;
        if !prompt.is_empty() && !instructions.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(&instructions);
        Ok(prompt)
    }

    fn components(path: &Path) -> Result<Vec<&std::ffi::OsStr>> {
        let mut parts = Vec::new();
        ensure!(!path.as_os_str().is_empty(), "Path must not be empty");
        for part in path.components() {
            match part {
                Component::Normal(name) => {
                    ensure!(
                        !name.to_string_lossy().eq_ignore_ascii_case(".space"),
                        ".space is private application storage"
                    );
                    parts.push(name);
                }
                Component::CurDir => {}
                _ => bail!("Use a project-relative path without '..'"),
            }
        }
        Ok(parts)
    }

    fn parent(&self, path: &Path) -> Result<(Dir, PathBuf)> {
        let parts = Self::components(path)?;
        let (name, parents) = parts.split_last().context("A file path is required")?;
        let mut dir = self.root.try_clone()?;
        for part in parents {
            dir = dir
                .open_dir_nofollow(part)
                .context("Cannot open folder; symbolic links are not allowed")?;
        }
        Ok((dir, PathBuf::from(name)))
    }

    fn create_parent(&self, path: &str) -> Result<(Dir, PathBuf)> {
        let parts = Self::components(Path::new(path))?;
        let (name, parents) = parts.split_last().context("A file path is required")?;
        let mut dir = self.root.try_clone()?;
        for part in parents {
            match dir.create_dir(part) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            dir = dir
                .open_dir_nofollow(part)
                .context("Cannot open folder; symbolic links are not allowed")?;
        }
        Ok((dir, PathBuf::from(name)))
    }

    pub fn read_file(&self, path: &Path, cancel: &CancellationToken) -> Result<FileContent> {
        let (parent, name) = self.parent(path)?;
        let mut file = open_regular(&parent, &name)?;
        let mut bytes = Vec::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            ensure!(!cancel.is_cancelled(), "File read cancelled");
            let length = match file.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if length == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..length]);
        }
        ensure!(!cancel.is_cancelled(), "File read cancelled");
        let text = String::from_utf8(bytes).context("File is not valid UTF-8")?;
        Ok(FileContent::new(text))
    }

    pub fn list_directory(&self, path: &Path) -> Result<DirectoryListing> {
        let mut dir = self.root.try_clone()?;
        for component in Self::components(path)? {
            dir = dir
                .open_dir_nofollow(component)
                .context("Cannot open folder; symbolic links are not allowed")?;
        }
        let mut entries = Vec::new();
        for entry in dir.entries()? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().eq_ignore_ascii_case(".space") {
                continue;
            }
            entries.push(DirectoryEntry {
                path: path.join(name),
                directory: entry.file_type()?.is_dir(),
            });
            if entries.len() > MAX_ENTRIES {
                break;
            }
        }
        let truncated = entries.len() > MAX_ENTRIES;
        entries.truncate(MAX_ENTRIES);
        entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(DirectoryListing { entries, truncated })
    }
}
