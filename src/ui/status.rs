use super::*;
use space::model::CONTEXT_WINDOW_TOKENS;

impl Workspace {
    pub(super) fn usage_footer(&self) -> Div {
        let usage = self
            .chat
            .as_ref()
            .map(|chat| chat.usage)
            .unwrap_or_default();
        let cost = self.chat.as_ref().map_or(0., Chat::estimated_cost_usd);
        let context_limit = format_tokens(CONTEXT_WINDOW_TOKENS);
        let context = self.chat.as_ref().and_then(|chat| chat.context_tokens);
        let context_label = self
            .chat
            .as_ref()
            .filter(|chat| chat.context_tokens.is_some() && chat.model != chat.context_model.id())
            .map_or_else(
                || "Context".into(),
                |chat| format!("Context ({})", chat.context_model.name()),
            );
        let context = context.map_or_else(
            || format!("{context_label}: — / {context_limit}"),
            |tokens| {
                let percent = tokens as f64 / CONTEXT_WINDOW_TOKENS as f64 * 100.;
                format!(
                    "{context_label}: {} / {context_limit} ({percent:.1}%)",
                    format_tokens(tokens)
                )
            },
        );
        div()
            .px_5()
            .pb_3()
            .flex()
            .flex_wrap()
            .gap_x_3()
            .gap_y_1()
            .text_xs()
            .text_color(rgb(0x858593))
            .children(
                [
                    context,
                    format!("{} input", format_tokens(usage.input)),
                    format!("{} cached", format_tokens(usage.cached)),
                    format!("{} output", format_tokens(usage.output)),
                    format!("{} thinking", format_tokens(usage.thinking)),
                    format!("Est. ${cost:.4} USD"),
                ]
                .into_iter()
                .map(|label| div().child(label)),
            )
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    // Promote counts that round to 1,000K to the M suffix.
    let (scale, suffix) = if tokens >= 999_995 {
        (1_000_000., "M")
    } else {
        (1_000., "K")
    };
    let value = format!("{:.2}", tokens as f64 / scale);
    format!(
        "{}{suffix}",
        value.trim_end_matches('0').trim_end_matches('.')
    )
}
