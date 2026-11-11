use anyhow::{anyhow, Context, Result};
use async_openai::{
    config::OpenAIConfig,
    error::OpenAIError,
    types::{
        ChatCompletionRequestAssistantMessage,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionResponseMessage,
        ChatCompletionTool, ChatCompletionToolChoiceOption, CreateChatCompletionRequest,
        CreateChatCompletionRequestArgs, CreateEmbeddingRequestArgs, ResponseFormat,
    },
    Client,
};
use serde_json::Value;
use std::time::Duration;

use crate::config::Settings;

pub struct LlmClient {
    client: Client<OpenAIConfig>,
    model: String,
    embedding_model: String,
}

impl Clone for LlmClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            model: self.model.clone(),
            embedding_model: self.embedding_model.clone(),
        }
    }
}

impl LlmClient {
    pub fn new(settings: &Settings) -> Result<Self> {
        settings.require_api_key()?;

        let config = OpenAIConfig::new()
            .with_api_key(settings.openai_api_key.clone())
            .with_api_base(settings.openai_base_url.clone());

        Ok(Self {
            client: Client::with_config(config).with_http_client(http_client()),
            model: settings.model.clone(),
            embedding_model: settings.embedding_model.clone(),
        })
    }

    pub async fn chat(&self, system: &str, user: &str, json_mode: bool) -> Result<String> {
        self.chat_with_history(system, &[], user, json_mode, 0.3)
            .await
    }

    /// Multi-turn chat. History items are `(role, text)` with role `user` or `assistant`.
    pub async fn chat_with_history(
        &self,
        system: &str,
        history: &[(String, String)],
        user: &str,
        json_mode: bool,
        temperature: f32,
    ) -> Result<String> {
        let mut messages = vec![ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: system.into(),
                name: None,
            },
        )];

        for (role, content) in history {
            match role.as_str() {
                "user" => messages.push(ChatCompletionRequestMessage::User(
                    ChatCompletionRequestUserMessage {
                        content: content.clone().into(),
                        name: None,
                    },
                )),
                "assistant" | "piya" => messages.push(ChatCompletionRequestMessage::Assistant(
                    ChatCompletionRequestAssistantMessage {
                        content: Some(content.clone().into()),
                        name: None,
                        tool_calls: None,
                        refusal: None,
                        audio: None,
                        #[allow(deprecated)]
                        function_call: None,
                    },
                )),
                _ => {}
            }
        }

        messages.push(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: user.into(),
                name: None,
            },
        ));

        let mut req = CreateChatCompletionRequestArgs::default();
        req.model(self.model.clone());
        req.messages(messages);
        req.temperature(temperature);

        if json_mode {
            req.response_format(ResponseFormat::JsonObject);
        }

        let request: CreateChatCompletionRequest = req.build()?;
        let chat = self.client.chat();
        let response = with_retries("chat completion", || chat.create(request.clone())).await?;

        Ok(response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default())
    }

    /// Multi-turn chat with optional MCP tools. Runs a tool loop until the model returns text.
    pub async fn chat_with_tools(
        &self,
        system: &str,
        history: &[(String, String)],
        user: &str,
        tools: &[ChatCompletionTool],
        mut on_tool: impl FnMut(&str, &str) -> Result<String>,
        temperature: f32,
        max_rounds: usize,
        cancel_epoch: Option<u64>,
    ) -> Result<String> {
        let mut messages = Self::build_messages(system, history, user);

        for _ in 0..max_rounds {
            if let Some(epoch) = cancel_epoch {
                crate::chat_cancel::check(epoch)?;
            }
            let assistant = self
                .complete(messages.clone(), Some(tools), temperature, cancel_epoch)
                .await?;

            let Some(tool_calls) = assistant.tool_calls.clone() else {
                return Ok(assistant.content.unwrap_or_default());
            };

            if tool_calls.is_empty() {
                return Ok(assistant.content.unwrap_or_default());
            }

            messages.push(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    content: assistant.content.map(Into::into),
                    name: None,
                    tool_calls: Some(tool_calls.clone()),
                    refusal: None,
                    audio: None,
                    #[allow(deprecated)]
                    function_call: None,
                },
            ));

            for call in tool_calls {
                if let Some(epoch) = cancel_epoch {
                    crate::chat_cancel::check(epoch)?;
                }
                let result = on_tool(&call.function.name, &call.function.arguments)
                    .unwrap_or_else(|err| format!("Error: {err:#}"));
                messages.push(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(result),
                        tool_call_id: call.id,
                    },
                ));
            }
        }

        Err(anyhow::anyhow!("tool loop exceeded {max_rounds} rounds"))
    }

    async fn complete(
        &self,
        messages: Vec<ChatCompletionRequestMessage>,
        tools: Option<&[ChatCompletionTool]>,
        temperature: f32,
        cancel_epoch: Option<u64>,
    ) -> Result<ChatCompletionResponseMessage> {
        let mut req = CreateChatCompletionRequestArgs::default();
        req.model(self.model.clone());
        req.messages(messages);
        req.temperature(temperature);

        if let Some(tools) = tools {
            if !tools.is_empty() {
                req.tools(tools.to_vec());
                req.tool_choice(ChatCompletionToolChoiceOption::Auto);
            }
        }

        let request: CreateChatCompletionRequest = req.build()?;
        let chat = self.client.chat();
        let call = with_retries("chat completion", || chat.create(request.clone()));
        let response = if let Some(epoch) = cancel_epoch {
            tokio::select! {
                _ = crate::chat_cancel::wait_cancelled(epoch) => {
                    return Err(anyhow!("cancelled"));
                }
                result = call => result?,
            }
        } else {
            call.await?
        };

        response
            .choices
            .first()
            .map(|c| c.message.clone())
            .ok_or_else(|| anyhow::anyhow!("empty chat completion response"))
    }

    fn build_messages(
        system: &str,
        history: &[(String, String)],
        user: &str,
    ) -> Vec<ChatCompletionRequestMessage> {
        let mut messages = vec![ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: system.into(),
                name: None,
            },
        )];

        for (role, content) in history {
            match role.as_str() {
                "user" => messages.push(ChatCompletionRequestMessage::User(
                    ChatCompletionRequestUserMessage {
                        content: content.clone().into(),
                        name: None,
                    },
                )),
                "assistant" | "piya" => messages.push(ChatCompletionRequestMessage::Assistant(
                    ChatCompletionRequestAssistantMessage {
                        content: Some(content.clone().into()),
                        name: None,
                        tool_calls: None,
                        refusal: None,
                        audio: None,
                        #[allow(deprecated)]
                        function_call: None,
                    },
                )),
                _ => {}
            }
        }

        messages.push(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: user.into(),
                name: None,
            },
        ));

        messages
    }

    pub async fn chat_json(&self, system: &str, user: &str) -> Result<Value> {
        let raw = self.chat(system, user, true).await?;
        Ok(serde_json::from_str(&raw).context("parse LLM JSON response")?)
    }

    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        // text-embedding-3-* caps at 8192 tokens; dense logs blow past that quickly.
        let truncated: Vec<String> = texts.iter().map(|t| truncate_for_embedding(t)).collect();

        let request = CreateEmbeddingRequestArgs::default()
            .model(self.embedding_model.clone())
            .input(truncated)
            .build()?;

        let embeddings = self.client.embeddings();
        let response =
            with_retries("embedding request", || embeddings.create(request.clone())).await?;

        Ok(response
            .data
            .into_iter()
            .map(|d| d.embedding)
            .collect())
    }
}

/// HTTP client with a request timeout so a stalled server cannot hang Piya forever.
/// Override with `PIYA_LLM_TIMEOUT_SEC` (default 180; local models may need more).
fn http_client() -> reqwest::Client {
    let secs = std::env::var("PIYA_LLM_TIMEOUT_SEC")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(180);
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(secs))
        .build()
        .unwrap_or_default()
}

const MAX_ATTEMPTS: u32 = 3;

/// Process-wide cap on in-flight LLM requests so parallel workers do not trip rate
/// limits. `PIYA_LLM_CONCURRENCY` (default 4).
fn llm_permits() -> &'static tokio::sync::Semaphore {
    static PERMITS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    PERMITS.get_or_init(|| {
        let n = std::env::var("PIYA_LLM_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(4);
        tokio::sync::Semaphore::new(n)
    })
}

/// Network-level failures worth retrying. API errors (bad request, auth) are not.
/// Rate limits (429) are already retried with backoff inside async-openai.
fn is_transient(err: &OpenAIError) -> bool {
    match err {
        OpenAIError::Reqwest(e) => e.is_timeout() || e.is_connect() || e.is_request(),
        _ => false,
    }
}

/// Run `op`, retrying transient network errors with 1s, 2s backoff.
async fn with_retries<T, F, Fut>(what: &str, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, OpenAIError>>,
{
    let mut attempt = 1;
    loop {
        // Hold a permit only while the request is in flight, not during backoff.
        let result = {
            let _permit = llm_permits().acquire().await;
            op().await
        };
        match result {
            Ok(v) => return Ok(v),
            Err(err) if attempt < MAX_ATTEMPTS && is_transient(&err) => {
                let delay = Duration::from_secs(1 << (attempt - 1));
                eprintln!(
                    "piya: {what} failed ({err}); retry {attempt}/{} in {}s",
                    MAX_ATTEMPTS - 1,
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(err) => return Err(anyhow!("{what} failed: {err:#}")),
        }
    }
}

/// Keep embedding inputs under the model token cap (~8192).
/// Prefer head + tail so long dumps still match on title and summary.
fn truncate_for_embedding(text: &str) -> String {
    // ~3 chars/token is optimistic for English; use ~2.5 for dense logs/domains.
    const MAX_CHARS: usize = 18_000;
    const HEAD: usize = 12_000;
    const TAIL: usize = 5_000;

    if text.chars().count() <= MAX_CHARS {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let head: String = chars.iter().take(HEAD).collect();
    let tail: String = chars.iter().rev().take(TAIL).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n\n…[truncated for embedding]…\n\n{tail}")
}

#[cfg(test)]
mod embed_tests {
    use super::truncate_for_embedding;

    #[test]
    fn leaves_short_text_alone() {
        assert_eq!(truncate_for_embedding("hello"), "hello");
    }

    #[test]
    fn truncates_huge_text() {
        let huge = "a".repeat(50_000);
        let out = truncate_for_embedding(&huge);
        assert!(out.len() < 20_000);
        assert!(out.contains("truncated for embedding"));
    }
}
