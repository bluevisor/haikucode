//! OpenAI-compatible chat API: streaming completions with tool calls, plus
//! model listing. Uses ureq (blocking) + rustls so it cross-compiles to ARMv6
//! as a single static binary with no system TLS dependency.

use serde::{Deserialize, Serialize};

pub const MAX_TOOL_OUTPUT: usize = 16000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// Attached images as `data:image/...;base64,...` URIs. Sent to the API as
    /// OpenAI content parts (see `messages_payload`). Stripped when sessions
    /// are saved — the payloads would balloon the session file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// An assistant turn in its provider's own wire form (Anthropic content
    /// blocks with signed thinking, Responses output items with encrypted
    /// reasoning), replayed verbatim to that same api/model within this run so
    /// reasoning carries across tool turns. Never sent on chat/completions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<serde_json::Value>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Message {
        Message { role: "system".into(), content: content.into(), images: Vec::new(), tool_calls: None, tool_call_id: None, native: None }
    }
    pub fn user(content: impl Into<String>) -> Message {
        Message { role: "user".into(), content: content.into(), images: Vec::new(), tool_calls: None, tool_call_id: None, native: None }
    }
    pub fn user_with_images(content: impl Into<String>, images: Vec<String>) -> Message {
        Message { role: "user".into(), content: content.into(), images, tool_calls: None, tool_call_id: None, native: None }
    }
    pub fn tool(id: String, content: String) -> Message {
        Message { role: "tool".into(), content, images: Vec::new(), tool_calls: None, tool_call_id: Some(id), native: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
    /// Provider data that must come back with the call: Gemini signs each
    /// function call (`{"google": {"thought_signature": …}}`) and answers 400
    /// to a history whose calls lost it. Sent only to the model that made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// A tool call accumulated from streamed deltas.
#[derive(Clone, Debug, Default)]
pub struct AccumCall {
    pub id: String,
    pub name: String,
    pub args: String,
    /// The provider's `extra_content` for this call (see `ToolCall`).
    pub extra: Option<serde_json::Value>,
}

impl AccumCall {
    pub fn args_ok(&self) -> bool {
        let a = if self.args.trim().is_empty() { "{}" } else { &self.args };
        serde_json::from_str::<serde_json::Value>(a).is_ok()
    }
    pub fn into_tool_call(self, fallback_idx: usize) -> ToolCall {
        let id = if self.id.is_empty() { format!("call_{fallback_idx}") } else { self.id };
        let args = if self.args.trim().is_empty() { "{}".into() } else { self.args };
        ToolCall { id, kind: "function".into(), function: FunctionCall { name: self.name, arguments: args }, extra_content: self.extra }
    }
}

/// Token usage of one completion, normalized across protocols.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    /// Every input token, cached ones included.
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Input tokens served from the provider's prompt cache.
    pub cached_tokens: u32,
    /// Input tokens written to the cache (Anthropic's cache_creation).
    pub cache_write_tokens: u32,
}

impl Usage {
    pub fn spent(&self) -> crate::money::Spent {
        crate::money::Spent {
            prompt: self.prompt_tokens as u64,
            completion: self.completion_tokens as u64,
            cached: self.cached_tokens as u64,
            written: self.cache_write_tokens as u64,
        }
    }
}

/// What one streamed completion produced: the reply text, its tool calls,
/// token usage, and the turn in its provider's native form (see
/// `Message::native`).
pub type Streamed = (String, Vec<AccumCall>, Option<Usage>, Option<serde_json::Value>);

mod http;
mod models;
mod payload;
mod stream;
mod tools_spec;

pub use http::{agent_http, chat_plain, chat_resilient, takes_images, HttpStatus};
pub(crate) use http::{authed, post_json};
pub use models::{chat_models, context_window, exchange_rate, fetch_balance, list_models};
pub use payload::native_is_history_bound;
pub(crate) use payload::cache_key;
pub(crate) use stream::{for_each_sse, LoopGuard};
pub use tools_spec::{tools_spec_subagent, tools_spec_with};

pub fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let head: String = s.chars().take(limit.saturating_sub(40)).collect();
    let dropped = s.chars().count() - limit + 40;
    format!("{head}\n... [truncated {dropped} chars] ...")
}
