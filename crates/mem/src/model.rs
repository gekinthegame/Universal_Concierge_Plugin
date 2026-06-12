//! The model layer. One model by default; providers are pluggable behind the
//! `Model` trait (see "Models are pluggable" in the spec). The core ships the
//! Ollama-compatible HTTP client; more providers are additive.
//!
//! The chat REPL talks to a `Model`, never a concrete client — so prompts and
//! responses become memory records regardless of which provider answered.

use crate::config::ModelConfig;
use serde::{Deserialize, Serialize};

/// A tool the worker may call. `parameters` is a JSON Schema object describing
/// the arguments. This is the transport-neutral shape; providers translate it.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// One structured tool invocation the model emitted. Arguments arrive as JSON
/// (an object, or a JSON-encoded string from sloppier models) — the executor
/// coerces them. We never regex-parse prose: the action is data.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// One turn in a tool-using conversation. Serializes straight into Ollama's
/// `/api/chat` message shape; empty/absent fields are skipped so user and
/// system turns stay clean.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl ChatMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }
}

/// The model contract. One method the REPL calls, plus the model's name (for
/// provenance on the records it produces).
pub trait Model {
    fn complete(&self, prompt: &str) -> anyhow::Result<String>;
    fn name(&self) -> &str;

    /// Like `complete`, but invokes `on_token` with each fragment as it streams
    /// in, for live display. Returns the full accumulated text (identical to
    /// `complete`). The default is non-streaming: it emits the whole answer as a
    /// single fragment, so providers that can't stream still satisfy callers.
    fn complete_streaming(
        &self,
        prompt: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<String> {
        let answer = self.complete(prompt)?;
        on_token(&answer);
        Ok(answer)
    }

    /// One round of a tool-using conversation: given the messages so far and the
    /// tools the model may call, return the assistant's reply (its text plus any
    /// structured tool calls). The worker handoff drives this in a loop. The
    /// default declines, so providers without tool support fail loudly rather
    /// than silently dropping the worker's work.
    fn chat(&self, _messages: &[ChatMessage], _tools: &[ToolDef]) -> anyhow::Result<ChatMessage> {
        anyhow::bail!("this model provider does not support tool-calling chat")
    }
}

/// Build the configured model for a role. Dispatches on `provider`; today only
/// `"ollama"` is compiled in, but this is the seam where provider plugins slot.
pub fn build(cfg: &ModelConfig) -> anyhow::Result<Box<dyn Model>> {
    match cfg.provider.as_str() {
        "ollama" => Ok(Box::new(OllamaModel::from_config(cfg))),
        other => anyhow::bail!("unknown model provider: {other:?}"),
    }
}

/// Forward the contract through a boxed trait object so callers (the REPL) can
/// stay provider-agnostic.
impl Model for Box<dyn Model> {
    fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        (**self).complete(prompt)
    }
    fn name(&self) -> &str {
        (**self).name()
    }
    fn complete_streaming(
        &self,
        prompt: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<String> {
        (**self).complete_streaming(prompt, on_token)
    }
    fn chat(&self, messages: &[ChatMessage], tools: &[ToolDef]) -> anyhow::Result<ChatMessage> {
        (**self).chat(messages, tools)
    }
}

/// The always-compiled provider: an Ollama-compatible `/api/generate` client.
pub struct OllamaModel {
    host: String,
    name: String,
}

impl OllamaModel {
    pub fn from_config(cfg: &ModelConfig) -> Self {
        Self {
            host: cfg.host.clone(),
            name: cfg.name.clone(),
        }
    }

    /// The generation endpoint, consumed as a stream.
    fn generate_url(&self) -> String {
        format!("{}/api/generate", self.host.trim_end_matches('/'))
    }

    /// The chat endpoint, used for tool-calling turns.
    fn chat_url(&self) -> String {
        format!("{}/api/chat", self.host.trim_end_matches('/'))
    }

    /// The request body for one tool-using chat turn (pure). Non-streaming: a
    /// turn ends with either a full file written via a tool call or a final
    /// summary, and the worker loop already reports progress per tool call, so
    /// there is nothing to stream token-by-token here.
    fn chat_body(&self, messages: &[ChatMessage], tools: &[ToolDef]) -> serde_json::Value {
        let tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                })
            })
            .collect();
        serde_json::json!({
            "model": self.name,
            "messages": messages,
            "tools": tools,
            "stream": false,
        })
    }

    /// The request body for a streaming completion (pure). Streaming keeps the
    /// connection active chunk-by-chunk, so a long local generation isn't a
    /// single silent wait that the client's default timeout would cut off.
    fn request_body(&self, prompt: &str) -> serde_json::Value {
        serde_json::json!({
            "model": self.name,
            "prompt": prompt,
            "stream": true,
        })
    }
}

impl Model for OllamaModel {
    fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.complete_streaming(prompt, &mut |_| {})
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn complete_streaming(
        &self,
        prompt: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<String> {
        // The blocking client's default request timeout would cut a long
        // generation off mid-answer (a short "hello" fits inside it; a "build a
        // whole app" plan does not). Set a generous cap and stream the response,
        // surfacing each fragment to `on_token` as it arrives.
        let response = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1800))
            .build()
            .map_err(|e| anyhow::anyhow!("model client build failed: {e}"))?
            .post(self.generate_url())
            .json(&self.request_body(prompt))
            .send()
            .map_err(|e| anyhow::anyhow!("model request failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            anyhow::bail!("model request failed with {status}: {body}");
        }
        read_stream(std::io::BufReader::new(response), on_token)
    }

    fn chat(&self, messages: &[ChatMessage], tools: &[ToolDef]) -> anyhow::Result<ChatMessage> {
        let response = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1800))
            .build()
            .map_err(|e| anyhow::anyhow!("model client build failed: {e}"))?
            .post(self.chat_url())
            .json(&self.chat_body(messages, tools))
            .send()
            .map_err(|e| anyhow::anyhow!("model request failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            anyhow::bail!("model request failed with {status}: {body}");
        }
        let body = response
            .text()
            .map_err(|e| anyhow::anyhow!("model response read failed: {e}"))?;
        parse_chat_response(&body)
    }
}

/// Ollama's non-streaming `/api/chat` reply: one JSON object carrying the
/// assistant `message`, or an `error`.
#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    message: Option<ChatMessage>,
    #[serde(default)]
    error: Option<String>,
}

/// Parse one `/api/chat` response body into the assistant turn (pure, so it is
/// unit tested without a socket).
fn parse_chat_response(body: &str) -> anyhow::Result<ChatMessage> {
    let parsed: ChatResponse =
        serde_json::from_str(body).map_err(|e| anyhow::anyhow!("chat response parse: {e}"))?;
    if let Some(err) = parsed.error {
        anyhow::bail!("model chat error: {err}");
    }
    parsed
        .message
        .ok_or_else(|| anyhow::anyhow!("chat response had no message"))
}

/// One streamed chunk of Ollama's `/api/generate` response. Each line is one of
/// these: a `response` fragment plus `done`, or an `error` if generation fails.
#[derive(Deserialize)]
struct GenChunk {
    #[serde(default)]
    response: String,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
}

/// Parse one streamed chunk line (pure).
fn parse_chunk(line: &str) -> anyhow::Result<GenChunk> {
    serde_json::from_str(line).map_err(|e| anyhow::anyhow!("model chunk parse: {e}"))
}

/// Accumulate a streamed `/api/generate` response: one JSON object per line,
/// each carrying a `response` fragment, until `done` (or the stream ends).
/// `on_token` sees each fragment as it arrives (for live display); the returned
/// string is the clean accumulation, with no display-only characters. A
/// mid-stream `error` chunk aborts. Pure over any `BufRead`, so it is unit
/// tested without a socket.
fn read_stream<R: std::io::BufRead>(
    reader: R,
    on_token: &mut dyn FnMut(&str),
) -> anyhow::Result<String> {
    let mut out = String::new();
    for line in reader.lines() {
        let line = line.map_err(|e| anyhow::anyhow!("model stream read failed: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let chunk = parse_chunk(&line)?;
        if let Some(err) = chunk.error {
            anyhow::bail!("model stream error: {err}");
        }
        on_token(&chunk.response);
        out.push_str(&chunk.response);
        if chunk.done {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    fn ollama(host: &str, name: &str) -> OllamaModel {
        OllamaModel::from_config(&ModelConfig {
            host: host.to_string(),
            name: name.to_string(),
            ..Default::default()
        })
    }

    #[test]
    fn build_dispatches_ollama_and_rejects_unknown() {
        assert!(build(&ModelConfig::default()).is_ok());
        let bad = ModelConfig {
            provider: "nope".to_string(),
            ..Default::default()
        };
        assert!(build(&bad).is_err());
    }

    #[test]
    fn name_reports_the_configured_model() {
        assert_eq!(ollama("http://x", "llama3.2").name(), "llama3.2");
    }

    #[test]
    fn generate_url_trims_trailing_slash() {
        assert_eq!(
            ollama("http://localhost:11434/", "m").generate_url(),
            "http://localhost:11434/api/generate"
        );
    }

    #[test]
    fn request_body_streams_with_model_and_prompt() {
        let body = ollama("http://x", "llama3.2").request_body("hello");
        assert_eq!(body["model"], "llama3.2");
        assert_eq!(body["prompt"], "hello");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn parse_chunk_extracts_response_and_done() {
        let mid = parse_chunk(r#"{"response":"hi","done":false}"#).unwrap();
        assert_eq!(mid.response, "hi");
        assert!(!mid.done);
        let last = parse_chunk(r#"{"response":"","done":true}"#).unwrap();
        assert!(last.done);
    }

    #[test]
    fn parse_chunk_errors_on_garbage() {
        assert!(parse_chunk("not json").is_err());
    }

    #[test]
    fn read_stream_accumulates_until_done() {
        let body = "{\"response\":\"Hello\",\"done\":false}\n\
                    {\"response\":\", world\",\"done\":true}\n\
                    {\"response\":\"IGNORED\",\"done\":false}\n";
        let got = read_stream(std::io::Cursor::new(body.as_bytes()), &mut |_| {}).unwrap();
        assert_eq!(
            got, "Hello, world",
            "stops at done, ignores trailing chunks"
        );
    }

    #[test]
    fn read_stream_emits_each_fragment_live_to_the_callback() {
        let body = "{\"response\":\"Hel\",\"done\":false}\n\
                    {\"response\":\"lo\",\"done\":true}\n";
        let mut seen = Vec::new();
        let got = read_stream(std::io::Cursor::new(body.as_bytes()), &mut |tok| {
            seen.push(tok.to_string())
        })
        .unwrap();
        assert_eq!(seen, vec!["Hel", "lo"], "callback sees fragments in order");
        assert_eq!(got, "Hello", "return value is the clean accumulation");
    }

    #[test]
    fn read_stream_tolerates_blanks_and_eof_without_done() {
        let body = "{\"response\":\"a\",\"done\":false}\n\n{\"response\":\"b\",\"done\":false}\n";
        let got = read_stream(std::io::Cursor::new(body.as_bytes()), &mut |_| {}).unwrap();
        assert_eq!(got, "ab", "EOF without explicit done returns what arrived");
    }

    #[test]
    fn read_stream_surfaces_a_mid_stream_error() {
        let body = "{\"error\":\"model not found\"}\n";
        let err = read_stream(std::io::Cursor::new(body.as_bytes()), &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("model not found"), "got: {err}");
    }

    #[test]
    fn complete_posts_to_the_configured_model_and_accumulates_the_stream() {
        let (host, request) = serve_once(
            200,
            "{\"response\":\"do\",\"done\":false}\n{\"response\":\"ne\",\"done\":true}\n",
        );
        let model = ollama(&host, "local-model");

        assert_eq!(model.complete("hello").unwrap(), "done");

        let request = request.join().unwrap();
        assert!(request.starts_with("POST /api/generate HTTP/1.1"));
        assert!(request.contains(r#""model":"local-model""#));
        assert!(request.contains(r#""prompt":"hello""#));
        assert!(request.contains(r#""stream":true"#));
    }

    #[test]
    fn chat_body_carries_messages_and_wraps_tools_as_functions() {
        let body = ollama("http://x", "worker").chat_body(
            &[ChatMessage::new("user", "build it")],
            &[ToolDef {
                name: "write_file".into(),
                description: "write a file".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
        );
        assert_eq!(body["model"], "worker");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "build it");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "write_file");
    }

    #[test]
    fn parse_chat_response_extracts_content_and_tool_calls() {
        let msg = parse_chat_response(
            r#"{"message":{"role":"assistant","content":"ok","tool_calls":[{"function":{"name":"write_file","arguments":{"path":"a.txt","content":"hi"}}}]}}"#,
        )
        .unwrap();
        assert_eq!(msg.content, "ok");
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].function.name, "write_file");
        assert_eq!(msg.tool_calls[0].function.arguments["path"], "a.txt");
    }

    #[test]
    fn parse_chat_response_with_no_tool_calls_is_a_plain_reply() {
        let msg =
            parse_chat_response(r#"{"message":{"role":"assistant","content":"done"}}"#).unwrap();
        assert_eq!(msg.content, "done");
        assert!(msg.tool_calls.is_empty());
    }

    #[test]
    fn parse_chat_response_surfaces_an_error() {
        let err = parse_chat_response(r#"{"error":"model not found"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("model not found"), "got: {err}");
    }

    #[test]
    fn complete_errors_on_http_failure() {
        let (host, request) = serve_once(500, r#"{"error":"no model"}"#);
        let model = ollama(&host, "local-model");

        let err = model.complete("hello").unwrap_err().to_string();
        assert!(err.contains("500"));
        assert!(err.contains("no model"));
        request.join().unwrap();
    }

    fn serve_once(status: u16, body: &'static str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();

            let mut request = Vec::new();
            let mut buf = [0; 1024];
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request_is_complete(&request) {
                    break;
                }
            }

            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (host, handle)
    }

    fn request_is_complete(request: &[u8]) -> bool {
        let Ok(text) = std::str::from_utf8(request) else {
            return false;
        };
        let Some(headers_end) = text.find("\r\n\r\n").map(|i| i + 4) else {
            return false;
        };
        let content_len = text[..headers_end]
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find_map(|(key, value)| {
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        request.len() >= headers_end + content_len
    }
}
