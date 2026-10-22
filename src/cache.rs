use std::cell::RefCell;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    gemini::Gemini,
    model::{Chat, Model, Usage, now},
    store::Store,
};

const HOUR: u64 = 3_600_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prefix {
    pub contents: usize,
    pub fingerprint: String,
}

impl Prefix {
    fn new(request: &Value, contents: usize) -> Option<Self> {
        let history = request["contents"].as_array()?.get(..contents)?;
        let bytes =
            serde_json::to_vec(&(&request["systemInstruction"], &request["tools"], history))
                .ok()?;
        Some(Self {
            contents,
            fingerprint: format!("{:x}", Sha256::digest(bytes)),
        })
    }

    fn matches(&self, request: &Value) -> bool {
        Self::new(request, self.contents)
            .is_some_and(|prefix| prefix.fingerprint == self.fingerprint)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedPrefix {
    pub name: String,
    pub model: Model,
    pub prefix: Prefix,
    pub tokens: u64,
    pub expires_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    model: Model,
    timestamp: u64,
    prefix: Prefix,
    input: u64,
    uncached_input: u64,
    implicit_cached: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheState {
    pub entries: Vec<CachedPrefix>,
    observations: Vec<Observation>,
}

pub struct Prepared {
    pub request: Value,
    pub cache: CachedPrefix,
}

impl CacheState {
    pub fn observe(
        &mut self,
        model: Model,
        request: &Value,
        usage: Usage,
        explicit_tokens: u64,
        timestamp: u64,
    ) {
        if model.explicit_cache_minimum().is_none() {
            return;
        }
        let Some(prefix) = request["contents"]
            .as_array()
            .and_then(|contents| Prefix::new(request, contents.len()))
        else {
            return;
        };
        self.observations
            .retain(|entry| timestamp.saturating_sub(entry.timestamp) < HOUR);
        // Bound local policy history even during unusually frequent tool rounds.
        if self.observations.len() >= 4_096 {
            self.observations.remove(0);
        }
        self.observations.push(Observation {
            model,
            timestamp,
            prefix,
            input: usage.input,
            uncached_input: usage.input.saturating_sub(explicit_tokens),
            implicit_cached: usage
                .cached
                .saturating_sub(explicit_tokens)
                .min(usage.input.saturating_sub(explicit_tokens)),
        });
    }

    fn candidate(
        &self,
        model: Model,
        request: &Value,
        existing: Option<&CachedPrefix>,
        timestamp: u64,
    ) -> Option<Observation> {
        let minimum = model.explicit_cache_minimum()?;
        let contents = request["contents"].as_array()?.len();
        let recent: Vec<_> = self
            .observations
            .iter()
            .filter(|entry| {
                entry.model == model && timestamp.saturating_sub(entry.timestamp) < HOUR
            })
            .collect();
        let latest = *recent.iter().rev().find(|entry| {
            entry.input >= minimum
                && entry.prefix.contents < contents
                && entry.prefix.matches(request)
        })?;
        let existing_tokens = existing.map_or(0, |cache| cache.tokens);
        if existing.is_some_and(|cache| cache.prefix.contents >= latest.prefix.contents) {
            return None;
        }
        let tail: u64 = recent.iter().map(|entry| entry.uncached_input).sum();
        let hits: u64 = recent.iter().map(|entry| entry.implicit_cached).sum();
        let miss_rate = if tail == 0 {
            1.
        } else {
            1. - hits as f64 / tail as f64
        };
        let prices = model.prices(timestamp);
        let saved = latest.input.saturating_sub(existing_tokens) as f64
            * miss_rate
            * (prices.input - prices.cached)
            * recent.len().max(1) as f64;
        // Compare savings with the complete storage allocation, including replacements.
        (saved > latest.input as f64 * prices.storage).then(|| latest.clone())
    }
}

pub async fn prepare(
    gemini: &Gemini,
    store: &Store,
    state: &RefCell<Chat>,
    model: Model,
    request: &Value,
) -> Result<Option<Prepared>> {
    if model.explicit_cache_minimum().is_none() {
        return Ok(None);
    }
    let timestamp = now();
    let mut obsolete = Vec::new();
    let (existing, candidate) = {
        let mut chat = state.borrow_mut();
        chat.cache.entries.retain(|cache| {
            let expired = cache.expires_at <= timestamp.saturating_add(30_000);
            let invalid = cache.model == model && !cache.prefix.matches(request);
            if invalid && !expired {
                obsolete.push(cache.name.clone());
            }
            !expired && !invalid
        });
        let existing = chat
            .cache
            .entries
            .iter()
            .find(|cache| cache.model == model)
            .cloned();
        let candidate = chat
            .cache
            .candidate(model, request, existing.as_ref(), timestamp);
        (existing, candidate)
    };
    for name in obsolete {
        gemini.delete_cache(name).await;
    }
    let cache = if let Some(candidate) = candidate {
        let body = json!({
            "model": format!("models/{}", model.id()),
            "systemInstruction": request["systemInstruction"],
            "tools": request["tools"],
            "contents": &request["contents"].as_array().unwrap()[..candidate.prefix.contents],
            "ttl": "3600s"
        });
        let created = gemini.create_cache(&body).await?;
        let cache = CachedPrefix {
            name: created.name,
            model,
            prefix: candidate.prefix,
            tokens: created.tokens,
            expires_at: created.expires_at,
        };
        let saved = {
            let mut chat = state.borrow_mut();
            let old = chat.cache.clone();
            let old_cost = chat.cache_storage_cost_usd;
            chat.cache.entries.retain(|entry| entry.model != model);
            chat.cache.entries.push(cache.clone());
            chat.cache_storage_cost_usd +=
                cache.tokens as f64 * model.prices(timestamp).storage / 1_000_000.;
            match store.save(&mut chat) {
                Ok(()) => Ok(()),
                Err(error) => {
                    chat.cache = old;
                    chat.cache_storage_cost_usd = old_cost;
                    Err(error)
                }
            }
        };
        if let Err(error) = saved {
            gemini.delete_cache(cache.name.clone()).await;
            return Err(error);
        }
        if let Some(existing) = existing {
            gemini.delete_cache(existing.name).await;
        }
        cache
    } else if let Some(existing) = existing {
        existing
    } else {
        return Ok(None);
    };
    let contents = request["contents"].as_array().unwrap();
    ensure!(
        cache.prefix.contents < contents.len(),
        "The cache has no remaining input"
    );
    Ok(Some(Prepared {
        request: json!({
            "cachedContent": cache.name,
            "contents": &contents[cache.prefix.contents..],
            "generationConfig": request["generationConfig"]
        }),
        cache,
    }))
}
