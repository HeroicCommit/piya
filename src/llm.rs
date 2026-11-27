//! Model client for every supported provider.
//!
//! Two chat backends behind one API:
//! * OpenAI-compatible Chat Completions (OpenAI, Gemini, OpenRouter, Groq, Mistral,
//!   DeepSeek, xAI, Ollama, LM Studio, custom servers);
//! * native Anthropic Messages API.
//!
//! Messages and tools stay in the OpenAI wire format everywhere else in Piya; the
//! Anthropic backend converts them on the way in and out. Embeddings (memory) use
//! their own OpenAI-compatible connection, which may be a different provider.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_openai::{
    config::OpenAIConfig,
    error::OpenAIError,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessage,
        ChatCompletionRequestMessage, ChatCompletionRequestMessageContentPartImage,
        ChatCompletionRequestMessageContentPartText, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
        ChatCompletionRequestUserMessageContentPart, ChatCompletionResponseMessage,
        ChatCompletionTool, ChatCompletionToolChoiceOption, ChatCompletionToolType,
        CreateChatCompletionRequestArgs, CreateEmbeddingRequestArgs, FunctionCall, ImageUrl,
        ResponseFormat, Role,
    },
    Client,
};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::providers::{self, Api, Connection};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageInput {
    pub media_type: String,
    pub base64_data: String,
}

#[derive(Clone, Debug, Default)]
pub struct LlmTurn {
    pub role: String,
    pub text: String,
    pub images: Vec<ImageInput>,
}

#[derive(Clone)]
pub struct LlmClient {
    inner: Arc<Inner>,
}

struct Inner {
    provider_id: String,
    backend: Backend,
    model: String,
    label: String,
    /// Set when the connection is unusable; returned by every chat call.
    chat_error: Option<String>,
    embedder: Option<Embedder>,
    /// Learned at runtime: this model rejects `temperature` (e.g. reasoning models).
    no_temperature: AtomicBool,
    /// Learned at runtime: this model/server rejects `response_format`.
    no_json_mode: AtomicBool,
}

enum Backend {
    Compat(Client<OpenAIConfig>),
    Anthropic {
        http: reqwest::Client,
        base_url: String,
        api_key: String,
    },
}

struct Embedder {
    client: Client<OpenAIConfig>,
    model: String,
    id: String,
}

fn compat_client(conn: &Connection) -> Client<OpenAIConfig> {
    let config = OpenAIConfig::new()
        .with_api_key(conn.api_key.trim().to_string())
        .with_api_base(conn.base_url.trim().trim_end_matches('/').to_string());
    Client::with_config(config).with_http_client(http_client())
}

impl LlmClient {
    /// Build from settings. Never fails for a missing key/model: chat calls report
    /// that instead, so memory listing and other non-chat paths keep working.
    pub fn new(settings: &Settings) -> Result<Self> {
        let conn = settings.chat_connection();
        let info = conn.info()?;
        let chat_error = settings.require_model().err().map(|e| format!("{e:#}"));
        let backend = match info.api {
            Api::OpenAiCompat => Backend::Compat(compat_client(&conn)),
            Api::Anthropic => Backend::Anthropic {
                http: http_client(),
                base_url: conn.base_url.trim().trim_end_matches('/').to_string(),
                api_key: conn.api_key.trim().to_string(),
            },
        };
        let embedder = match settings.embed_connection() {
            Some(e) if !settings.embedding_model.trim().is_empty() && e.validate().is_ok() => {
                let info = e.info()?;
                if info.api != Api::OpenAiCompat {
                    None
                } else {
                    Some(Embedder {
                        client: compat_client(&e),
                        model: settings.embedding_model.trim().to_string(),
                        id: format!("{}:{}", e.provider, settings.embedding_model.trim()),
                    })
                }
            }
            _ => None,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                provider_id: conn.provider.to_ascii_lowercase(),
                backend,
                model: settings.model.trim().to_string(),
                label: info.label.to_string(),
                chat_error,
                embedder,
                no_temperature: AtomicBool::new(false),
                no_json_mode: AtomicBool::new(false),
            }),
        })
    }

    /// e.g. "Anthropic (Claude) · claude-sonnet-…".
    pub fn describe(&self) -> String {
        format!("{} · {}", self.inner.label, self.inner.model)
    }

    /// `provider:model` of the embeddings model, `None` when memory search is off.
    pub fn embedding_id(&self) -> Option<String> {
        self.inner.embedder.as_ref().map(|e| e.id.clone())
    }

    fn ready(&self) -> Result<()> {
        match &self.inner.chat_error {
            Some(err) => Err(anyhow!("{err}")),
            None => Ok(()),
        }
    }

    pub async fn chat(&self, system: &str, user: &str, json_mode: bool) -> Result<String> {
        self.chat_with_history(system, &[], user, json_mode, 0.3).await
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
        let messages = Self::build_messages(system, history, user);
        let reply = self
            .complete(messages, None, temperature, json_mode, None)
            .await?;
        Ok(reply.content.unwrap_or_default())
    }

    /// Multi-turn chat with tools. Runs a tool loop until the model returns text.
    pub async fn chat_with_tools(
        &self,
        system: &str,
        history: &[(String, String)],
        user: &str,
        tools: &[ChatCompletionTool],
        on_tool: impl FnMut(&str, &str) -> Result<String>,
        temperature: f32,
        max_rounds: usize,
        cancel_epoch: Option<u64>,
    ) -> Result<String> {
        let rich_history: Vec<LlmTurn> = history
            .iter()
            .map(|(role, text)| LlmTurn {
                role: role.clone(),
                text: text.clone(),
                images: Vec::new(),
            })
            .collect();
        let rich_user = LlmTurn {
            role: "user".into(),
            text: user.to_string(),
            images: Vec::new(),
        };
        self.chat_with_tools_rich(
            system,
            &rich_history,
            &rich_user,
            tools,
            on_tool,
            temperature,
            max_rounds,
            cancel_epoch,
        )
        .await
    }

    /// Multi-turn chat with tools and optional image inputs (vision).
    #[allow(clippy::too_many_arguments)]
    pub async fn chat_with_tools_rich(
        &self,
        system: &str,
        history: &[LlmTurn],
        user: &LlmTurn,
        tools: &[ChatCompletionTool],
        mut on_tool: impl FnMut(&str, &str) -> Result<String>,
        temperature: f32,
        max_rounds: usize,
        cancel_epoch: Option<u64>,
    ) -> Result<String> {
        if !user.images.is_empty() && self.is_known_text_only() {
            return Err(self.vision_unsupported_error(None));
        }

        // If the current model is known text-only, strip any old history images up front
        // so switching models mid-conversation does not break text follow-ups.
        let include_history_images = !self.is_known_text_only();
        let mut messages = Self::build_rich_messages(system, history, user, include_history_images);
        let history_had_images = include_history_images && history.iter().any(|t| !t.images.is_empty());

        for round in 0..max_rounds {
            if let Some(epoch) = cancel_epoch {
                crate::chat_cancel::check(epoch)?;
            }
            let assistant = match self
                .complete(messages.clone(), Some(tools), temperature, false, cancel_epoch)
                .await
            {
                Ok(msg) => msg,
                Err(err) if looks_like_vision_rejection(&err) => {
                    if !user.images.is_empty() {
                        return Err(self.vision_unsupported_error(Some(&err)));
                    }
                    if round == 0 && history_had_images {
                        messages = Self::build_rich_messages(system, history, user, false);
                        self.complete(messages.clone(), Some(tools), temperature, false, cancel_epoch)
                            .await?
                    } else {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            };

            let tool_calls = assistant.tool_calls.clone().unwrap_or_default();
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

        Err(anyhow!("tool loop exceeded {max_rounds} rounds"))
    }

    fn is_known_text_only(&self) -> bool {
        let p = self.inner.provider_id.as_str();
        let m = self.inner.model.to_ascii_lowercase();
        p == "deepseek"
            || m.starts_with("deepseek-chat")
            || m.starts_with("deepseek-reasoner")
            || m.starts_with("gpt-3.5")
            || m == "o1-mini"
            || m == "o3-mini"
    }

    fn vision_unsupported_error(&self, cause: Option<&anyhow::Error>) -> anyhow::Error {
        let detail = cause
            .map(|e| format!(" ({e:#})"))
            .unwrap_or_default();
        anyhow!(
            "{} cannot read images{detail}. Switch to a vision-capable model (such as GPT-4o / GPT-4.1, Claude, Gemini, or an Ollama vision model like llava or qwen2.5vl) in Settings → Models.",
            self.describe()
        )
    }

    async fn complete(
        &self,
        messages: Vec<ChatCompletionRequestMessage>,
        tools: Option<&[ChatCompletionTool]>,
        temperature: f32,
        json_mode: bool,
        cancel_epoch: Option<u64>,
    ) -> Result<ChatCompletionResponseMessage> {
        self.ready()?;
        let call = self.complete_adaptive(messages, tools, temperature, json_mode);
        if let Some(epoch) = cancel_epoch {
            tokio::select! {
                _ = crate::chat_cancel::wait_cancelled(epoch) => Err(anyhow!("cancelled")),
                result = call => result,
            }
        } else {
            call.await
        }
    }

    /// Send once; if the model rejects `temperature` or `response_format`, remember
    /// that and resend without it.
    async fn complete_adaptive(
        &self,
        messages: Vec<ChatCompletionRequestMessage>,
        tools: Option<&[ChatCompletionTool]>,
        temperature: f32,
        json_mode: bool,
    ) -> Result<ChatCompletionResponseMessage> {
        for _ in 0..3 {
            let send_temp = !self.inner.no_temperature.load(Ordering::Relaxed);
            let send_json = json_mode && !self.inner.no_json_mode.load(Ordering::Relaxed);
            let result = match &self.inner.backend {
                Backend::Compat(client) => {
                    self.compat_complete(client, &messages, tools, send_temp.then_some(temperature), send_json)
                        .await
                }
                Backend::Anthropic { http, base_url, api_key } => {
                    anthropic_complete(
                        http,
                        base_url,
                        api_key,
                        &self.inner.model,
                        &messages,
                        tools,
                        send_temp.then_some(temperature),
                        json_mode,
                    )
                    .await
                }
            };
            match result {
                Ok(msg) => return Ok(msg),
                Err(err) => {
                    let text = format!("{err:#}").to_ascii_lowercase();
                    let rejected = text.contains("unsupported")
                        || text.contains("not support")
                        || text.contains("invalid")
                        || text.contains("does not support")
                        || text.contains("only the default");
                    if send_temp && rejected && text.contains("temperature") {
                        self.inner.no_temperature.store(true, Ordering::Relaxed);
                        continue;
                    }
                    if send_json && rejected && (text.contains("response_format") || text.contains("json")) {
                        self.inner.no_json_mode.store(true, Ordering::Relaxed);
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        Err(anyhow!("{} kept rejecting the request parameters", self.describe()))
    }

    async fn compat_complete(
        &self,
        client: &Client<OpenAIConfig>,
        messages: &[ChatCompletionRequestMessage],
        tools: Option<&[ChatCompletionTool]>,
        temperature: Option<f32>,
        json_mode: bool,
    ) -> Result<ChatCompletionResponseMessage> {
        let mut messages = messages.to_vec();
        if json_mode {
            // Some servers require the word "JSON" in the prompt when json mode is on.
            messages.push(user_message("Respond with a single JSON object."));
        }
        let mut req = CreateChatCompletionRequestArgs::default();
        req.model(self.inner.model.clone());
        req.messages(messages);
        if let Some(t) = temperature {
            req.temperature(t);
        }
        if json_mode {
            req.response_format(ResponseFormat::JsonObject);
        }
        if let Some(tools) = tools.filter(|t| !t.is_empty()) {
            req.tools(tools.to_vec());
            req.tool_choice(ChatCompletionToolChoiceOption::Auto);
        }
        let request = req.build()?;
        let chat = client.chat();
        let response = with_retries("chat completion", is_transient_openai, || {
            chat.create(request.clone())
        })
        .await
        .map_err(|e| anyhow!("{} ({}): {e:#}", self.inner.label, self.inner.model))?;
        response
            .choices
            .first()
            .map(|c| c.message.clone())
            .ok_or_else(|| anyhow!("{} returned an empty response", self.describe()))
    }

    fn build_messages(
        system: &str,
        history: &[(String, String)],
        user: &str,
    ) -> Vec<ChatCompletionRequestMessage> {
        let rich_history: Vec<LlmTurn> = history
            .iter()
            .map(|(role, content)| LlmTurn {
                role: role.clone(),
                text: content.clone(),
                images: Vec::new(),
            })
            .collect();
        let rich_user = LlmTurn {
            role: "user".into(),
            text: user.to_string(),
            images: Vec::new(),
        };
        Self::build_rich_messages(system, &rich_history, &rich_user, false)
    }

    fn build_rich_messages(
        system: &str,
        history: &[LlmTurn],
        user: &LlmTurn,
        include_history_images: bool,
    ) -> Vec<ChatCompletionRequestMessage> {
        let mut messages = vec![ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: system.into(),
                name: None,
            },
        )];
        // Keep raw base64 images only on the most recent history image turn so payloads stay small.
        let last_img_idx = if include_history_images {
            history.iter().rposition(|t| !t.images.is_empty())
        } else {
            None
        };
        for (idx, turn) in history.iter().enumerate() {
            match turn.role.as_str() {
                "user" => {
                    let imgs: &[ImageInput] = if Some(idx) == last_img_idx {
                        &turn.images
                    } else {
                        &[]
                    };
                    messages.push(user_turn_message(&turn.text, imgs));
                }
                "assistant" | "piya" => messages.push(ChatCompletionRequestMessage::Assistant(
                    ChatCompletionRequestAssistantMessage {
                        content: Some(turn.text.clone().into()),
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
        messages.push(user_turn_message(&user.text, &user.images));
        messages
    }

    pub async fn chat_json(&self, system: &str, user: &str) -> Result<Value> {
        let raw = self.chat(system, user, true).await?;
        parse_json_reply(&raw).with_context(|| {
            format!(
                "parse JSON reply from {}: {}",
                self.describe(),
                raw.chars().take(200).collect::<String>()
            )
        })
    }

    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let Some(embedder) = &self.inner.embedder else {
            bail!("memory search is off — choose a memory (embeddings) model in Settings → Models");
        };
        // Embedding models cap input around 8k tokens; dense logs blow past that quickly.
        let truncated: Vec<String> = texts.iter().map(|t| truncate_for_embedding(t)).collect();
        let request = CreateEmbeddingRequestArgs::default()
            .model(embedder.model.clone())
            .input(truncated)
            .build()?;
        let embeddings = embedder.client.embeddings();
        let response = with_retries("embedding request", is_transient_openai, || {
            embeddings.create(request.clone())
        })
        .await
        .map_err(|e| anyhow!("embeddings ({}): {e:#}", embedder.id))?;
        Ok(response.data.into_iter().map(|d| d.embedding).collect())
    }
}

fn user_message(text: &str) -> ChatCompletionRequestMessage {
    user_turn_message(text, &[])
}

fn user_turn_message(text: &str, images: &[ImageInput]) -> ChatCompletionRequestMessage {
    if images.is_empty() {
        return ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
            content: text.to_string().into(),
            name: None,
        });
    }
    let mut parts: Vec<ChatCompletionRequestUserMessageContentPart> = Vec::new();
    let prompt_text = if text.trim().is_empty() {
        "Inspect the attached image(s) and respond.".to_string()
    } else {
        text.to_string()
    };
    parts.push(ChatCompletionRequestUserMessageContentPart::Text(
        ChatCompletionRequestMessageContentPartText { text: prompt_text },
    ));
    for img in images {
        parts.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
            ChatCompletionRequestMessageContentPartImage {
                image_url: ImageUrl {
                    url: format!("data:{};base64,{}", img.media_type, img.base64_data),
                    detail: None,
                },
            },
        ));
    }
    ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
        content: ChatCompletionRequestUserMessageContent::Array(parts),
        name: None,
    })
}

fn looks_like_vision_rejection(err: &anyhow::Error) -> bool {
    let lower = format!("{err:#}").to_ascii_lowercase();
    lower.contains("image_url")
        || lower.contains("does not support image")
        || lower.contains("cannot read image")
        || lower.contains("vision")
        || lower.contains("multimodal")
        || lower.contains("expected a string")
        || lower.contains("content must be a string")
        || (lower.contains("image") && (lower.contains("unsupported") || lower.contains("not support") || lower.contains("invalid")))
}

/// Parse a JSON object reply, tolerating ```json fences or prose around it
/// (providers without a JSON mode).
pub fn parse_json_reply(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }
    let start = trimmed.find('{').ok_or_else(|| anyhow!("no JSON object in reply"))?;
    let end = trimmed.rfind('}').ok_or_else(|| anyhow!("no JSON object in reply"))?;
    if end <= start {
        bail!("no JSON object in reply");
    }
    Ok(serde_json::from_str(&trimmed[start..=end])?)
}

// ---------------------------------------------------------------------------
// Anthropic Messages API
// ---------------------------------------------------------------------------

fn max_output_tokens() -> u32 {
    std::env::var("PIYA_MAX_OUTPUT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &u32| *v > 0)
        .unwrap_or(8192)
}

/// Text from an OpenAI-format `content` (string or array of text parts).
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// User content blocks for Anthropic Messages API (text + base64 images).
fn anthropic_user_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                Vec::new()
            } else {
                vec![json!({"type": "text", "text": s})]
            }
        }
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                match part.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                            if !t.trim().is_empty() {
                                out.push(json!({"type": "text", "text": t}));
                            }
                        }
                    }
                    Some("image_url") => {
                        if let Some(url) = part.pointer("/image_url/url").and_then(|u| u.as_str()) {
                            if let Some((meta, b64)) = url
                                .strip_prefix("data:")
                                .and_then(|rest| rest.split_once(";base64,"))
                            {
                                out.push(json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": meta,
                                        "data": b64,
                                    }
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Convert OpenAI-format messages + tools into an Anthropic Messages request body.
pub fn anthropic_request(
    model: &str,
    messages: &[ChatCompletionRequestMessage],
    tools: Option<&[ChatCompletionTool]>,
    temperature: Option<f32>,
    json_mode: bool,
    max_tokens: u32,
) -> Result<Value> {
    let mut system: Vec<String> = Vec::new();
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    let mut push = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        match out.last_mut() {
            Some((r, b)) if r == role => b.extend(blocks),
            _ => out.push((role.to_string(), blocks)),
        }
    };

    for msg in messages {
        let v = serde_json::to_value(msg)?;
        let role = v.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let text = content_text(v.get("content"));
        match role {
            "system" | "developer" => {
                if !text.trim().is_empty() {
                    system.push(text);
                }
            }
            "user" => push("user", anthropic_user_blocks(v.get("content"))),
            "assistant" => {
                let mut blocks = Vec::new();
                if !text.trim().is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                if let Some(calls) = v.get("tool_calls").and_then(|c| c.as_array()) {
                    for call in calls {
                        let args = call.pointer("/function/arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                        let input: Value = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.get("id").and_then(|i| i.as_str()).unwrap_or(""),
                            "name": call.pointer("/function/name").and_then(|n| n.as_str()).unwrap_or(""),
                            "input": if input.is_object() { input } else { json!({}) },
                        }));
                    }
                }
                push("assistant", blocks);
            }
            "tool" => push(
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": v.get("tool_call_id").and_then(|i| i.as_str()).unwrap_or(""),
                    "content": if text.is_empty() { "(empty)".to_string() } else { text },
                })],
            ),
            _ => {}
        }
    }

    // The conversation must start with a user turn.
    if out.first().map(|(r, _)| r.as_str()) != Some("user") {
        out.insert(0, ("user".into(), vec![json!({"type": "text", "text": "(conversation continues)"})]));
    }
    if json_mode {
        system.push("Respond with a single valid JSON object and nothing else — no prose, no code fences.".into());
    }

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": out
            .into_iter()
            .map(|(role, content)| json!({"role": role, "content": content}))
            .collect::<Vec<_>>(),
    });
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if let Some(t) = temperature {
        body["temperature"] = json!(t.clamp(0.0, 1.0));
    }
    if let Some(tools) = tools.filter(|t| !t.is_empty()) {
        body["tools"] = json!(tools
            .iter()
            .map(|t| {
                let schema = t
                    .function
                    .parameters
                    .clone()
                    .filter(|p| p.is_object())
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                json!({
                    "name": t.function.name,
                    "description": t.function.description.clone().unwrap_or_default(),
                    "input_schema": schema,
                })
            })
            .collect::<Vec<_>>());
    }
    Ok(body)
}

/// Convert an Anthropic Messages response into an OpenAI-format assistant message.
pub fn anthropic_response(body: &Value) -> Result<ChatCompletionResponseMessage> {
    let blocks = body
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("Anthropic response has no content"))?;
    let mut text = Vec::new();
    let mut calls = Vec::new();
    for b in blocks {
        match b.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    text.push(t.to_string());
                }
            }
            Some("tool_use") => calls.push(ChatCompletionMessageToolCall {
                id: b.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                r#type: ChatCompletionToolType::Function,
                function: FunctionCall {
                    name: b.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                    arguments: b.get("input").cloned().unwrap_or_else(|| json!({})).to_string(),
                },
            }),
            _ => {}
        }
    }
    let joined = text.join("\n");
    #[allow(deprecated)]
    Ok(ChatCompletionResponseMessage {
        content: if joined.is_empty() && !calls.is_empty() { None } else { Some(joined) },
        refusal: None,
        tool_calls: if calls.is_empty() { None } else { Some(calls) },
        role: Role::Assistant,
        function_call: None,
        audio: None,
    })
}

#[derive(Debug)]
struct HttpError {
    status: Option<reqwest::StatusCode>,
    message: String,
    transient: bool,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "{s}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn anthropic_complete(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[ChatCompletionRequestMessage],
    tools: Option<&[ChatCompletionTool]>,
    temperature: Option<f32>,
    json_mode: bool,
) -> Result<ChatCompletionResponseMessage> {
    let body = anthropic_request(model, messages, tools, temperature, json_mode, max_output_tokens())?;
    let url = format!("{base_url}/v1/messages");
    let resp: Value = with_retries("chat completion", |e: &HttpError| e.transient, || async {
        let r = http
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", providers::ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| HttpError {
                status: None,
                transient: e.is_timeout() || e.is_connect() || e.is_request(),
                message: e.to_string(),
            })?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(HttpError {
                status: Some(status),
                transient: status.as_u16() == 429 || status.as_u16() == 529 || status.is_server_error(),
                message: providers::api_error_message(&text),
            });
        }
        serde_json::from_str::<Value>(&text).map_err(|e| HttpError {
            status: Some(status),
            transient: false,
            message: format!("unreadable response: {e}"),
        })
    })
    .await
    .map_err(|e| anyhow!("Anthropic ({model}): {e:#}"))?;
    anthropic_response(&resp)
}

// ---------------------------------------------------------------------------
// HTTP plumbing shared by both backends
// ---------------------------------------------------------------------------

/// HTTP client with a request timeout so a stalled server cannot hang Piya forever.
/// Override with `PIYA_LLM_TIMEOUT_SEC` (default 180; local models may need more).
fn http_client() -> reqwest::Client {
    let secs = std::env::var("PIYA_LLM_TIMEOUT_SEC")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(180);
    providers::http(Duration::from_secs(secs))
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
fn is_transient_openai(err: &OpenAIError) -> bool {
    match err {
        OpenAIError::Reqwest(e) => e.is_timeout() || e.is_connect() || e.is_request(),
        _ => false,
    }
}

/// Run `op`, retrying transient errors with 1s, 2s backoff.
async fn with_retries<T, E, F, Fut>(what: &str, transient: impl Fn(&E) -> bool, mut op: F) -> Result<T>
where
    E: std::fmt::Display,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
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
            Err(err) if attempt < MAX_ATTEMPTS && transient(&err) => {
                let delay = Duration::from_secs(1 << (attempt - 1));
                eprintln!(
                    "piya: {what} failed ({err}); retry {attempt}/{} in {}s",
                    MAX_ATTEMPTS - 1,
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(err) => return Err(anyhow!("{what} failed: {err}")),
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

#[cfg(test)]
mod anthropic_tests {
    use super::*;
    use crate::chat_tools::schedule;

    #[test]
    fn converts_a_tool_conversation() {
        let mut msgs = LlmClient::build_messages("You are Piya.", &[("piya".into(), "Hi!".into())], "remind me in 5 min");
        msgs.push(ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
            content: None,
            name: None,
            tool_calls: Some(vec![ChatCompletionMessageToolCall {
                id: "call_1".into(),
                r#type: ChatCompletionToolType::Function,
                function: FunctionCall { name: "piya_schedule_create".into(), arguments: r#"{"kind":"reminder"}"#.into() },
            }]),
            refusal: None,
            audio: None,
            #[allow(deprecated)]
            function_call: None,
        }));
        msgs.push(ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
            content: ChatCompletionRequestToolMessageContent::Text("Saved ✔".into()),
            tool_call_id: "call_1".into(),
        }));
        let tools = schedule::openai_tools();
        let body = anthropic_request("claude-x", &msgs, Some(&tools), Some(0.2), false, 1024).unwrap();

        assert_eq!(body["system"], "You are Piya.");
        let m = body["messages"].as_array().unwrap();
        // history starting with an assistant turn gets a user turn in front
        assert_eq!(m[0]["role"], "user");
        assert_eq!(m[1]["role"], "assistant");
        assert_eq!(m[2]["role"], "user");
        assert_eq!(m[3]["content"][0]["type"], "tool_use");
        assert_eq!(m[3]["content"][0]["input"]["kind"], "reminder");
        assert_eq!(m[4]["role"], "user");
        assert_eq!(m[4]["content"][0]["type"], "tool_result");
        assert_eq!(m[4]["content"][0]["tool_use_id"], "call_1");
        assert_eq!(body["tools"][0]["name"], "piya_schedule_create");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        // roles strictly alternate
        for pair in m.windows(2) {
            assert_ne!(pair[0]["role"], pair[1]["role"]);
        }
    }

    #[test]
    fn parses_tool_use_response() {
        let resp = json!({"content": [
            {"type": "text", "text": "Setting that up."},
            {"type": "tool_use", "id": "toolu_1", "name": "piya_notify", "input": {"body": "hi"}}
        ], "stop_reason": "tool_use"});
        let msg = anthropic_response(&resp).unwrap();
        let calls = msg.tool_calls.unwrap();
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].function.name, "piya_notify");
        assert_eq!(serde_json::from_str::<Value>(&calls[0].function.arguments).unwrap()["body"], "hi");
        assert_eq!(msg.content.as_deref(), Some("Setting that up."));
    }

    #[test]
    fn json_replies_tolerate_fences() {
        assert_eq!(parse_json_reply("```json\n{\"a\":1}\n```").unwrap()["a"], 1);
        assert_eq!(parse_json_reply("Here you go: {\"a\":2} thanks").unwrap()["a"], 2);
        assert!(parse_json_reply("no json").is_err());
    }

    #[test]
    fn converts_image_turn_to_anthropic_vision_block() {
        let user = LlmTurn {
            role: "user".into(),
            text: "What error is shown?".into(),
            images: vec![ImageInput {
                media_type: "image/png".into(),
                base64_data: "iVBORw0KGgo=".into(),
            }],
        };
        let msgs = LlmClient::build_rich_messages("Sys", &[], &user, true);
        let body = anthropic_request("claude-sonnet-4-0", &msgs, None, None, false, 1024).unwrap();
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], "iVBORw0KGgo=");
    }
}
