use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const MODEL: &str = "gemini-4-argon";
pub const CONTEXT_WINDOW_TOKENS: u64 = 1_048_576;
pub const SYSTEM: &str = "";

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Content {
    pub role: Role,
    // Keep opaque signatures and future API fields intact.
    pub parts: Vec<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Model,
}

impl Content {
    pub fn text(role: Role, text: &str) -> Self {
        Self {
            role,
            parts: vec![json!({"text": text})],
        }
    }

    pub fn visible_text(&self) -> String {
        self.parts
            .iter()
            .filter(|part| part["thought"] != true)
            .filter_map(|part| part["text"].as_str())
            .collect()
    }

    pub fn thought_text(&self) -> String {
        self.parts
            .iter()
            .filter(|part| part["thought"] == true)
            .filter_map(|part| part["text"].as_str())
            .collect()
    }

    pub fn calls(&self) -> Vec<Value> {
        self.parts
            .iter()
            .filter_map(|part| part.get("functionCall").cloned())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, rename = "promptTokenCount")]
    pub input: u64,
    #[serde(default, rename = "cachedContentTokenCount")]
    pub cached: u64,
    #[serde(default, rename = "candidatesTokenCount")]
    pub output: u64,
    #[serde(default, rename = "thoughtsTokenCount")]
    pub thinking: u64,
    #[serde(default, rename = "totalTokenCount")]
    pub total: u64,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.cached = self.cached.saturating_add(other.cached);
        self.output = self.output.saturating_add(other.output);
        self.thinking = self.thinking.saturating_add(other.thinking);
        self.total = self.total.saturating_add(other.total);
    }
}

impl Usage {
    pub fn context_tokens(self) -> u64 {
        self.total.max(
            self.input
                .saturating_add(self.output)
                .saturating_add(self.thinking),
        )
    }

    pub fn estimated_cost_usd(self, timestamp: u64) -> f64 {
        // Standard Argon pricing: https://ai.google.dev/gemini-api/docs/pricing
        // The introductory rates end on December 1, 2026 (UTC).
        let multiplier = if timestamp < 1_796_083_200_000 {
            1.
        } else {
            2.
        };
        let cached = self.cached.min(self.input);
        let cost = (self.input - cached) as f64 * 2.
            + cached as f64 * 0.10
            + (self.output as f64 + self.thinking as f64) * 10.;
        cost * multiplier / 1_000_000.
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Running,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub patch: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolChange {
    pub content_index: usize,
    pub part_index: usize,
    pub change: FileChange,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chat {
    pub version: u32,
    pub id: Uuid,
    pub title: String,
    pub updated_at: u64,
    pub model: String,
    pub system: String,
    pub tools: Value,
    pub contents: Vec<Content>,
    #[serde(default)]
    pub file_changes: Vec<ToolChange>,
    pub usage: Usage,
    // Keep the latest request separate from cumulative usage.
    #[serde(default)]
    pub context_tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub usage_complete: bool,
    pub status: Status,
    pub notice: Option<String>,
    #[serde(default)]
    pub partial: String,
    #[serde(default)]
    pub partial_thoughts: String,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            version: 1,
            id: Uuid::new_v4(),
            title: "New chat".into(),
            updated_at: now(),
            model: MODEL.into(),
            system: SYSTEM.into(),
            tools: crate::project::declarations(),
            contents: Vec::new(),
            file_changes: Vec::new(),
            usage: Usage::default(),
            context_tokens: None,
            cost_usd: Some(0.),
            usage_complete: true,
            status: Status::Idle,
            notice: None,
            partial: String::new(),
            partial_thoughts: String::new(),
        }
    }
}

impl Chat {
    pub fn estimated_cost_usd(&self) -> f64 {
        self.cost_usd
            .unwrap_or_else(|| self.usage.estimated_cost_usd(self.updated_at))
    }

    pub fn record_usage(&mut self, usage: Usage, timestamp: u64) {
        self.context_tokens = Some(usage.context_tokens());
        self.cost_usd = Some(self.estimated_cost_usd() + usage.estimated_cost_usd(timestamp));
        self.usage += usage;
    }

    pub fn push_user(&mut self, text: &str) {
        if self.contents.is_empty() {
            self.title = text
                .lines()
                .next()
                .unwrap_or(text)
                .chars()
                .take(48)
                .collect();
        }
        self.contents.push(Content::text(Role::User, text));
        self.status = Status::Running;
        self.notice = None;
        self.partial.clear();
        self.partial_thoughts.clear();
    }

    // Recovery completes the protocol without repeating a possibly completed write.
    pub fn interrupt(&mut self, message: &str) {
        if let Some(index) = self.contents.iter().rposition(|c| c.role == Role::Model) {
            let calls = self.contents[index].calls();
            if !calls.is_empty() {
                let responses = self.contents.get(index + 1).map_or(0, |c| c.parts.len());
                let missing: Vec<_> = calls.iter().skip(responses)
                    .map(|call| function_response(call, json!({
                        "ok": false,
                        "error": "Run interrupted. This operation was not replayed. Its outcome may be unknown; read the file before proceeding."
                    }))).collect();
                if !missing.is_empty() {
                    if self.contents.len() == index + 1 {
                        self.contents.push(Content {
                            role: Role::User,
                            parts: missing,
                        });
                    } else {
                        self.contents[index + 1].parts.extend(missing);
                    }
                }
            }
        }
        self.status = Status::Interrupted;
        self.notice = Some(message.into());
    }
}

pub fn function_response(call: &Value, result: Value) -> Value {
    let mut response = json!({"name": call["name"], "response": result});
    if let Some(id) = call.get("id") {
        response["id"] = id.clone();
    }
    json!({"functionResponse": response})
}
