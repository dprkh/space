mod cache;
mod metrics;

use self::cache::Prepared;
use super::{Event, publish};
use crate::{
    gemini::{Gemini, Generation, GenerationUpdate, cache_rejected},
    metrics::Context as MetricContext,
    model::{Chat, Content, Model, Role, Status, ToolChange, Usage, function_response, now},
    project::{Project, ToolOutput},
    store::{Store, check_pdf_capacity},
};
use anyhow::{Context, Result, bail};
use async_channel::Sender;
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc, time::Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) struct RunTask {
    pub(super) state: Rc<RefCell<Chat>>,
    pub(super) id: Uuid,
    pub(super) cancel: CancellationToken,
}

impl RunTask {
    pub(super) async fn execute(
        self,
        project: &Project,
        store: &Store,
        gemini: &Gemini,
        events: &Sender<Event>,
    ) -> (Uuid, Uuid, Result<()>) {
        let model = self.state.borrow().partial_model;
        let result = Run {
            project,
            store,
            gemini,
            events,
            state: &self.state,
            id: self.id,
            cancel: &self.cancel,
            model,
            cache_disabled: false,
        }
        .execute()
        .await;
        let mut chat = self.state.borrow_mut();
        if let Err(error) = result {
            chat.interrupt(&format!("{error:#}"));
        }
        let result = store.save(&mut chat);
        (chat.id, self.id, result)
    }
}

struct Run<'a> {
    project: &'a Project,
    store: &'a Store,
    gemini: &'a Gemini,
    events: &'a Sender<Event>,
    state: &'a RefCell<Chat>,
    id: Uuid,
    cancel: &'a CancellationToken,
    model: Model,
    cache_disabled: bool,
}

struct RoundResponse {
    generation: Result<Generation>,
    usage: Option<Usage>,
    explicit_tokens: u64,
    started_at: u64,
}

struct PendingTools {
    content_index: usize,
    calls: Vec<(usize, Value)>,
}

impl Run<'_> {
    async fn execute(&mut self) -> Result<()> {
        for round in 0..=32 {
            let context = MetricContext {
                chat_id: self.state.borrow().id,
                run_id: Some(self.id),
                round: Some(round),
            };
            let request = Gemini::request(&self.state.borrow());
            let prepared = self.prepare_request(&request, context).await?;
            publish(&self.state.borrow(), self.events, false, Some(self.id));
            let response = self
                .generate_round(&request, prepared.as_ref(), context)
                .await?;
            self.record_usage(&request, &response);
            let tools = self.save_response(response.generation?)?;
            if tools.calls.is_empty() {
                self.state.borrow_mut().status = Status::Idle;
                return Ok(());
            }
            if round == 32 {
                bail!("Reached the limit of 32 tool rounds");
            }
            self.state.borrow_mut().contents.push(Content {
                role: Role::User,
                parts: Vec::new(),
            });
            self.execute_tools(tools).await?;
        }
        unreachable!("The last round returns before executing tools")
    }

    async fn prepare_request(
        &mut self,
        request: &Value,
        context: MetricContext,
    ) -> Result<Option<Prepared>> {
        self.store.metrics().record(
            "cache_preparation_started",
            Some(context),
            json!({ "model": self.model }),
        );
        let started = Instant::now();
        if self.cache_disabled {
            self.store.metrics().record(
                "cache_decision",
                Some(context),
                json!({
                    "action": "skip", "reason": "disabled_for_run", "model": self.model,
                }),
            );
            self.record_preparation(context, started, "disabled", None);
            return Ok(None);
        }
        let prepared = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                self.record_preparation(context, started, "cancelled", None);
                bail!("Run stopped");
            },
            result = self.prepare_cache(request, context) => result,
        };
        match prepared {
            Ok(prepared) => {
                self.record_preparation(context, started, "success", prepared.as_ref());
                Ok(prepared)
            }
            Err(error) => {
                self.cache_disabled = true;
                self.state.borrow_mut().cache_notice =
                    Some(format!("Cache unavailable: {error:#}"));
                self.record_preparation(context, started, crate::gemini::error_kind(&error), None);
                Ok(None)
            }
        }
    }

    fn record_preparation(
        &self,
        context: MetricContext,
        started: Instant,
        outcome: &str,
        prepared: Option<&Prepared>,
    ) {
        let duration_ms = started.elapsed().as_secs_f64() * 1_000.;
        self.store.metrics().record(
            "cache_preparation_finished",
            Some(context),
            json!({
                "outcome": outcome,
                "duration_ms": duration_ms,
                "selected_cache": prepared.map(|prepared| &prepared.cache),
            }),
        );
    }

    async fn generate_round(
        &mut self,
        request: &Value,
        prepared: Option<&Prepared>,
        context: MetricContext,
    ) -> Result<RoundResponse> {
        let mut cached = prepared;
        let mut progress = StreamProgress {
            state: self.state,
            events: self.events,
            chat_id: self.state.borrow().id,
            run_id: self.id,
            model: self.model,
            received: false,
            usage: None,
        };
        let started_at = now();
        let mut attempt_number = 0;
        loop {
            attempt_number += 1;
            let mut attempt = metrics::Attempt::start(
                self.store.metrics(),
                context,
                attempt_number,
                self.model,
                cached.map(|prepared| &prepared.cache),
            );
            let body = cached.map_or(request, |prepared| &prepared.request);
            let (result, cancelled) = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => (Err(anyhow::anyhow!("Run stopped")), true),
                result = self.gemini.generate(self.model.id(), body, |update| {
                    attempt.observe(&update);
                    progress.update(update);
                }) => (result, false),
            };
            attempt.finish(&result, cancelled);
            if let Some(prepared) = cached
                && progress.usage.is_none()
                && result.as_ref().is_err_and(cache_rejected)
                && self.reject_cache(&prepared.cache, context)?
            {
                cached = None;
                self.cache_disabled = true;
                continue;
            }
            return Ok(RoundResponse {
                generation: result,
                usage: progress.usage,
                explicit_tokens: cached.map_or(0, |prepared| prepared.cache.tokens),
                started_at,
            });
        }
    }

    fn record_usage(&self, request: &Value, response: &RoundResponse) {
        let mut chat = self.state.borrow_mut();
        if let Some(usage) = response.usage {
            chat.record_usage(self.model, usage, response.started_at);
            if response
                .generation
                .as_ref()
                .is_ok_and(|generation| generation.finish_reason == "STOP")
            {
                chat.cache.observe(
                    self.model,
                    request,
                    usage,
                    response.explicit_tokens,
                    response.started_at,
                );
            }
        } else if response.generation.is_ok()
            || !chat.partial.is_empty()
            || !chat.partial_thoughts.is_empty()
            || self.cancel.is_cancelled()
        {
            chat.usage_complete = false;
        }
    }

    fn save_response(&self, generation: Generation) -> Result<PendingTools> {
        let calls = generation
            .content
            .parts
            .iter()
            .enumerate()
            .filter_map(|(index, part)| part.get("functionCall").cloned().map(|call| (index, call)))
            .collect();
        let mut chat = self.state.borrow_mut();
        chat.partial.clear();
        chat.partial_thoughts.clear();
        let content_index = chat.contents.len();
        // An empty completion ends the turn without adding an empty API message.
        if !generation.content.parts.is_empty() {
            chat.contents.push(generation.content);
            chat.response_models.insert(content_index, self.model);
        }
        chat.record_activity();
        // Save calls and signatures before any file operation starts.
        self.store.save(&mut chat)?;
        publish(&chat, self.events, false, Some(self.id));
        if generation.finish_reason != "STOP" {
            bail!(
                "Response stopped: {}. Any requested file operations were not run",
                generation.finish_reason
            );
        }
        Ok(PendingTools {
            content_index,
            calls,
        })
    }

    async fn execute_tools(&self, tools: PendingTools) -> Result<()> {
        for (part_index, call) in tools.calls {
            // Process chat controls between synchronous file operations.
            tokio::task::yield_now().await;
            if self.cancel.is_cancelled() {
                bail!("Run stopped");
            }
            self.execute_tool(tools.content_index, part_index, call)?;
        }
        Ok(())
    }

    fn execute_tool(&self, content_index: usize, part_index: usize, call: Value) -> Result<()> {
        let ToolOutput {
            response,
            pdf,
            change,
        } = self.project.execute(&call);
        let has_pdf = pdf.is_some();
        let mut response = function_response(&call, response);
        if let Some(pdf) = pdf {
            response["functionResponse"]["parts"] = json!([pdf.into_part()]);
        }
        let mut chat = self.state.borrow_mut();
        if let Some(change) = change {
            chat.file_changes.push(ToolChange {
                content_index,
                part_index,
                change,
            });
        }
        chat.contents
            .last_mut()
            .context("Missing tool response")?
            .parts
            .push(response);
        if has_pdf && let Err(error) = check_pdf_capacity(&chat) {
            let response = chat
                .contents
                .last_mut()
                .and_then(|content| content.parts.last_mut())
                .context("Missing PDF tool response")?;
            *response = function_response(
                &call,
                json!({
                    "ok": false,
                    "error": format!("{error:#}"),
                }),
            );
        }
        self.store.save(&mut chat)?;
        publish(&chat, self.events, false, Some(self.id));
        Ok(())
    }
}

struct StreamProgress<'a> {
    state: &'a RefCell<Chat>,
    events: &'a Sender<Event>,
    chat_id: Uuid,
    run_id: Uuid,
    model: Model,
    received: bool,
    usage: Option<Usage>,
}

impl StreamProgress<'_> {
    fn update(&mut self, update: GenerationUpdate) {
        match update {
            GenerationUpdate::ResponseStatus(_) | GenerationUpdate::FinishReason(_) => {}
            GenerationUpdate::Text(text) => {
                self.record_activity();
                self.state.borrow_mut().partial.push_str(&text);
                let _ = self.events.try_send(Event::Delta {
                    chat_id: self.chat_id,
                    run_id: self.run_id,
                    text,
                });
            }
            GenerationUpdate::Thought(text) => {
                self.record_activity();
                self.state.borrow_mut().partial_thoughts.push_str(&text);
                let _ = self.events.try_send(Event::ThoughtDelta {
                    chat_id: self.chat_id,
                    run_id: self.run_id,
                    text,
                });
            }
            GenerationUpdate::Usage(usage) => {
                self.usage = Some(usage);
                let tokens = self.model.context_tokens(usage);
                {
                    let mut chat = self.state.borrow_mut();
                    chat.context_tokens = Some(tokens);
                    chat.context_model = self.model;
                }
                let _ = self.events.try_send(Event::ContextTokens {
                    chat_id: self.chat_id,
                    run_id: self.run_id,
                    tokens,
                    model: self.model,
                });
            }
        }
    }

    fn record_activity(&mut self) {
        if self.received {
            return;
        }
        self.received = true;
        let mut chat = self.state.borrow_mut();
        chat.record_activity();
        let _ = self.events.try_send(Event::Activity {
            chat_id: self.chat_id,
            run_id: self.run_id,
            timestamp: chat.updated_at,
        });
    }
}
