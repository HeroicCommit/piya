use anyhow::{anyhow, Context, Result};
use async_openai::{
    config::OpenAIConfig,
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
            client: Client::with_config(config),
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
        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|err| anyhow!("chat completion failed: {err:#}"))?;

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
        let response = if let Some(epoch) = cancel_epoch {
            tokio::select! {
                _ = crate::chat_cancel::wait_cancelled(epoch) => {
                    return Err(anyhow!("cancelled"));
                }
                result = chat.create(request) => {
                    result.map_err(|err| anyhow!("chat completion failed: {err:#}"))?
                }
            }
        } else {
            chat.create(request)
                .await
                .map_err(|err| anyhow!("chat completion failed: {err:#}"))?
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

        let response = self
            .client
            .embeddings()
            .create(request)
            .await
            .context("embedding request failed")?;

        Ok(response
            .data
            .into_iter()
            .map(|d| d.embedding)
            .collect())
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
