use std::time::Instant;

use anyhow::Result;
use serde_json::json;

use crate::{
    cache::CachedPrefix,
    gemini::{Generation, GenerationUpdate, error_kind},
    metrics::{Context, Metrics},
    model::{Model, Usage, now},
};

pub(super) struct Attempt<'a> {
    metrics: &'a Metrics,
    context: Context,
    number: usize,
    started: Instant,
    first_text_ms: Option<f64>,
    first_thought_ms: Option<f64>,
    http_status: Option<u16>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    explicit_tokens: u64,
}

impl<'a> Attempt<'a> {
    pub(super) fn start(
        metrics: &'a Metrics,
        context: Context,
        number: usize,
        model: Model,
        cache: Option<&CachedPrefix>,
    ) -> Self {
        metrics.record(
            "request_started",
            Some(context),
            json!({
                "attempt": number,
                "model": model,
                "cache": cache,
                "rates": model.prices(now()),
            }),
        );
        Self {
            metrics,
            context,
            number,
            started: Instant::now(),
            first_text_ms: None,
            first_thought_ms: None,
            http_status: None,
            finish_reason: None,
            usage: None,
            explicit_tokens: cache.map_or(0, |cache| cache.tokens),
        }
    }

    pub(super) fn observe(&mut self, update: &GenerationUpdate) {
        match update {
            GenerationUpdate::ResponseStatus(status) => self.http_status = Some(*status),
            GenerationUpdate::FinishReason(reason) => self.finish_reason = Some(reason.clone()),
            GenerationUpdate::Usage(usage) => self.usage = Some(*usage),
            GenerationUpdate::Text(_) if self.first_text_ms.is_none() => {
                self.first_text_ms = Some(self.started.elapsed().as_secs_f64() * 1_000.);
            }
            GenerationUpdate::Thought(_) if self.first_thought_ms.is_none() => {
                self.first_thought_ms = Some(self.started.elapsed().as_secs_f64() * 1_000.);
            }
            _ => {}
        }
    }

    pub(super) fn finish(self, result: &Result<Generation>, cancelled: bool) {
        let duration_ms = self.started.elapsed().as_secs_f64() * 1_000.;
        let outcome = if cancelled {
            "cancelled"
        } else {
            result
                .as_ref()
                .map_or_else(|error| error_kind(error), |_| "success")
        };
        let implicit_cached_estimate = self.usage.map(|usage| {
            usage
                .cached
                .saturating_sub(self.explicit_tokens)
                .min(usage.input.saturating_sub(self.explicit_tokens))
        });
        self.metrics.record(
            "request_finished",
            Some(self.context),
            json!({
                "attempt": self.number,
                "outcome": outcome,
                "http_status": self.http_status,
                "finish_reason": self.finish_reason,
                "usage": self.usage,
                "explicit_prefix_tokens": self.explicit_tokens,
                "implicit_cached_estimate": implicit_cached_estimate,
                "duration_ms": duration_ms,
                "first_text_ms": self.first_text_ms,
                "first_thought_ms": self.first_thought_ms,
            }),
        );
    }
}
