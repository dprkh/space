use super::{MAX_ENTRIES, MAX_FILE_SIZE, Project, write};
use crate::{
    fs::{open_regular, read_text},
    model::FileChange,
    pdf::Pdf,
};
use anyhow::{Context, Result, bail, ensure};
use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use chrono::{Local, SecondsFormat};
use serde::Deserialize;
use serde_json::{Value, json};
use similar::TextDiff;
use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
    time::Duration,
};

pub struct ToolOutput {
    pub response: Value,
    pub pdf: Option<Pdf>,
    pub change: Option<FileChange>,
}

impl ToolOutput {
    fn success(result: Value, change: Option<FileChange>) -> Self {
        Self {
            response: json!({"ok": true, "result": result}),
            pdf: None,
            change,
        }
    }
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
    pub fn execute(&self, call: &Value) -> ToolOutput {
        self.execute_inner(call).unwrap_or_else(|error| ToolOutput {
            response: json!({"ok": false, "error": format!("{error:#}")}),
            pdf: None,
            change: None,
        })
    }

    fn execute_inner(&self, call: &Value) -> Result<ToolOutput> {
        let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
        match call["name"].as_str().context("Missing tool name")? {
            "current_time" => Ok(ToolOutput::success(
                json!({ "local_time": Local::now().to_rfc3339_opts(SecondsFormat::Secs, false) }),
                None,
            )),
            "read" => {
                let args: PathArgs = serde_json::from_value(args)?;
                self.read(&args.path)
            }
            "create" => self
                .create(serde_json::from_value(args)?)
                .map(|(result, change)| ToolOutput::success(result, change)),
            "edit" => self
                .edit(serde_json::from_value(args)?)
                .map(|(result, change)| ToolOutput::success(result, change)),
            "delete" => self
                .delete(serde_json::from_value(args)?)
                .map(|(result, change)| ToolOutput::success(result, change)),
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
        let (dir, name) = self.parent(Path::new(&args.path))?;
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
        let (dir, name) = self.parent(Path::new(&args.path))?;
        let text = read_text(&mut open_regular(&dir, &name)?, MAX_FILE_SIZE)?;
        dir.remove_file(&name)?;
        dir.try_clone()?.into_std_file().sync_all()?;
        let change = file_change(&args.path, Some(&text), None);
        Ok((
            json!({"path": args.path, "bytes": text.len()}),
            Some(change),
        ))
    }

    fn read(&self, path: &str) -> Result<ToolOutput> {
        let parts = Self::components(Path::new(path))?;
        let directory = if parts.is_empty() {
            Some(self.root.try_clone()?)
        } else {
            let (parent, name) = self.parent(Path::new(path))?;
            parent.open_dir_nofollow(&name).ok()
        };
        if let Some(dir) = directory {
            return read_directory(dir);
        }
        self.read_document(path)
    }

    fn read_document(&self, path: &str) -> Result<ToolOutput> {
        let (parent, name) = self.parent(Path::new(path))?;
        let mut reader = BufReader::new(open_regular(&parent, &name)?);
        if Pdf::has_header(reader.fill_buf()?) {
            let pdf = Pdf::read(name.to_string_lossy().into_owned(), reader)?;
            return Ok(ToolOutput {
                response: json!({"ok": true, "result": {"path": path, "mime_type": "application/pdf"}}),
                pdf: Some(pdf),
                change: None,
            });
        }
        let mut bytes = Vec::new();
        reader.take(MAX_FILE_SIZE + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_FILE_SIZE, "File exceeds 1 MiB");
        let text = String::from_utf8(bytes).context("File is not valid UTF-8")?;
        Ok(ToolOutput::success(json!({"text": text}), None))
    }
}

fn read_directory(dir: Dir) -> Result<ToolOutput> {
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
    Ok(ToolOutput::success(
        json!({"entries": entries, "truncated": truncated}),
        None,
    ))
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

pub fn declarations() -> Value {
    json!([{"functionDeclarations": [
        {"name": "current_time", "description": "Get the current date and time in the local timezone."},
        {"name": "read", "description": "Read a UTF-8 text file, read a PDF with its text and images, or list a folder. Use paths relative to the project. PDFs are sent directly to the model.",
         "parameters": {"type":"OBJECT", "properties":{"path":{"type":"STRING"}},"required":["path"]}},
        {"name": "create", "description": "Create a new file and any missing parent folders.",
         "parameters": {"type":"OBJECT", "properties":{"path":{"type":"STRING"},"content":{"type":"STRING"}},"required":["path","content"]}},
        {"name": "edit", "description": "Replace old_text with new_text. old_text must be nonempty and occur exactly once.",
         "parameters": {"type":"OBJECT", "properties":{"path":{"type":"STRING"},"old_text":{"type":"STRING"},"new_text":{"type":"STRING"}},"required":["path","old_text","new_text"]}},
        {"name": "delete", "description": "Delete one regular UTF-8 file. Folders cannot be deleted. Use a project-relative path.",
         "parameters": {"type":"OBJECT", "properties":{"path":{"type":"STRING"}},"required":["path"]}}
    ]}])
}
