use super::Run;
use crate::{
    agent::cache_metrics::delete_cache,
    cache::{CachedPrefix, Prefix},
    gemini::{error_kind, error_status},
    metrics::Context as MetricContext,
    model::now,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub(super) struct Prepared {
    pub(super) request: Value,
    pub(super) cache: CachedPrefix,
}

impl Run<'_> {
    pub(super) async fn prepare_cache(
        &self,
        request: &Value,
        context: MetricContext,
    ) -> Result<Option<Prepared>> {
        if self.model.explicit_cache_minimum().is_none() {
            self.store.metrics().record(
                "cache_decision",
                Some(context),
                json!({
                    "action": "skip", "reason": "unsupported_explicit_cache", "model": self.model,
                    "rates": self.model.prices(now()),
                }),
            );
            return Ok(None);
        }
        let timestamp = now();
        let plan = self
            .state
            .borrow_mut()
            .cache
            .plan(self.model, request, timestamp);
        let action = match (&plan.candidate, &plan.existing) {
            (Some(_), Some(_)) => "replace",
            (Some(_), None) => "create",
            (None, Some(_)) => "reuse",
            (None, None) => "skip",
        };
        self.store.metrics().record(
            "cache_decision",
            Some(context),
            json!({
                "model": self.model,
                "action": action,
                "existing": plan.existing,
                "obsolete": plan.obsolete,
                "selection": plan.decision,
                "rates": self.model.prices(timestamp),
            }),
        );
        for cache in plan.obsolete {
            delete_cache(
                self.gemini,
                self.store,
                cache,
                Some(context),
                "prefix_changed",
            )
            .await;
        }
        let cache = if let Some(candidate) = plan.candidate {
            let cache = self
                .create_cache(request, candidate, timestamp, context)
                .await?;
            if let Some(existing) = plan.existing {
                delete_cache(self.gemini, self.store, existing, Some(context), "replaced").await;
            }
            cache
        } else if let Some(existing) = plan.existing {
            existing
        } else {
            return Ok(None);
        };
        let contents = request["contents"]
            .as_array()
            .context("Missing request contents")?;
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

    async fn create_cache(
        &self,
        request: &Value,
        prefix: Prefix,
        timestamp: u64,
        context: MetricContext,
    ) -> Result<CachedPrefix> {
        let contents = request["contents"]
            .as_array()
            .and_then(|contents| contents.get(..prefix.contents))
            .context("Invalid cache prefix")?;
        let body = json!({
            "model": format!("models/{}", self.model.id()),
            "systemInstruction": request["systemInstruction"],
            "tools": request["tools"],
            "contents": contents,
            "ttl": "3600s"
        });
        let operation = self.store.metrics().operation(
            Some(context),
            "create",
            "policy",
            json!({
                "model": self.model,
                "prefix": prefix,
                "ttl_seconds": 3600,
                "rates": self.model.prices(timestamp),
            }),
        );
        let mut http_status = None;
        let created = match self
            .gemini
            .create_cache(&body, |status| http_status = Some(status))
            .await
        {
            Ok(created) => {
                operation.finish("success", http_status, json!({
                    "cache_id": created.name, "tokens": created.tokens, "expires_at": created.expires_at,
                }));
                created
            }
            Err(error) => {
                operation.finish(
                    error_kind(&error),
                    http_status.or_else(|| error_status(&error)),
                    json!({}),
                );
                return Err(error);
            }
        };
        let cache = CachedPrefix {
            name: created.name,
            model: self.model,
            prefix,
            tokens: created.tokens,
            expires_at: created.expires_at,
        };
        let saved = self.save_cache(&cache, timestamp);
        self.store.metrics().record("cache_saved", Some(context), json!({
            "cache": cache, "success": saved.is_ok(),
            "storage_allocation_estimate_usd": cache.tokens as f64 * self.model.prices(timestamp).storage / 1_000_000.,
        }));
        if let Err(error) = saved {
            delete_cache(self.gemini, self.store, cache, Some(context), "save_failed").await;
            return Err(error);
        }
        Ok(cache)
    }

    fn save_cache(&self, cache: &CachedPrefix, timestamp: u64) -> Result<()> {
        let mut chat = self.state.borrow_mut();
        let old = chat.cache.clone();
        let old_cost = chat.cache_storage_cost_usd;
        chat.cache.entries.retain(|entry| entry.model != self.model);
        chat.cache.entries.push(cache.clone());
        chat.cache_storage_cost_usd +=
            cache.tokens as f64 * self.model.prices(timestamp).storage / 1_000_000.;
        if let Err(error) = self.store.save(&mut chat) {
            chat.cache = old;
            chat.cache_storage_cost_usd = old_cost;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn reject_cache(
        &self,
        cache: &CachedPrefix,
        context: MetricContext,
    ) -> Result<bool> {
        let mut chat = self.state.borrow_mut();
        if !chat.partial.is_empty() || !chat.partial_thoughts.is_empty() {
            return Ok(false);
        }
        chat.cache.entries.retain(|entry| entry.name != cache.name);
        chat.cache_notice = Some("Cache unavailable. Using the full conversation.".into());
        let saved = self.store.save(&mut chat);
        self.store.metrics().record(
            "cache_rejected",
            Some(context),
            json!({
                "cache": cache, "state_saved": saved.is_ok(),
            }),
        );
        saved?;
        Ok(true)
    }
}
