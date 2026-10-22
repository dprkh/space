use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const MODEL: &str = "gemini-4-argon";
pub const CONTEXT_WINDOW_TOKENS: u64 = 1_048_576;
pub const SYSTEM: &str = "";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Model {
    #[default]
    #[serde(rename = "gemini-4-argon")]
    Argon,
    #[serde(rename = "gemini-3.8-flash")]
    Flash38,
}

pub struct Prices {
    pub input: f64,
    pub cached: f64,
    pub output: f64,
    pub storage: f64,
}

impl Model {
    pub const ALL: [Self; 2] = [Self::Argon, Self::Flash38];

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.id() == id)
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Argon => MODEL,
            Self::Flash38 => "gemini-3.8-flash",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Argon => "Gemini 4 Argon",
            Self::Flash38 => "Gemini 3.8 Flash",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Argon => "GEMINI 4 ARGON",
            Self::Flash38 => "GEMINI 3.8 FLASH",
        }
    }

    pub fn explicit_cache_minimum(self) -> Option<u64> {
        match self {
            Self::Argon => None,
            Self::Flash38 => Some(4_096),
        }
    }

    pub fn context_tokens(self, usage: Usage) -> u64 {
        match self {
            Self::Argon => usage.context_tokens(),
            Self::Flash38 => usage.input,
        }
    }

    pub fn prices(self, timestamp: u64) -> Prices {
        // Standard rates: https://ai.google.dev/gemini-api/docs/pricing
        let (input, cached, output, storage, end) = match self {
            Self::Argon => (2., 0.10, 10., 0., 1_796_083_200_000),
            Self::Flash38 => (0.75, 0.075, 3.75, 0.50, 1_798_761_600_000),
        };
        let multiplier = if timestamp < end { 1. } else { 2. };
        Prices {
            input: input * multiplier,
            cached: cached * multiplier,
            output: output * multiplier,
            storage: storage * multiplier,
        }
    }
}

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

    pub fn estimated_cost_usd(self, model: Model, timestamp: u64) -> f64 {
        let prices = model.prices(timestamp);
        let cached = self.cached.min(self.input);
        ((self.input - cached) as f64 * prices.input
            + cached as f64 * prices.cached
            + (self.output as f64 + self.thinking as f64) * prices.output)
            / 1_000_000.
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
    #[serde(default)]
    pub title_is_custom: bool,
    #[serde(default)]
    pub archived: bool,
    pub updated_at: u64,
    pub model: String,
    #[serde(default)]
    pub response_models: BTreeMap<usize, Model>,
    #[serde(default)]
    pub partial_model: Model,
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
    pub context_model: Model,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub cache_storage_cost_usd: f64,
    #[serde(default)]
    pub cache: crate::cache::CacheState,
    #[serde(default)]
    pub cache_notice: Option<String>,
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
            title_is_custom: false,
            archived: false,
            updated_at: now(),
            model: MODEL.into(),
            response_models: BTreeMap::new(),
            partial_model: Model::default(),
            system: SYSTEM.into(),
            tools: crate::project::declarations(),
            contents: Vec::new(),
            file_changes: Vec::new(),
            usage: Usage::default(),
            context_tokens: None,
            context_model: Model::default(),
            cost_usd: Some(0.),
            cache_storage_cost_usd: 0.,
            cache: crate::cache::CacheState::default(),
            cache_notice: None,
            usage_complete: true,
            status: Status::Idle,
            notice: None,
            partial: String::new(),
            partial_thoughts: String::new(),
        }
    }
}

impl Chat {
    pub fn token_cost_usd(&self) -> f64 {
        self.cost_usd
            .unwrap_or_else(|| self.usage.estimated_cost_usd(Model::Argon, self.updated_at))
    }

    pub fn estimated_cost_usd(&self) -> f64 {
        self.token_cost_usd() + self.cache_storage_cost_usd
    }

    pub fn response_model(&self, content_index: usize) -> Model {
        if content_index == self.contents.len() {
            self.partial_model
        } else {
            self.response_models
                .get(&content_index)
                .copied()
                .unwrap_or_default()
        }
    }

    pub fn record_usage(&mut self, model: Model, usage: Usage, timestamp: u64) {
        self.context_tokens = Some(model.context_tokens(usage));
        self.context_model = model;
        self.cost_usd = Some(self.token_cost_usd() + usage.estimated_cost_usd(model, timestamp));
        self.usage += usage;
    }

    pub fn record_activity(&mut self) {
        self.cost_usd = Some(self.token_cost_usd());
        self.updated_at = now();
    }

    pub fn push_user(&mut self, text: &str) {
        self.record_activity();
        if self.contents.is_empty() && !self.title_is_custom {
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
        self.cache_notice = None;
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
