//! Providers that speak OpenAI's Chat Completions API (`POST {base_url}/chat/completions`):
//! DeepSeek, Kimi, Gemini, OpenRouter, xAI, Groq, local servers and most others. Each is a
//! `[backend.providers.<name>]` entry with a base URL and the environment variable that
//! holds its API key; the agent loop is the same as for the Anthropic API.

use anyhow::{Context, Result};
use rig_core::client::{CompletionClient, ModelListingClient};
use rig_core::providers::openai;
use serde::Deserialize;

/// `[backend.providers.<name>]`: an OpenAI-compatible endpoint.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    /// Shown in the footer and the backend picker; the provider's name when empty.
    #[serde(default)]
    pub label: String,
    /// The API's base, without `/chat/completions`.
    pub base_url: String,
    /// The environment variable holding the API key. Empty for servers without keys
    /// (Ollama, LM Studio, vLLM).
    #[serde(default)]
    pub api_key_env: String,
    /// The model to use while `model.name` is a Claude model.
    pub model: String,
    /// The provider's output limit, when it's below `agent.max_tokens`.
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

impl ProviderSettings {
    /// Whether requests can be made: the key's variable is set, or none is needed.
    pub fn key_set(&self) -> bool {
        self.api_key_env.is_empty() || key(&self.api_key_env).is_some()
    }
}

fn key(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|k| !k.trim().is_empty())
}

#[derive(Clone)]
pub struct Compat {
    /// The provider's name in `[backend.providers]`.
    pub name: String,
    pub label: String,
    pub api_key_env: String,
    pub default_model: String,
    max_tokens: Option<u64>,
    client: openai::CompletionsClient,
}

impl Compat {
    pub fn connect(name: &str, provider: &ProviderSettings) -> Result<Self> {
        let api_key = if provider.api_key_env.is_empty() {
            // Keyless local servers accept any bearer token.
            "none".to_string()
        } else {
            key(&provider.api_key_env)
                .with_context(|| format!("{} is not set", provider.api_key_env))?
        };
        let client = openai::CompletionsClient::builder()
            .api_key(api_key)
            .base_url(provider.base_url.trim_end_matches('/'))
            .build()
            .with_context(|| format!("creating a client for {}", provider.base_url))?;
        Ok(Self {
            name: name.to_string(),
            label: if provider.label.trim().is_empty() {
                name.to_string()
            } else {
                provider.label.clone()
            },
            api_key_env: provider.api_key_env.clone(),
            default_model: provider.model.clone(),
            max_tokens: provider.max_tokens,
            client,
        })
    }

    /// The model a turn uses: `name`, unless that's a Claude model this provider
    /// doesn't serve.
    pub fn model<'a>(&'a self, name: &'a str) -> &'a str {
        if super::is_claude_model(name) {
            &self.default_model
        } else {
            name.trim()
        }
    }

    pub fn completion_model(&self, name: &str) -> openai::completion::CompletionModel {
        self.client.completion_model(self.model(name).to_string())
    }

    /// Why `/login` doesn't apply.
    pub fn no_login(&self) -> String {
        match self.api_key_env.as_str() {
            "" => format!("{} needs no sign-in", self.label),
            var => format!("{} uses {var}; there's nothing to sign in to", self.label),
        }
    }

    /// Caps the per-response token limit at the provider's.
    pub fn max_tokens(&self, configured: u64) -> u64 {
        self.max_tokens
            .map_or(configured, |cap| configured.min(cap))
    }

    pub async fn list_models(&self) -> Result<Vec<(String, String)>> {
        let list = self.client.list_models().await?;
        let mut models: Vec<(String, String)> = list
            .data
            .into_iter()
            .map(|m| (m.id.clone(), m.name.unwrap_or(m.id)))
            .collect();
        models.sort();
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Router, extract::State, http::header, response::IntoResponse, routing};
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    use super::*;
    use crate::agent::{AgentEvent, Effort, TurnCtx};
    use crate::backend::{Backend, Turn};
    use crate::config::Settings;
    use crate::permissions::Policy;
    use crate::workspace::Workspace;

    type Seen = Arc<Mutex<Vec<Value>>>;

    async fn chat(State(seen): State<Seen>, body: String) -> impl IntoResponse {
        seen.lock()
            .unwrap()
            .push(serde_json::from_str(&body).unwrap());
        let chunk = |choices: Value, usage: Value| {
            let chunk = json!({ "id": "c1", "object": "chat.completion.chunk", "created": 0,
                "model": "served-model", "choices": choices, "usage": usage });
            format!("data: {chunk}\n\n")
        };
        let body = [
            chunk(
                json!([{ "index": 0, "delta": { "role": "assistant", "content": "Hello" },
                    "finish_reason": null }]),
                Value::Null,
            ),
            chunk(
                json!([{ "index": 0, "delta": { "content": " there" }, "finish_reason": "stop" }]),
                Value::Null,
            ),
            chunk(
                json!([]),
                json!({ "prompt_tokens": 12, "completion_tokens": 2, "total_tokens": 14 }),
            ),
            "data: [DONE]\n\n".to_string(),
        ]
        .concat();
        ([(header::CONTENT_TYPE, "text/event-stream")], body)
    }

    async fn models() -> impl IntoResponse {
        let list = json!({ "object": "list", "data": [
            { "id": "b-model", "object": "model", "created": 0, "owned_by": "x" },
            { "id": "a-model", "object": "model", "created": 0, "owned_by": "x" },
        ] });
        (
            [(header::CONTENT_TYPE, "application/json")],
            list.to_string(),
        )
    }

    /// A Chat Completions server on localhost, and the bodies it was sent.
    async fn server() -> (String, Seen) {
        let seen = Seen::default();
        let app = Router::new()
            .route("/v1/chat/completions", routing::post(chat))
            .route("/v1/models", routing::get(models))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}/v1/"), seen)
    }

    fn provider(base_url: &str) -> ProviderSettings {
        ProviderSettings {
            label: "Local".into(),
            base_url: base_url.into(),
            api_key_env: String::new(),
            model: "served-model".into(),
            max_tokens: Some(1000),
        }
    }

    #[tokio::test]
    async fn runs_turns_against_an_openai_compatible_api() {
        let (base_url, seen) = server().await;
        let compat = Compat::connect("local", &provider(&base_url)).unwrap();
        assert_eq!(
            compat.list_models().await.unwrap(),
            [
                ("a-model".to_string(), "a-model".to_string()),
                ("b-model".to_string(), "b-model".to_string())
            ]
        );

        let mut settings = Settings::default();
        settings.model.effort = Some(Effort::XHigh);
        let workspace = Workspace::current().unwrap();
        let permissions = Policy::new(&settings.permissions, workspace.cwd.clone()).shared();
        let turn = Turn {
            // A Claude model isn't the provider's, so its own is used.
            model: settings.model.name.clone(),
            history: vec![rig_core::completion::Message::user("hi")],
            prompt: "hi".into(),
            thread: None,
            config: settings.turn_config(0),
            workspace,
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        Backend::Compat(compat)
            .run(turn, TurnCtx::new(permissions, None), tx)
            .await;
        let mut text = String::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                AgentEvent::TextDelta(t) => text.push_str(&t),
                AgentEvent::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        assert_eq!(text, "Hello there");

        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["model"], "served-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 1000, "capped at the provider's limit");
        assert_eq!(body["reasoning_effort"], "high");
        assert!(
            body.get("thinking").is_none(),
            "no Anthropic parameters: {body}"
        );
        assert!(body.get("output_config").is_none(), "{body}");
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(body["tools"].as_array().is_some_and(|t| !t.is_empty()));
    }

    #[test]
    fn keys_come_from_the_environment() {
        let mut keyed = provider("http://localhost/v1");
        keyed.api_key_env = "TAMALE_TEST_KEY_THAT_ISNT_SET".into();
        assert!(!keyed.key_set());
        let error = Compat::connect("p", &keyed).err().unwrap();
        assert!(format!("{error:#}").contains("TAMALE_TEST_KEY_THAT_ISNT_SET is not set"));
        assert!(provider("http://localhost/v1").key_set(), "keyless servers");
    }

    #[test]
    fn switching_providers_switches_models() {
        let compat = |name: &str, model: &str| {
            let mut p = provider("http://localhost/v1");
            p.model = model.into();
            Backend::Compat(Compat::connect(name, &p).unwrap())
        };
        let deepseek = compat("deepseek", "deepseek-chat");
        let kimi = compat("kimi", "kimi-k2.5");
        let api = Backend::Api(rig_core::providers::anthropic::Client::new("k").unwrap());
        let claude = Settings::default().model.name;

        assert_eq!(
            deepseek.model_after(&api, &claude).as_deref(),
            Some("deepseek-chat")
        );
        assert_eq!(
            kimi.model_after(&deepseek, "deepseek-chat").as_deref(),
            Some("kimi-k2.5")
        );
        assert_eq!(deepseek.model_after(&deepseek, "deepseek-reasoner"), None);
        assert_eq!(
            api.model_after(&kimi, "kimi-k2.5").as_deref(),
            Some(claude.as_str())
        );
        assert_eq!(api.model_after(&kimi, "claude-sonnet-4-6"), None);
        assert!(!deepseek.same_login(&kimi));
    }
}
