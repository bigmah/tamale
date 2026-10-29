use std::time::Duration;

use futures::StreamExt;
use rig_core::completion::{
    AssistantContent, CompletionError, CompletionModel, FinishReason, Message, ToolDefinition,
    Usage,
};
use rig_core::streaming::StreamedAssistantContent;
use tokio::sync::mpsc;

use super::{AgentEvent, TurnConfig};

pub(super) struct Response {
    pub choice: Vec<AssistantContent>,
    pub usage: Usage,
    pub finish_reason: Option<FinishReason>,
}

/// One completion request, retried with exponential backoff on transient errors.
pub(super) async fn request<M>(
    model: &M,
    preamble: &str,
    tool_defs: &[ToolDefinition],
    history: &[Message],
    config: TurnConfig,
    stream_text: bool,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) -> anyhow::Result<Response>
where
    M: CompletionModel + Clone,
{
    let (prompt, rest) = history
        .split_last()
        .ok_or_else(|| anyhow::anyhow!("empty history"))?;

    let mut attempt = 0;
    let timeout = Duration::from_secs(config.limits.request_timeout_secs);
    loop {
        let result = async {
            let opening = model
                .completion_request(prompt.clone())
                .preamble(preamble.to_string())
                .messages(rest.to_vec())
                .tools(tool_defs.to_vec())
                .max_tokens(if stream_text {
                    config.limits.max_tokens
                } else {
                    config.limits.compact_max_tokens
                })
                .additional_params(config.reasoning.params())
                .stream();
            let mut stream = tokio::time::timeout(timeout, opening)
                .await
                .map_err(|_| timeout_error(timeout))??;
            let mut content_error = None;
            while let Some(item) = tokio::time::timeout(timeout, stream.next())
                .await
                .map_err(|_| timeout_error(timeout))?
            {
                match item {
                    Ok(StreamedAssistantContent::Text(t)) if stream_text => {
                        let _ = tx.send(AgentEvent::TextDelta(t.text));
                    }
                    // A token limit can cut a tool's JSON in half. Drain to the
                    // terminal record before deciding whether this is truncation.
                    Err(
                        e @ (CompletionError::JsonError(_) | CompletionError::ResponseError(_)),
                    ) => {
                        content_error.get_or_insert(e);
                    }
                    Err(e) => return Err(e),
                    _ => {}
                }
            }
            let terminal = stream.response.as_ref().ok_or_else(|| {
                CompletionError::ProviderError(
                    "stream ended before a final response was received".into(),
                )
            })?;
            let finish_reason = terminal.finish_reason.clone();
            if let Some(e) = content_error
                && !matches!(finish_reason, Some(FinishReason::Length))
            {
                return Err(e);
            }
            Ok::<_, CompletionError>(Response {
                choice: stream.choice.to_vec(),
                usage: stream.usage(),
                finish_reason,
            })
        }
        .await;

        match result {
            Ok(r) => return Ok(r),
            Err(e) if attempt < config.limits.max_retries && is_retryable(&e) => {
                attempt += 1;
                let delay = retry_delay(&e, attempt);
                let _ = tx.send(AgentEvent::Retry {
                    attempt,
                    delay,
                    error: short_error(&e),
                });
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn timeout_error(timeout: Duration) -> CompletionError {
    CompletionError::ProviderError(format!(
        "no response event received for {}s (request timeout)",
        timeout.as_secs()
    ))
}

fn is_retryable(e: &CompletionError) -> bool {
    if let Some(status) = e.provider_response_status() {
        let code = status.as_u16();
        if matches!(code, 408 | 409 | 429) || code >= 500 {
            return true;
        }
        // Some providers send error envelopes with a 2xx status; fall through to the body.
        if !status.is_success() {
            return false;
        }
    }
    if let Some(body) = e.provider_response_body() {
        return [
            "overloaded_error",
            "rate_limit_error",
            "api_error",
            "timeout_error",
        ]
        .iter()
        .any(|k| body.contains(k));
    }
    // No HTTP response at all: connection resets, timeouts, broken streams.
    matches!(
        e,
        CompletionError::HttpError(_) | CompletionError::ProviderError(_)
    )
}

fn retry_delay(e: &CompletionError, attempt: u32) -> Duration {
    let retry_after = e
        .provider_response_headers()
        .and_then(|h| h.get("retry-after"))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if let Some(secs) = retry_after {
        return Duration::from_secs(secs.clamp(1, 120));
    }
    let base = Duration::from_secs(1 << (attempt - 1).min(5)).min(Duration::from_secs(30));
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() % 500)
        .unwrap_or(0);
    base + Duration::from_millis(jitter as u64)
}

fn short_error(e: &CompletionError) -> String {
    if let Ok(Some(json)) = e.provider_response_json()
        && let Some(msg) = json.pointer("/error/message").and_then(|m| m.as_str())
    {
        return match e.provider_response_status() {
            Some(s) if !s.is_success() => format!("{} {msg}", s.as_u16()),
            _ => msg.to_string(),
        };
    }
    let s = e.to_string();
    match s.char_indices().nth(200) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}
