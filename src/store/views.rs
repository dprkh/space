use super::*;

impl Store {
    pub(crate) fn workspace_view(&self) -> Result<Option<String>> {
        read_view(&self.dir, Path::new("view.json"))
    }

    pub(crate) fn save_workspace_view(&self, json: &str) -> Result<()> {
        save_view(&self.dir, Path::new("view.json"), json)
    }

    pub(crate) fn chat_view(&self, id: Uuid) -> Result<Option<String>> {
        read_view(&self.chats, Path::new(&format!("{id}.view.json")))
    }

    pub(crate) fn save_chat_view(&self, id: Uuid, json: &str) -> Result<()> {
        match open_regular(&self.chats, Path::new(&format!("{id}.json"))) {
            Ok(_) => save_view(&self.chats, Path::new(&format!("{id}.view.json")), json),
            Err(error) if missing(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn read_view(dir: &Dir, path: &Path) -> Result<Option<String>> {
    match open_regular(dir, path) {
        Ok(mut file) => read_text(&mut file, MAX_CHAT_SIZE).map(Some),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn save_view(dir: &Dir, path: &Path, json: &str) -> Result<()> {
    ensure!(
        json.len() as u64 <= MAX_CHAT_SIZE,
        "Saved view exceeds the 64 MiB storage limit"
    );
    write::replace(dir, path, json.as_bytes())
}

fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
