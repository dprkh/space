use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model::{Model, Usage};

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

pub(crate) struct CachePlan {
    pub existing: Option<CachedPrefix>,
    pub candidate: Option<Prefix>,
    pub obsolete: Vec<CachedPrefix>,
    pub decision: Decision,
}

#[derive(Default, Serialize)]
pub(crate) struct Decision {
    reason: &'static str,
    recent_requests: Option<usize>,
    candidate_prefix: Option<Prefix>,
    candidate_tokens: Option<u64>,
    uncached_input: Option<u64>,
    implicit_cached_estimate: Option<u64>,
    miss_rate_estimate: Option<f64>,
    predicted_savings_usd: Option<f64>,
    predicted_storage_cost_usd: Option<f64>,
}

impl CacheState {
    pub(crate) fn plan(&mut self, model: Model, request: &Value, timestamp: u64) -> CachePlan {
        let mut obsolete = Vec::new();
        self.entries.retain(|cache| {
            let expired = cache.expires_at <= timestamp.saturating_add(30_000);
            let invalid = cache.model == model && !cache.prefix.matches(request);
            if invalid && !expired {
                obsolete.push(cache.clone());
            }
            !expired && !invalid
        });
        let existing = self
            .entries
            .iter()
            .find(|cache| cache.model == model)
            .cloned();
        let mut decision = Decision {
            reason: "no_eligible_prefix",
            ..Decision::default()
        };
        let candidate = self
            .candidate(model, request, existing.as_ref(), timestamp, &mut decision)
            .map(|entry| entry.prefix);
        CachePlan {
            existing,
            candidate,
            obsolete,
            decision,
        }
    }

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
        decision: &mut Decision,
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
        decision.recent_requests = Some(recent.len());
        let latest = *recent.iter().rev().find(|entry| {
            entry.input >= minimum
                && entry.prefix.contents < contents
                && entry.prefix.matches(request)
        })?;
        decision.candidate_prefix = Some(latest.prefix.clone());
        decision.candidate_tokens = Some(latest.input);
        let existing_tokens = existing.map_or(0, |cache| cache.tokens);
        if existing.is_some_and(|cache| cache.prefix.contents >= latest.prefix.contents) {
            decision.reason = "existing_covers_candidate";
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
        decision.uncached_input = Some(tail);
        decision.implicit_cached_estimate = Some(hits);
        decision.miss_rate_estimate = Some(miss_rate);
        decision.predicted_savings_usd = Some(saved / 1_000_000.);
        decision.predicted_storage_cost_usd =
            Some(latest.input as f64 * prices.storage / 1_000_000.);
        let profitable = saved > latest.input as f64 * prices.storage;
        decision.reason = if profitable {
            "profitable"
        } else {
            "not_profitable"
        };
        // Compare savings with the complete storage allocation, including replacements.
        profitable.then(|| latest.clone())
    }
}
