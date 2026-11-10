use rig_core::completion::Usage;

pub(super) const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// Built-in context window of a model in tokens. Overrides are resolved in `config`.
pub fn context_window(model: &str) -> u64 {
    if let Some(window) = other_context_window(model) {
        return window;
    }
    let m = model.strip_prefix("anthropic/").unwrap_or(model);
    // Opus 4.6+, Sonnet 4.6 and every Claude 5 model have a 1M-token window.
    let one_million = m.starts_with("claude-opus-4-6")
        || m.starts_with("claude-opus-4-7")
        || m.starts_with("claude-opus-4-8")
        || m.starts_with("claude-sonnet-4-6")
        || ["opus", "sonnet", "haiku", "fable"]
            .iter()
            .any(|line| m.starts_with(&format!("claude-{line}-5")));
    if one_million {
        1_000_000
    } else {
        DEFAULT_CONTEXT_WINDOW
    }
}

/// Context windows of the default models of the built-in OpenAI-compatible providers.
/// A gateway's `vendor/` prefix is ignored.
fn other_context_window(model: &str) -> Option<u64> {
    let m = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    Some(if m.starts_with("deepseek") {
        128_000
    } else if m == "kimi-k2" || m.starts_with("kimi-k2-0711") {
        131_072
    } else if m.starts_with("kimi-k2") {
        262_144
    } else if m.starts_with("gemini-2.5") || m.starts_with("gemini-3") {
        1_048_576
    } else if m.starts_with("grok-4") {
        256_000
    } else if m.starts_with("gpt-5") {
        400_000
    } else if m.starts_with("gpt-oss") {
        131_072
    } else {
        return None;
    })
}

/// USD price per million input and output tokens, or `None` for unknown models.
fn pricing(model: &str) -> Option<(f64, f64)> {
    let m = model.strip_prefix("anthropic/").unwrap_or(model);
    let price = if m.starts_with("claude-opus-4-0")
        || m.starts_with("claude-opus-4-1")
        || m == "claude-opus-4"
        || m.starts_with("claude-opus-4-2025")
        || m.starts_with("claude-3-opus")
    {
        (15.0, 75.0)
    } else if m.starts_with("claude-opus") {
        // Opus 4.5 and later.
        (5.0, 25.0)
    } else if m.starts_with("claude-sonnet")
        || m.starts_with("claude-3-7-sonnet")
        || m.starts_with("claude-3-5-sonnet")
    {
        (3.0, 15.0)
    } else if m.starts_with("claude-haiku-4") {
        (1.0, 5.0)
    } else if m.starts_with("claude-3-5-haiku") {
        (0.8, 4.0)
    } else if m.starts_with("claude-3-haiku") {
        (0.25, 1.25)
    } else {
        return None;
    };
    Some(price)
}

/// Estimated USD cost of a request, or `None` if the model's pricing is unknown.
/// Cache reads bill at 0.1x the input price, cache writes (5-minute TTL) at 1.25x.
pub fn cost(model: &str, u: &Usage) -> Option<f64> {
    let (input, output) = pricing(model)?;
    let dollars = (u.input_tokens as f64 * input
        + u.cached_input_tokens as f64 * input * 0.1
        + u.cache_creation_input_tokens as f64 * input * 1.25
        + u.output_tokens as f64 * output)
        / 1e6;
    Some(dollars)
}

/// Total tokens a request occupied in the context window (prompt incl. cache, plus output).
pub fn context_size(u: &Usage) -> u64 {
    u.input_tokens + u.cached_input_tokens + u.cache_creation_input_tokens + u.output_tokens
}
