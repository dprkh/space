use super::Run;
use crate::{
    cache::{CachedPrefix, Prefix},
    model::now,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub(super) struct Prepared {
    pub(super) request: Value,
    pub(super) cache: CachedPrefix,
}

impl Run<'_> {
    pub(super) async fn prepare_cache(&self, request: &Value) -> Result<Option<Prepared>> {
        if self.model.explicit_cache_minimum().is_none() {
            return Ok(None);
        }
        let timestamp = now();
        let plan = self
            .state
            .borrow_mut()
            .cache
            .plan(self.model, request, timestamp);
        for name in plan.obsolete {
            self.gemini.delete_cache(name).await;
        }
        let cache = if let Some(candidate) = plan.candidate {
            let cache = self.create_cache(request, candidate, timestamp).await?;
            if let Some(existing) = plan.existing {
                self.gemini.delete_cache(existing.name).await;
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
        let created = self.gemini.create_cache(&body).await?;
        let cache = CachedPrefix {
            name: created.name,
            model: self.model,
            prefix,
            tokens: created.tokens,
            expires_at: created.expires_at,
        };
        if let Err(error) = self.save_cache(&cache, timestamp) {
            self.gemini.delete_cache(cache.name.clone()).await;
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

    pub(super) fn reject_cache(&self, cache: &CachedPrefix) -> Result<bool> {
        let mut chat = self.state.borrow_mut();
        if !chat.partial.is_empty() || !chat.partial_thoughts.is_empty() {
            return Ok(false);
        }
        chat.cache.entries.retain(|entry| entry.name != cache.name);
        chat.cache_notice = Some("Cache unavailable. Using the full conversation.".into());
        self.store.save(&mut chat)?;
        Ok(true)
    }
}
