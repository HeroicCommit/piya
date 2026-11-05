use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolType, FunctionObject};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::Client as HttpClient;
use serde_json::{json, Value};

use crate::mcp_config::{McpConfig, McpServerEntry, McpTransport};

const PROTOCOL_VERSION: &str = "2024-11-05";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Qualified tool name: `{server}__{tool}` (OpenAI-safe).
pub fn qualify_tool(server: &str, tool: &str) -> String {
    format!("{}__{}", sanitize_name(server), sanitize_name(tool))
}

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
struct McpTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Clone, Debug)]
struct RegisteredTool {
    llm_name: String,
    server_key: String,
    mcp_tool_name: String,
    description: String,
    input_schema: Value,
}

enum ActiveSession {
    Stdio {
        server_key: String,
        session: StdioMcpSession,
    },
    Http {
        server_key: String,
        session: HttpMcpSession,
    },
}

impl ActiveSession {
    fn server_key(&self) -> &str {
        match self {
            Self::Stdio { server_key, .. } | Self::Http { server_key, .. } => server_key,
        }
    }

    fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        match self {
            Self::Stdio { session, .. } => session.list_tools(),
            Self::Http { session, .. } => session.list_tools(),
        }
    }

    fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<String> {
        match self {
            Self::Stdio { session, .. } => session.call_tool(tool, arguments),
            Self::Http { session, .. } => session.call_tool(tool, arguments),
        }
    }
}

struct StdioMcpSession {
    server_name: String,
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl StdioMcpSession {
    fn spawn(server_name: &str, command: &str, args: &[String], env: &HashMap<String, String>) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn MCP server `{server_name}`"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP `{server_name}` missing stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP `{server_name}` missing stdout"))?;

        let mut session = Self {
            server_name: server_name.to_string(),
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        };

        session.initialize()?;
        Ok(session)
    }

    fn initialize(&mut self) -> Result<()> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "piya", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        let _ = result;
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_line(&msg)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_line(&msg)?;

        let deadline = std::time::Instant::now() + REQUEST_TIMEOUT;
        loop {
            if std::time::Instant::now() > deadline {
                return Err(anyhow!(
                    "MCP `{}` timed out waiting for `{method}`",
                    self.server_name
                ));
            }
            let line = self.read_line()?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value =
                serde_json::from_str(&line).context("parse MCP JSON-RPC line")?;
            if v.get("id").and_then(|i| i.as_u64()) != Some(id) {
                continue;
            }
            if let Some(err) = v.get("error") {
                return Err(anyhow!("MCP `{}`: {err}", self.server_name));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn write_line(&mut self, msg: &Value) -> Result<()> {
        let line = serde_json::to_string(msg)?;
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .context("read MCP stdout")?;
        // 0 bytes = EOF: the server exited or closed stdout. Without this check the
        // request loop treats the empty string as "skip" and spins at 100% CPU.
        if n == 0 {
            return Err(anyhow!(
                "MCP `{}` closed its output (server exited?)",
                self.server_name
            ));
        }
        Ok(line)
    }

    fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        parse_tool_list(self.request("tools/list", json!({}))?)
    }

    fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<String> {
        let result = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )?;
        format_tool_result(&result)
    }
}

impl Drop for StdioMcpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpMcpSession {
    server_name: String,
    url: String,
    client: HttpClient,
    headers: HeaderMap,
    session_id: Option<String>,
    next_id: u64,
}

impl HttpMcpSession {
    fn connect(server_name: &str, url: &str, headers: &HashMap<String, String>) -> Result<Self> {
        let mut header_map = HeaderMap::new();
        for (k, v) in headers {
            let name = HeaderName::from_bytes(k.as_bytes())
                .with_context(|| format!("invalid HTTP header name `{k}`"))?;
            let value = HeaderValue::from_str(v)
                .with_context(|| format!("invalid HTTP header value for `{k}`"))?;
            header_map.insert(name, value);
        }

        let client = HttpClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build HTTP MCP client")?;

        let mut session = Self {
            server_name: server_name.to_string(),
            url: url.to_string(),
            client,
            headers: header_map,
            session_id: None,
            next_id: 1,
        };

        session.initialize()?;
        Ok(session)
    }

    fn initialize(&mut self) -> Result<()> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "piya", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        let _ = result;
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.post_message(msg, None)?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let response = self.post_message(msg, Some(id))?;
        if let Some(err) = response.get("error") {
            return Err(anyhow!("MCP `{}`: {err}", self.server_name));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    fn post_message(&mut self, msg: Value, expect_id: Option<u64>) -> Result<Value> {
        let response_meta = crate::runtime::block_on({
            let client = self.client.clone();
            let url = self.url.clone();
            let headers = self.headers.clone();
            let session_id = self.session_id.clone();
            let server_name = self.server_name.clone();
            async move {
                let mut req = client
                    .post(&url)
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/json, text/event-stream");
                for (k, v) in &headers {
                    req = req.header(k, v);
                }
                if let Some(session_id) = &session_id {
                    req = req.header("Mcp-Session-Id", session_id);
                }

                let response = req
                    .json(&msg)
                    .send()
                    .await
                    .with_context(|| format!("HTTP MCP `{server_name}` POST failed"))?;

                let new_session = response
                    .headers()
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let content_type = response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let body = response
                    .text()
                    .await
                    .with_context(|| format!("read HTTP MCP `{server_name}` response"))?;
                Ok::<_, anyhow::Error>((new_session, content_type, body))
            }
        })?;

        let (new_session, content_type, body) = response_meta;
        if let Some(session_id) = new_session {
            self.session_id = Some(session_id);
        }

        if body.trim().is_empty() {
            return Ok(Value::Null);
        }

        parse_http_body(&body, &content_type, expect_id)
    }

    fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        parse_tool_list(self.request("tools/list", json!({}))?)
    }

    fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<String> {
        let result = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )?;
        format_tool_result(&result)
    }
}

fn parse_http_body(body: &str, content_type: &str, expect_id: Option<u64>) -> Result<Value> {
    if content_type.contains("text/event-stream") || body.starts_with("event:") {
        return parse_sse_response(body, expect_id);
    }

    let value: Value = serde_json::from_str(body).context("parse HTTP MCP JSON response")?;
    if let Some(id) = expect_id {
        if value.get("id").and_then(|v| v.as_u64()) != Some(id) {
            return Err(anyhow!("HTTP MCP response id mismatch"));
        }
    }
    Ok(value)
}

fn parse_sse_response(body: &str, expect_id: Option<u64>) -> Result<Value> {
    let mut last_match = None;
    for chunk in body.split("\n\n") {
        let mut data_line = None;
        for line in chunk.lines() {
            if let Some(rest) = line.strip_prefix("data: ") {
                data_line = Some(rest.trim());
            }
        }
        let Some(data) = data_line else { continue };
        if data.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(data).context("parse SSE data JSON")?;
        if let Some(id) = expect_id {
            if value.get("id").and_then(|v| v.as_u64()) == Some(id) {
                return Ok(value);
            }
            last_match = Some(value);
        } else {
            last_match = Some(value);
        }
    }

    last_match.ok_or_else(|| anyhow!("no JSON-RPC payload in SSE response"))
}

fn parse_tool_list(result: Value) -> Result<Vec<McpTool>> {
    let tools = result
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(tools
        .into_iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.to_string();
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or(json!({ "type": "object", "properties": {} }));
            Some(McpTool {
                name,
                description,
                input_schema,
            })
        })
        .collect())
}

/// Connected MCP servers and their tools, ready for LLM function calling.
pub struct McpToolHub {
    sessions: Vec<ActiveSession>,
    tools: Vec<RegisteredTool>,
    truncated_tool_count: usize,
    connect_errors: Vec<String>,
    llm_budget_applied: bool,
}

impl McpToolHub {
    pub fn connect(config: &McpConfig) -> Self {
        let mut hub = Self {
            sessions: Vec::new(),
            tools: Vec::new(),
            truncated_tool_count: 0,
            connect_errors: Vec::new(),
            llm_budget_applied: false,
        };

        for (server_name, entry) in config.enabled_servers() {
            hub.connect_server(server_name, entry);
        }

        hub
    }

    /// Keep the highest-priority MCP tools within OpenAI's function limit.
    pub fn select_for_llm(&mut self, max_tools: usize) {
        if self.llm_budget_applied {
            return;
        }
        self.llm_budget_applied = true;
        self.tools.sort_by(|a, b| {
            tool_priority(&b.server_key, &b.mcp_tool_name)
                .cmp(&tool_priority(&a.server_key, &a.mcp_tool_name))
                .then_with(|| a.llm_name.cmp(&b.llm_name))
        });
        if self.tools.len() > max_tools {
            self.truncated_tool_count = self.tools.len() - max_tools;
            self.tools.truncate(max_tools);
            self.connect_errors.push(format!(
                "{count} lower-priority MCP tools were omitted (OpenAI allows at most 128 tools). \
                 Talk, mail, and send tools are prioritized.",
                count = self.truncated_tool_count
            ));
        }
    }

    fn connect_server(&mut self, server_name: &str, entry: &McpServerEntry) {
        let transport = match entry.resolve_transport() {
            Ok(t) => t,
            Err(err) => {
                self.connect_errors.push(format!(
                    "`{server_name}` invalid config: {err:#}"
                ));
                return;
            }
        };

        let mut session = match transport {
            McpTransport::Stdio { command, args, env } => {
                match StdioMcpSession::spawn(server_name, &command, &args, &env) {
                    Ok(s) => ActiveSession::Stdio {
                        server_key: server_name.to_string(),
                        session: s,
                    },
                    Err(err) => {
                        self.connect_errors.push(format!(
                            "`{server_name}` failed to start: {err:#}"
                        ));
                        return;
                    }
                }
            }
            McpTransport::Http { url, headers } => {
                match HttpMcpSession::connect(server_name, &url, &headers) {
                    Ok(s) => ActiveSession::Http {
                        server_key: server_name.to_string(),
                        session: s,
                    },
                    Err(err) => {
                        self.connect_errors.push(format!(
                            "`{server_name}` HTTP connect failed: {err:#}"
                        ));
                        return;
                    }
                }
            }
        };

        match session.list_tools() {
            Ok(tools) => {
                for tool in tools {
                    let llm_name = qualify_tool(server_name, &tool.name);
                    let description = truncate_description(if tool.description.is_empty() {
                        format!("MCP tool `{}` on server `{server_name}`", tool.name)
                    } else {
                        format!("[{server_name}] {}", tool.description)
                    });
                    self.tools.push(RegisteredTool {
                        llm_name: llm_name.clone(),
                        server_key: server_name.to_string(),
                        mcp_tool_name: tool.name,
                        description,
                        input_schema: crate::mcp::schema::sanitize_openai_schema(&tool.input_schema),
                    });
                }
                self.sessions.push(session);
            }
            Err(err) => self.connect_errors.push(format!(
                "`{server_name}` connected but tools/list failed: {err:#}"
            )),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn connect_errors(&self) -> &[String] {
        &self.connect_errors
    }

    pub fn openai_tools(&self) -> Vec<ChatCompletionTool> {
        self.tools
            .iter()
            .map(|t| ChatCompletionTool {
                r#type: ChatCompletionToolType::Function,
                function: FunctionObject {
                    name: t.llm_name.clone(),
                    description: Some(t.description.clone()),
                    parameters: Some(t.input_schema.clone()),
                    strict: None,
                },
            })
            .collect()
    }

    pub fn system_summary(&self) -> String {
        let mut lines = vec![
            "You have MCP (Model Context Protocol) tools connected. When the user asks you to fetch data, run commands, check status, or interact with external services, you MUST call the appropriate MCP tool — do not say you lack the capability if a matching tool is available.".to_string(),
            "Tool names use the form `server__tool`. After tool results return, summarize clearly for the user.".to_string(),
        ];

        if self.tools.is_empty() {
            lines.push("No MCP tools are currently available to the model.".to_string());
        } else {
            let mut by_server: HashMap<&str, usize> = HashMap::new();
            for t in &self.tools {
                *by_server.entry(t.server_key.as_str()).or_default() += 1;
            }
            lines.push(format!(
                "{} MCP tools exposed to the model:",
                self.tools.len()
            ));
            for (server, count) in by_server {
                lines.push(format!("- `{server}`: {count} tools"));
            }
            lines.push(
                "Examples: `nextcloud__talk_send_message`, `nextcloud__talk_list_conversations`, `nextcloud__nc_mail_send_message`.".to_string(),
            );
            if self.truncated_tool_count > 0 {
                lines.push(format!(
                    "Note: {count} additional MCP tools were omitted due to the 128-tool API limit (Talk/mail/send tools are prioritized).",
                    count = self.truncated_tool_count
                ));
            }
        }

        for err in &self.connect_errors {
            lines.push(format!("Connection note: {err}"));
        }

        lines.join("\n")
    }

    pub fn call(&mut self, qualified_name: &str, arguments_json: &str) -> Result<String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.llm_name == qualified_name)
            .ok_or_else(|| anyhow!("unknown MCP tool `{qualified_name}`"))?;

        let args: Value = if arguments_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(arguments_json)
                .with_context(|| format!("parse tool arguments for `{qualified_name}`"))?
        };

        let session = self
            .sessions
            .iter_mut()
            .find(|s| s.server_key() == tool.server_key)
            .ok_or_else(|| anyhow!("no MCP session for server `{}`", tool.server_key))?;

        session.call_tool(&tool.mcp_tool_name, args)
    }
}

fn tool_priority(server: &str, tool: &str) -> i32 {
    let hay = format!("{server} {tool}").to_lowercase();
    let mut score = 0;
    if hay.contains("talk_send") {
        score += 500;
    }
    if hay.contains("talk_") {
        score += 400;
    }
    if hay.contains("send_message") || hay.contains("send") {
        score += 350;
    }
    if hay.contains("mail_") {
        score += 300;
    }
    if hay.contains("search") {
        score += 120;
    }
    if hay.contains("list_") {
        score += 80;
    }
    if hay.contains("get_") {
        score += 40;
    }
    score
}

fn truncate_description(text: String) -> String {
    const MAX: usize = 320;
    if text.chars().count() <= MAX {
        return text;
    }
    let trimmed: String = text.chars().take(MAX).collect();
    format!("{trimmed}…")
}

fn format_tool_result(result: &Value) -> Result<String> {
    if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
        let text = extract_content_text(result);
        return Err(anyhow!("tool error: {text}"));
    }
    Ok(extract_content_text(result))
}

fn extract_content_text(result: &Value) -> String {
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let parts: Vec<String> = content
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    block.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    Some(block.to_string())
                }
            })
            .collect();
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    result.to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::mcp_config::{McpConfig, McpServerEntry};

    #[test]
    fn qualify_and_sanitize() {
        let q = qualify_tool("my-server", "search_docs");
        assert_eq!(q, "my-server__search_docs");
    }

    #[test]
    fn parse_sse_payload() {
        let body = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\r\n\r\n";
        let value = parse_sse_response(body, Some(2)).unwrap();
        assert!(value.get("result").is_some());
    }

    #[test]
    #[ignore = "requires local Nextcloud MCP at http://127.0.0.1:8000/mcp"]
    fn connects_to_nextcloud_http_mcp() {
        let cfg = McpConfig {
            servers: HashMap::from([(
                "nextcloud".into(),
                McpServerEntry {
                    command: String::new(),
                    args: vec![],
                    env: HashMap::new(),
                    enabled: true,
                    transport: Some("http".into()),
                    url: Some("http://127.0.0.1:8000/mcp".into()),
                    headers: HashMap::new(),
                },
            )]),
        };
        let hub = McpToolHub::connect(&cfg);
        assert!(hub.connect_errors().is_empty(), "{:?}", hub.connect_errors());
        assert!(hub
            .openai_tools()
            .iter()
            .any(|t| t.function.name.contains("talk_send_message")));
    }

    /// A server that exits immediately must produce a prompt error, not a busy loop.
    #[test]
    fn dead_stdio_server_errors_quickly_instead_of_spinning() {
        let started = std::time::Instant::now();
        let result = StdioMcpSession::spawn(
            "dead",
            "sh",
            &["-c".to_string(), "exit 0".to_string()],
            &HashMap::new(),
        );
        assert!(result.is_err(), "initialize must fail when the server is gone");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}; request loop is spinning",
            started.elapsed()
        );
    }
}
