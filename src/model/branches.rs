use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::{Chat, Content, Model, Role, Status, ToolChange};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Branches {
    at: BTreeMap<usize, Versions>,
    // Store inactive tails by ID so nested edits do not nest the saved JSON.
    saved: BTreeMap<Uuid, Branch>,
}

impl Branches {
    fn truncate(&mut self, content_index: usize) {
        self.at.retain(|&index, _| index < content_index);
        let mut pending: Vec<_> = self
            .at
            .values()
            .flat_map(|versions| versions.ids.iter().copied())
            .collect();
        let mut retained = HashSet::new();
        while let Some(id) = pending.pop() {
            if !retained.insert(id) {
                continue;
            }
            if let Some(branch) = self.saved.get(&id) {
                pending.extend(
                    branch
                        .versions
                        .values()
                        .flat_map(|versions| versions.ids.iter().copied()),
                );
            }
        }
        self.saved.retain(|id, _| retained.contains(id));
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Versions {
    active: Uuid,
    ids: Vec<Uuid>,
}

impl Default for Versions {
    fn default() -> Self {
        let active = Uuid::new_v4();
        Self {
            active,
            ids: vec![active],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Branch {
    contents: Vec<Content>,
    response_models: BTreeMap<usize, Model>,
    file_changes: Vec<ToolChange>,
    versions: BTreeMap<usize, Versions>,
    context_tokens: Option<u64>,
    context_model: Model,
    partial_model: Model,
    partial: String,
    partial_thoughts: String,
    status: Status,
    notice: Option<String>,
    cache_notice: Option<String>,
}

impl Chat {
    pub fn delete_message(&mut self, content_index: usize) -> Result<()> {
        ensure!(
            self.status != Status::Running,
            "Stop this chat before deleting a message"
        );
        let message = self
            .contents
            .get(content_index)
            .is_some_and(|content| content.is_user_message() || content.role == Role::Model);
        let partial = content_index == self.contents.len()
            && (!self.partial.is_empty() || !self.partial_thoughts.is_empty());
        ensure!(message || partial, "Select a message to delete");

        self.contents.truncate(content_index);
        self.response_models
            .retain(|&index, _| index < content_index);
        self.file_changes
            .retain(|change| change.content_index < content_index);
        self.branches.truncate(content_index);
        self.partial.clear();
        self.partial_thoughts.clear();
        self.status = Status::Idle;
        self.notice = None;
        self.cache_notice = None;
        self.context_tokens = None;
        self.record_activity();
        Ok(())
    }

    pub fn active_branch(&self) -> Option<Uuid> {
        self.branches
            .at
            .last_key_value()
            .map(|(_, versions)| versions.active)
    }

    pub fn message_versions(&self, content_index: usize) -> Option<(usize, usize)> {
        let versions = self.branches.at.get(&content_index)?;
        let selected = versions.ids.iter().position(|id| *id == versions.active)?;
        Some((selected, versions.ids.len()))
    }

    pub fn edit_message(&mut self, content_index: usize, text: &str) -> Result<()> {
        ensure!(
            self.status != Status::Running,
            "This chat is already running"
        );
        let mut message = self
            .contents
            .get(content_index)
            .filter(|content| content.is_user_message())
            .cloned()
            .context("Select a user message to edit")?;
        message.parts.retain(|part| part.get("text").is_none());
        if !text.is_empty() {
            message.parts.insert(0, json!({"text": text}));
        }
        ensure!(!message.parts.is_empty(), "The message is empty");

        // Older builds must reject this file instead of discarding saved branches.
        self.version = 2;
        let mut versions = self.branches.at.remove(&content_index).unwrap_or_default();
        let previous = self.take_branch(content_index);
        self.branches.saved.insert(versions.active, previous);
        versions.active = Uuid::new_v4();
        versions.ids.push(versions.active);
        self.branches.at.insert(content_index, versions);
        self.contents.push(message);
        self.context_tokens = None;
        self.begin_run();
        Ok(())
    }

    pub fn select_message_version(&mut self, content_index: usize, version: usize) -> Result<()> {
        ensure!(
            self.status != Status::Running,
            "Stop this chat before changing branches"
        );
        ensure!(
            self.contents
                .get(content_index)
                .is_some_and(Content::is_user_message),
            "Select a user message version"
        );
        let versions = self
            .branches
            .at
            .get(&content_index)
            .context("This message has no other versions")?;
        let id = *versions
            .ids
            .get(version)
            .context("Unknown message version")?;
        if id == versions.active {
            return Ok(());
        }
        let branch = self
            .branches
            .saved
            .remove(&id)
            .context("The saved branch is missing")?;
        let mut versions = self.branches.at.remove(&content_index).unwrap();
        let previous = self.take_branch(content_index);
        self.branches.saved.insert(versions.active, previous);
        versions.active = id;
        self.restore_branch(branch);
        self.branches.at.insert(content_index, versions);
        self.record_activity();
        Ok(())
    }

    fn take_branch(&mut self, content_index: usize) -> Branch {
        let changes = self
            .file_changes
            .partition_point(|change| change.content_index < content_index);
        Branch {
            contents: self.contents.split_off(content_index),
            response_models: self.response_models.split_off(&content_index),
            file_changes: self.file_changes.split_off(changes),
            versions: self.branches.at.split_off(&(content_index + 1)),
            context_tokens: self.context_tokens,
            context_model: self.context_model,
            partial_model: self.partial_model,
            partial: std::mem::take(&mut self.partial),
            partial_thoughts: std::mem::take(&mut self.partial_thoughts),
            status: self.status,
            notice: self.notice.take(),
            cache_notice: self.cache_notice.take(),
        }
    }

    fn restore_branch(&mut self, branch: Branch) {
        self.contents.extend(branch.contents);
        self.response_models.extend(branch.response_models);
        self.file_changes.extend(branch.file_changes);
        self.branches.at.extend(branch.versions);
        self.context_tokens = branch.context_tokens;
        self.context_model = branch.context_model;
        self.partial_model = branch.partial_model;
        self.partial = branch.partial;
        self.partial_thoughts = branch.partial_thoughts;
        self.status = branch.status;
        self.notice = branch.notice;
        self.cache_notice = branch.cache_notice;
    }
}
