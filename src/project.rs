mod write;

use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, File, MetadataExt, OpenOptions, OpenOptionsExt},
};
use chrono::{Local, SecondsFormat};
use serde::Deserialize;
use serde_json::{Value, json};
use similar::TextDiff;

use crate::model::FileChange;

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    path: String,
    content: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    old_text: String,
    new_text: String,
}

impl Project {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .context("Cannot open the project folder")?;
        let root = Dir::open_ambient_dir(&path, ambient_authority())?;
        Ok(Self { path, root })
    }

    pub fn execute(&self, call: &Value) -> (Value, Option<FileChange>) {
        match self.execute_inner(call) {
            Ok((result, change)) => (json!({"ok": true, "result": result}), change),
            Err(error) => (json!({"ok": false, "error": format!("{error:#}")}), None),
        }
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

    fn execute_inner(&self, call: &Value) -> Result<(Value, Option<FileChange>)> {
        let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
        match call["name"].as_str().context("Missing tool name")? {
            "current_time" => Ok((
                json!({ "local_time": Local::now().to_rfc3339_opts(SecondsFormat::Secs, false) }),
                None,
            )),
            "read" => {
                let args: PathArgs = serde_json::from_value(args)?;
                Ok((self.read(&args.path)?, None))
            }
            "create" => self.create(serde_json::from_value(args)?),
            "edit" => self.edit(serde_json::from_value(args)?),
            "delete" => self.delete(serde_json::from_value(args)?),
            name => bail!("Unknown tool: {name}"),
        }
    }

    fn create(&self, args: CreateArgs) -> Result<(Value, Option<FileChange>)> {
        ensure!(
            args.content.len() as u64 <= MAX_FILE_SIZE,
            "File exceeds 1 MiB"
        );
        let (dir, name) = self.create_parent(&args.path)?;
        write::create(&dir, &name, args.content.as_bytes())?;
        let change = file_change(&args.path, None, Some(&args.content));
        Ok((
            json!({"path": args.path, "bytes": args.content.len()}),
            Some(change),
        ))
    }

    fn edit(&self, args: EditArgs) -> Result<(Value, Option<FileChange>)> {
        ensure!(!args.old_text.is_empty(), "old_text must not be empty");
        let (dir, name) = self.parent(&args.path)?;
        let mut file = open_regular(&dir, &name)?;
        let text = read_text(&mut file, MAX_FILE_SIZE)?;
        ensure!(
            text.match_indices(&args.old_text).take(2).count() == 1,
            "old_text must match exactly once; read the file and use more context"
        );
        let updated = text.replacen(&args.old_text, &args.new_text, 1);
        ensure!(updated.len() as u64 <= MAX_FILE_SIZE, "File exceeds 1 MiB");
        write::replace(
            &dir,
            &name,
            updated.as_bytes(),
            file.metadata()?.permissions(),
        )?;
        let change = file_change(&args.path, Some(&text), Some(&updated));
        Ok((
            json!({"path": args.path, "bytes": updated.len()}),
            Some(change),
        ))
    }

    fn delete(&self, args: PathArgs) -> Result<(Value, Option<FileChange>)> {
        let (dir, name) = self.parent(&args.path)?;
        let text = read_text(&mut open_regular(&dir, &name)?, MAX_FILE_SIZE)?;
        dir.remove_file(&name)?;
        dir.try_clone()?.into_std_file().sync_all()?;
        let change = file_change(&args.path, Some(&text), None);
        Ok((
            json!({"path": args.path, "bytes": text.len()}),
            Some(change),
        ))
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

    fn parent(&self, path: &str) -> Result<(Dir, PathBuf)> {
        let parts = Self::components(Path::new(path))?;
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

    fn read(&self, path: &str) -> Result<Value> {
        let parts = Self::components(Path::new(path))?;
        let directory = if parts.is_empty() {
            Some(self.root.try_clone()?)
        } else {
            let (parent, name) = self.parent(path)?;
            parent.open_dir_nofollow(&name).ok()
        };
        if let Some(dir) = directory {
            let mut entries = Vec::new();
            for entry in dir.entries()? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.eq_ignore_ascii_case(".space") {
                    continue;
                }
                let kind = entry.file_type()?;
                entries.push(json!({"name": name, "kind": if kind.is_dir() {"directory"} else if kind.is_file() {"file"} else {"link_or_special"}}));
                if entries.len() > MAX_ENTRIES {
                    break;
                }
            }
            let truncated = entries.len() > MAX_ENTRIES;
            entries.truncate(MAX_ENTRIES);
            entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            return Ok(json!({"entries": entries, "truncated": truncated}));
        }
        let (parent, name) = self.parent(path)?;
        Ok(json!({"text": read_text(&mut open_regular(&parent, &name)?, MAX_FILE_SIZE)?}))
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

fn file_change(path: &str, before: Option<&str>, after: Option<&str>) -> FileChange {
    let old_path = before.map_or_else(|| "/dev/null".to_owned(), |_| format!("a/{path}"));
    let new_path = after.map_or_else(|| "/dev/null".to_owned(), |_| format!("b/{path}"));
    let mut patch = TextDiff::configure()
        .timeout(Duration::from_millis(250))
        .diff_lines(before.unwrap_or_default(), after.unwrap_or_default())
        .unified_diff()
        .context_radius(3)
        .header(&old_path, &new_path)
        .to_string();
    if patch.is_empty() {
        patch = format!("--- {old_path}\n+++ {new_path}\n");
    }
    FileChange {
        path: path.to_owned(),
        patch,
    }
}

// Nonblocking open prevents a FIFO from blocking before its type can be checked.
pub(crate) fn open_regular(dir: &Dir, name: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .follow(FollowSymlinks::No);
    let file = dir.open_with(name, &options)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "Only regular files without hard links are allowed"
    );
    Ok(file)
}

pub(crate) fn read_text(file: &mut File, limit: u64) -> Result<String> {
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "File exceeds the size limit ({limit} bytes)"
    );
    String::from_utf8(bytes).context("File is not valid UTF-8")
}

pub fn declarations() -> Value {
    json!([{"functionDeclarations": [
        {"name": "current_time", "description": "Get the current date and time in the local timezone."},
        {"name": "read", "description": "Read a file or list a folder. Use paths relative to the project.",
         "parameters": {"type":"OBJECT", "properties":{"path":{"type":"STRING"}},"required":["path"]}},
        {"name": "create", "description": "Create a new file and any missing parent folders.",
         "parameters": {"type":"OBJECT", "properties":{"path":{"type":"STRING"},"content":{"type":"STRING"}},"required":["path","content"]}},
        {"name": "edit", "description": "Replace old_text with new_text. old_text must be nonempty and occur exactly once.",
         "parameters": {"type":"OBJECT", "properties":{"path":{"type":"STRING"},"old_text":{"type":"STRING"},"new_text":{"type":"STRING"}},"required":["path","old_text","new_text"]}},
        {"name": "delete", "description": "Delete one regular UTF-8 file. Folders cannot be deleted. Use a project-relative path.",
         "parameters": {"type":"OBJECT", "properties":{"path":{"type":"STRING"}},"required":["path"]}}
    ]}])
}
