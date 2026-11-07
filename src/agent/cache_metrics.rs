use serde_json::json;

use crate::{
    cache::CachedPrefix,
    gemini::{Gemini, error_kind, error_status},
    metrics::Context,
    model::now,
    store::Store,
};

pub(super) async fn delete_cache(
    gemini: &Gemini,
    store: &Store,
    cache: CachedPrefix,
    context: Option<Context>,
    reason: &str,
) {
    let operation = store.metrics().operation(
        context,
        "delete",
        reason,
        json!({
            "cache": cache,
            "rates": cache.model.prices(now()),
        }),
    );
    match gemini.delete_cache(cache.name).await {
        Ok(status) => operation.finish(
            if status.is_success() {
                "success"
            } else {
                "http_error"
            },
            Some(status.as_u16()),
            json!({}),
        ),
        Err(error) => operation.finish(error_kind(&error), error_status(&error), json!({})),
    }
}
