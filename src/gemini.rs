use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use crate::model::{Chat, Content, Role, Usage};

pub struct Gemini {
    client: reqwest::Client,
    endpoint: String,
    key: String,
}

pub struct Generation {
    pub content: Content,
    pub usage: Usage,
    pub finish_reason: String,
}

impl Gemini {
    pub fn new(key: String) -> Result<Self> {
        Self::with_endpoint(
            key,
            "https://generativelanguage.googleapis.com/v1beta/models".into(),
        )
    }

    pub(crate) fn with_endpoint(key: String, endpoint: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut value = HeaderValue::from_str(&key).context("Invalid GEMINI_API_KEY")?;
        value.set_sensitive(true);
        headers.insert("x-goog-api-key", value);
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(180))
            .build()?;
        Ok(Self {
            client,
            endpoint,
            key,
        })
    }

    pub fn request(chat: &Chat) -> Value {
        json!({
            "systemInstruction": {"parts": [{"text": chat.system}]},
            "tools": chat.tools,
            "contents": chat.contents,
            "generationConfig": {
                "candidateCount": 1,
                "maxOutputTokens": 16_384,
                "thinkingConfig": {"thinkingLevel": "HIGH", "includeThoughts": true}
            }
        })
    }

    pub async fn generate(
        &self,
        chat: &Chat,
        mut on_text: impl FnMut(&str),
        mut on_thought: impl FnMut(&str),
        mut on_usage: impl FnMut(Usage),
    ) -> Result<Generation> {
        ensure!(
            !self.key.trim().is_empty(),
            "Set GEMINI_API_KEY before sending a message"
        );
        ensure!(
            chat.model == crate::model::MODEL,
            "Unsupported saved model: {}",
            chat.model
        );
        let response = self
            .client
            .post(format!(
                "{}/{}:streamGenerateContent?alt=sse",
                self.endpoint, chat.model
            ))
            .json(&Self::request(chat))
            .send()
            .await
            .context("Cannot connect to Google")?;
        let status = response.status();
        if !status.is_success() {
            let body: Value = response.json().await.unwrap_or_default();
            let message = body["error"]["message"]
                .as_str()
                .unwrap_or("Request failed");
            bail!(
                "Google API {status}: {}",
                message.replace(&self.key, "[redacted]")
            );
        }
        let mut events = response.bytes_stream().eventsource();
        let mut generation = Generation {
            content: Content {
                role: Role::Model,
                parts: Vec::new(),
            },
            usage: Usage::default(),
            finish_reason: String::new(),
        };
        while let Some(event) = events.next().await {
            let event = event.context("Response stream interrupted")?;
            if event.data == "[DONE]" {
                break;
            }
            ensure!(
                event.data.len() <= 8 * 1024 * 1024,
                "Response event is too large"
            );
            let data: Value =
                serde_json::from_str(&event.data).context("Invalid JSON in response stream")?;
            if let Some(usage) = data.get("usageMetadata") {
                generation.usage = serde_json::from_value(usage.clone())?;
                on_usage(generation.usage);
            }
            if let Some(error) = data.get("error") {
                bail!(
                    "Google stream error: {}",
                    error["message"]
                        .as_str()
                        .unwrap_or("Unknown error")
                        .replace(&self.key, "[redacted]")
                );
            }
            if let Some(reason) = data["promptFeedback"]["blockReason"].as_str() {
                bail!("Google blocked the request: {reason}");
            }
            if let Some(candidate) = data["candidates"].as_array().and_then(|c| c.first()) {
                if let Some(parts) = candidate["content"]["parts"].as_array() {
                    for part in parts {
                        if let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) {
                            if part["thought"] == true {
                                on_thought(text);
                            } else {
                                on_text(text);
                            }
                        }
                        // Keep even empty text parts: they can contain a signature.
                        generation.content.parts.push(part.clone());
                    }
                }
                if let Some(reason) = candidate["finishReason"].as_str() {
                    generation.finish_reason = reason.into();
                }
            }
        }
        ensure!(
            !generation.finish_reason.is_empty(),
            "Response stream ended before completion"
        );
        ensure!(
            !generation.content.parts.is_empty(),
            "Google returned an empty response ({})",
            generation.finish_reason
        );
        Ok(generation)
    }
}
